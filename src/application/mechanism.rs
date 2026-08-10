use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::{preflight_remote_capabilities, RemotePreflightError, RemotePreflightReport};
use crate::config::EnvironmentConfig;
use crate::domain::RollbackReference;
use crate::ports::{
    RemoteExecutionPort, RemoteExecutionResult, RemoteTaskResult, RemoteTransferResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentMechanismAction {
    Precheck,
    CaptureRollback,
    Apply,
    Activate,
    Verify,
    RollbackRestore,
}

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
    TargetCheck(RemoteExecutionError),
    CapabilityDiscovery(RemoteExecutionError),
    TargetUnreachable(String),
    MissingCapabilities { target: String, tasks: Vec<String> },
}

/// Application-owned semantic boundary for deployment mechanisms.
///
/// The application layer remains responsible for durable state transitions,
/// timeout interpretation, retries, recovery, idempotency, and rollback policy.
/// A mechanism only translates an approved semantic lifecycle action into the
/// configured remote capabilities required to perform that action.
#[async_trait]
pub trait DeploymentMechanismPort: Send + Sync {
    fn action_is_configured(
        &self,
        environment: &EnvironmentConfig,
        action: DeploymentMechanismAction,
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
        action: DeploymentMechanismAction,
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
}

#[async_trait]
impl<R> DeploymentMechanismPort for JarSystemdMechanism<R>
where
    R: RemoteExecutionPort + ?Sized,
{
    fn action_is_configured(
        &self,
        environment: &EnvironmentConfig,
        action: DeploymentMechanismAction,
    ) -> bool {
        match action {
            DeploymentMechanismAction::Precheck => environment.tasks.precheck.is_some(),
            DeploymentMechanismAction::RollbackRestore => environment.tasks.rollback.is_some(),
            DeploymentMechanismAction::CaptureRollback
            | DeploymentMechanismAction::Apply
            | DeploymentMechanismAction::Activate
            | DeploymentMechanismAction::Verify => true,
        }
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
        action: DeploymentMechanismAction,
    ) -> RemoteExecutionResult<MechanismTaskExecution> {
        let (task, parameters) = match action {
            DeploymentMechanismAction::Precheck => (
                environment
                    .tasks
                    .precheck
                    .as_deref()
                    .expect("precheck action must be configured before execution"),
                BTreeMap::new(),
            ),
            DeploymentMechanismAction::CaptureRollback => (
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
            DeploymentMechanismAction::Apply => (
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
            DeploymentMechanismAction::Activate => {
                (environment.tasks.restart.as_str(), BTreeMap::new())
            }
            DeploymentMechanismAction::Verify => {
                (environment.tasks.health_check.as_str(), BTreeMap::new())
            }
            DeploymentMechanismAction::RollbackRestore => (
                environment
                    .tasks
                    .rollback
                    .as_deref()
                    .expect("rollback action must be configured before execution"),
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
        };
        self.run_task(&environment.target, task, parameters).await
    }

    async fn preflight_rollback(
        &self,
        reference: &RollbackReference,
    ) -> Result<RemotePreflightReport, RollbackPreflightError> {
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
            reference.rollback_task().to_owned(),
            reference.restart_task().to_owned(),
            reference.health_check_task().to_owned(),
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
        let (task, parameters) = match action {
            RollbackMechanismAction::Restore => (
                reference.rollback_task(),
                BTreeMap::from([
                    (
                        "backup_path".to_owned(),
                        Value::String(reference.backup_path().to_owned()),
                    ),
                    (
                        "install_path".to_owned(),
                        Value::String(reference.install_path().to_owned()),
                    ),
                ]),
            ),
            RollbackMechanismAction::Activate => (reference.restart_task(), BTreeMap::new()),
            RollbackMechanismAction::Verify => (reference.health_check_task(), BTreeMap::new()),
        };
        self.run_task(reference.target(), task, parameters).await
    }
}
