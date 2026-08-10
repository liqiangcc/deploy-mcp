use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::{preflight_remote_capabilities, RemotePreflightError, RemotePreflightReport};
use crate::config::EnvironmentConfig;
use crate::domain::{
    Deployment, DeploymentLifecycleOperation, DeploymentMechanismKind, JarSystemdRollbackSnapshot,
    MechanismContractFingerprint, RollbackError, RollbackReference,
};
use crate::ports::{
    RemoteExecutionError, RemoteExecutionPort, RemoteExecutionResult, RemoteTaskResult,
    RemoteTransferResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollbackMechanismAction {
    Restore,
    Activate,
    Verify,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MechanismTaskExecution {
    pub task: String,
    pub result: RemoteTaskResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackPreflightError {
    MechanismMismatch,
    TargetCheck(RemoteExecutionError),
    CapabilityDiscovery(RemoteExecutionError),
    TargetUnreachable(String),
    MissingCapabilities { target: String, tasks: Vec<String> },
}

/// Application-owned semantic boundary for deployment mechanisms.
///
/// The application layer remains responsible for durable state transitions,
/// timeout interpretation, retries, recovery, idempotency, and rollback policy.
/// A mechanism only translates generic lifecycle intent into the configured
/// remote capabilities required to perform that operation.
#[async_trait]
pub trait DeploymentMechanismPort: Send + Sync {
    fn kind(&self) -> DeploymentMechanismKind;

    fn precheck_is_configured(&self, environment: &EnvironmentConfig) -> bool;

    fn rollback_is_configured(&self, environment: &EnvironmentConfig) -> bool;

    fn build_rollback_reference(
        &self,
        deployment: &Deployment,
        environment: &EnvironmentConfig,
    ) -> Result<RollbackReference, RollbackError>;

    fn rollback_contract_matches(
        &self,
        environment: &EnvironmentConfig,
        reference: &RollbackReference,
    ) -> bool;

    async fn preflight(
        &self,
        environment: &EnvironmentConfig,
    ) -> Result<RemotePreflightReport, RemotePreflightError>;

    async fn prepare(
        &self,
        environment: &EnvironmentConfig,
        local_path: &str,
    ) -> RemoteExecutionResult<RemoteTransferResult>;

    async fn execute(
        &self,
        environment: &EnvironmentConfig,
        operation: DeploymentLifecycleOperation,
    ) -> RemoteExecutionResult<MechanismTaskExecution>;

    async fn execute_current_rollback(
        &self,
        environment: &EnvironmentConfig,
        action: RollbackMechanismAction,
    ) -> RemoteExecutionResult<MechanismTaskExecution>;

    async fn preflight_rollback(
        &self,
        reference: &RollbackReference,
    ) -> Result<RemotePreflightReport, RollbackPreflightError>;

    async fn execute_rollback(
        &self,
        reference: &RollbackReference,
        action: RollbackMechanismAction,
    ) -> RemoteExecutionResult<MechanismTaskExecution>;
}

/// v0.1-compatible JAR/systemd mechanism backed exclusively by
/// `RemoteExecutionPort` capabilities.
pub struct JarSystemdMechanism<R>
where
    R: RemoteExecutionPort + ?Sized,
{
    remote: Arc<R>,
}

impl<R> JarSystemdMechanism<R>
where
    R: RemoteExecutionPort + ?Sized,
{
    pub fn new(remote: Arc<R>) -> Self {
        Self { remote }
    }

    async fn run_task(
        &self,
        target: &str,
        task: &str,
        parameters: BTreeMap<String, Value>,
    ) -> RemoteExecutionResult<MechanismTaskExecution> {
        let result = self.remote.run_task(target, task, parameters).await?;
        Ok(MechanismTaskExecution {
            task: task.to_owned(),
            result,
        })
    }

    fn rollback_snapshot(
        &self,
        environment: &EnvironmentConfig,
    ) -> Result<JarSystemdRollbackSnapshot, RollbackError> {
        let rollback_task = environment
            .tasks
            .rollback
            .as_deref()
            .ok_or(RollbackError::EmptyCapability("rollback_task"))?;
        JarSystemdRollbackSnapshot::new(
            environment.backup_path.clone(),
            environment.install_path.clone(),
            rollback_task,
            environment.tasks.restart.clone(),
            environment.tasks.health_check.clone(),
        )
    }

    fn invalid_operation(operation: DeploymentLifecycleOperation) -> RemoteExecutionError {
        RemoteExecutionError::InvalidResponse {
            tool: "deployment_mechanism".to_owned(),
            message: format!("lifecycle operation {operation:?} is not executable as a named task"),
        }
    }
}

#[async_trait]
impl<R> DeploymentMechanismPort for JarSystemdMechanism<R>
where
    R: RemoteExecutionPort + ?Sized,
{
    fn kind(&self) -> DeploymentMechanismKind {
        DeploymentMechanismKind::JarSystemd
    }

    fn precheck_is_configured(&self, environment: &EnvironmentConfig) -> bool {
        environment.tasks.precheck.is_some()
    }

    fn rollback_is_configured(&self, environment: &EnvironmentConfig) -> bool {
        environment.tasks.rollback.is_some()
    }

    fn build_rollback_reference(
        &self,
        deployment: &Deployment,
        environment: &EnvironmentConfig,
    ) -> Result<RollbackReference, RollbackError> {
        let snapshot = self.rollback_snapshot(environment)?;
        let fingerprint = MechanismContractFingerprint::jar_systemd(&environment.target, &snapshot);
        RollbackReference::new_with_snapshot(
            deployment.id().clone(),
            deployment.application().clone(),
            deployment.environment().clone(),
            self.kind(),
            environment.target.clone(),
            fingerprint,
            crate::domain::RollbackMechanismSnapshot::JarSystemd(snapshot),
        )
    }

    fn rollback_contract_matches(
        &self,
        environment: &EnvironmentConfig,
        reference: &RollbackReference,
    ) -> bool {
        if environment.mechanism.kind != self.kind() || reference.mechanism_kind() != self.kind() {
            return false;
        }
        let Ok(snapshot) = self.rollback_snapshot(environment) else {
            return false;
        };
        if environment.target != reference.target() {
            return false;
        }
        let fingerprint = MechanismContractFingerprint::jar_systemd(&environment.target, &snapshot);
        &fingerprint == reference.contract_fingerprint()
    }

    async fn preflight(
        &self,
        environment: &EnvironmentConfig,
    ) -> Result<RemotePreflightReport, RemotePreflightError> {
        preflight_remote_capabilities(self.remote.as_ref(), environment).await
    }

    async fn prepare(
        &self,
        environment: &EnvironmentConfig,
        local_path: &str,
    ) -> RemoteExecutionResult<RemoteTransferResult> {
        self.remote
            .upload_file(
                &environment.target,
                local_path,
                &environment.staging_path,
                true,
            )
            .await
    }

    async fn execute(
        &self,
        environment: &EnvironmentConfig,
        operation: DeploymentLifecycleOperation,
    ) -> RemoteExecutionResult<MechanismTaskExecution> {
        let (task, parameters) = match operation {
            DeploymentLifecycleOperation::Precheck => (
                environment
                    .tasks
                    .precheck
                    .as_deref()
                    .ok_or_else(|| Self::invalid_operation(operation))?,
                BTreeMap::new(),
            ),
            DeploymentLifecycleOperation::CaptureRollback => (
                environment.tasks.backup.as_str(),
                BTreeMap::from([
                    (
                        "install_path".to_owned(),
                        Value::String(environment.install_path.clone()),
                    ),
                    (
                        "backup_path".to_owned(),
                        Value::String(environment.backup_path.clone()),
                    ),
                ]),
            ),
            DeploymentLifecycleOperation::Apply => (
                environment.tasks.install.as_str(),
                BTreeMap::from([
                    (
                        "staging_path".to_owned(),
                        Value::String(environment.staging_path.clone()),
                    ),
                    (
                        "install_path".to_owned(),
                        Value::String(environment.install_path.clone()),
                    ),
                ]),
            ),
            DeploymentLifecycleOperation::Activate => {
                (environment.tasks.restart.as_str(), BTreeMap::new())
            }
            DeploymentLifecycleOperation::Verify => {
                (environment.tasks.health_check.as_str(), BTreeMap::new())
            }
            DeploymentLifecycleOperation::Validate | DeploymentLifecycleOperation::Prepare => {
                return Err(Self::invalid_operation(operation));
            }
        };
        self.run_task(&environment.target, task, parameters).await
    }

    async fn execute_current_rollback(
        &self,
        environment: &EnvironmentConfig,
        action: RollbackMechanismAction,
    ) -> RemoteExecutionResult<MechanismTaskExecution> {
        let (task, parameters) = match action {
            RollbackMechanismAction::Restore => (
                environment.tasks.rollback.as_deref().ok_or_else(|| {
                    RemoteExecutionError::InvalidResponse {
                        tool: "deployment_mechanism".to_owned(),
                        message: "rollback action is not configured".to_owned(),
                    }
                })?,
                BTreeMap::from([
                    (
                        "backup_path".to_owned(),
                        Value::String(environment.backup_path.clone()),
                    ),
                    (
                        "install_path".to_owned(),
                        Value::String(environment.install_path.clone()),
                    ),
                ]),
            ),
            RollbackMechanismAction::Activate => {
                (environment.tasks.restart.as_str(), BTreeMap::new())
            }
            RollbackMechanismAction::Verify => {
                (environment.tasks.health_check.as_str(), BTreeMap::new())
            }
        };
        self.run_task(&environment.target, task, parameters).await
    }

    async fn preflight_rollback(
        &self,
        reference: &RollbackReference,
    ) -> Result<RemotePreflightReport, RollbackPreflightError> {
        if reference.mechanism_kind() != self.kind() {
            return Err(RollbackPreflightError::MechanismMismatch);
        }
        let Some(snapshot) = reference.jar_systemd_snapshot() else {
            return Err(RollbackPreflightError::MechanismMismatch);
        };

        let check = self
            .remote
            .check_target(reference.target())
            .await
            .map_err(RollbackPreflightError::TargetCheck)?;
        if !check.reachable {
            return Err(RollbackPreflightError::TargetUnreachable(
                reference.target().to_owned(),
            ));
        }

        let available_tasks = self
            .remote
            .list_tasks(reference.target())
            .await
            .map_err(RollbackPreflightError::CapabilityDiscovery)?;
        let required = BTreeSet::from([
            snapshot.rollback_task().to_owned(),
            snapshot.restart_task().to_owned(),
            snapshot.health_check_task().to_owned(),
        ]);
        let missing = required
            .difference(&available_tasks)
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(RollbackPreflightError::MissingCapabilities {
                target: reference.target().to_owned(),
                tasks: missing,
            });
        }

        Ok(RemotePreflightReport {
            target: reference.target().to_owned(),
            remote_identity: check.remote_identity,
            available_tasks,
        })
    }

    async fn execute_rollback(
        &self,
        reference: &RollbackReference,
        action: RollbackMechanismAction,
    ) -> RemoteExecutionResult<MechanismTaskExecution> {
        if reference.mechanism_kind() != self.kind() {
            return Err(RemoteExecutionError::InvalidResponse {
                tool: "deployment_mechanism".to_owned(),
                message: "rollback reference mechanism does not match jar_systemd".to_owned(),
            });
        }
        let snapshot = reference.jar_systemd_snapshot().ok_or_else(|| {
            RemoteExecutionError::InvalidResponse {
                tool: "deployment_mechanism".to_owned(),
                message: "rollback reference does not contain jar_systemd snapshot".to_owned(),
            }
        })?;
        let (task, parameters) = match action {
            RollbackMechanismAction::Restore => (
                snapshot.rollback_task(),
                BTreeMap::from([
                    (
                        "backup_path".to_owned(),
                        Value::String(snapshot.backup_path().to_owned()),
                    ),
                    (
                        "install_path".to_owned(),
                        Value::String(snapshot.install_path().to_owned()),
                    ),
                ]),
            ),
            RollbackMechanismAction::Activate => (snapshot.restart_task(), BTreeMap::new()),
            RollbackMechanismAction::Verify => (snapshot.health_check_task(), BTreeMap::new()),
        };
        self.run_task(reference.target(), task, parameters).await
    }
}
