use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::{preflight_remote_capabilities, RemotePreflightError, RemotePreflightReport};
use crate::config::{DockerComposeTaskReferences, EnvironmentConfig};
use crate::domain::{
    Deployment, DeploymentLifecycleOperation, DeploymentMechanismKind,
    DockerComposeRollbackSnapshot, JarSystemdRollbackSnapshot, MechanismContractFingerprint,
    ReleaseIdentity, RollbackError, RollbackMechanismSnapshot, RollbackReference,
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
pub enum MechanismPrepareExecution {
    FileTransfer(RemoteTransferResult),
    Task(MechanismTaskExecution),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MechanismRollbackCapture {
    pub execution: MechanismTaskExecution,
    pub reference: Option<RollbackReference>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackPreflightError {
    MechanismMismatch,
    TargetCheck(RemoteExecutionError),
    CapabilityDiscovery(RemoteExecutionError),
    TargetUnreachable(String),
    MissingCapabilities { target: String, tasks: Vec<String> },
}

#[async_trait]
pub trait DeploymentMechanismPort: Send + Sync {
    fn kind(&self) -> DeploymentMechanismKind;
    fn precheck_is_configured(&self, environment: &EnvironmentConfig) -> bool;
    fn rollback_is_configured(&self, environment: &EnvironmentConfig) -> bool;
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
        release: &ReleaseIdentity,
        local_path: Option<&str>,
    ) -> RemoteExecutionResult<MechanismPrepareExecution>;
    async fn capture_rollback(
        &self,
        deployment: &Deployment,
        environment: &EnvironmentConfig,
    ) -> RemoteExecutionResult<MechanismRollbackCapture>;
    async fn execute(
        &self,
        environment: &EnvironmentConfig,
        release: &ReleaseIdentity,
        operation: DeploymentLifecycleOperation,
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

pub struct JarSystemdMechanism<R: RemoteExecutionPort + ?Sized> {
    remote: Arc<R>,
}

impl<R: RemoteExecutionPort + ?Sized> JarSystemdMechanism<R> {
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

    fn build_reference(
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
            RollbackMechanismSnapshot::JarSystemd(snapshot),
        )
    }

    fn invalid_operation(operation: DeploymentLifecycleOperation) -> RemoteExecutionError {
        invalid_mechanism_response(format!(
            "lifecycle operation {operation:?} is not executable by jar_systemd"
        ))
    }
}

#[async_trait]
impl<R: RemoteExecutionPort + ?Sized> DeploymentMechanismPort for JarSystemdMechanism<R> {
    fn kind(&self) -> DeploymentMechanismKind {
        DeploymentMechanismKind::JarSystemd
    }

    fn precheck_is_configured(&self, environment: &EnvironmentConfig) -> bool {
        environment.tasks.precheck.is_some()
    }

    fn rollback_is_configured(&self, environment: &EnvironmentConfig) -> bool {
        environment.tasks.rollback.is_some()
    }

    fn rollback_contract_matches(
        &self,
        environment: &EnvironmentConfig,
        reference: &RollbackReference,
    ) -> bool {
        if environment.mechanism.kind != self.kind()
            || reference.mechanism_kind() != self.kind()
            || environment.target != reference.target()
        {
            return false;
        }
        let Ok(snapshot) = self.rollback_snapshot(environment) else {
            return false;
        };
        MechanismContractFingerprint::jar_systemd(&environment.target, &snapshot)
            == *reference.contract_fingerprint()
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
        release: &ReleaseIdentity,
        local_path: Option<&str>,
    ) -> RemoteExecutionResult<MechanismPrepareExecution> {
        if release.local_file().is_none() {
            return Err(invalid_mechanism_response(
                "jar_systemd requires a local-file release",
            ));
        }
        let local_path = local_path.ok_or_else(|| {
            invalid_mechanism_response("jar_systemd requires a local artifact path")
        })?;
        let result = self
            .remote
            .upload_file(
                &environment.target,
                local_path,
                &environment.staging_path,
                true,
            )
            .await?;
        Ok(MechanismPrepareExecution::FileTransfer(result))
    }

    async fn capture_rollback(
        &self,
        deployment: &Deployment,
        environment: &EnvironmentConfig,
    ) -> RemoteExecutionResult<MechanismRollbackCapture> {
        let execution = self
            .run_task(
                &environment.target,
                &environment.tasks.backup,
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
            )
            .await?;
        let reference = if execution.result.success && self.rollback_is_configured(environment) {
            Some(
                self.build_reference(deployment, environment)
                    .map_err(rollback_error_response)?,
            )
        } else {
            None
        };
        Ok(MechanismRollbackCapture {
            execution,
            reference,
        })
    }

    async fn execute(
        &self,
        environment: &EnvironmentConfig,
        release: &ReleaseIdentity,
        operation: DeploymentLifecycleOperation,
    ) -> RemoteExecutionResult<MechanismTaskExecution> {
        if release.local_file().is_none() {
            return Err(invalid_mechanism_response(
                "jar_systemd requires a local-file release",
            ));
        }
        let (task, parameters) = match operation {
            DeploymentLifecycleOperation::Precheck => (
                environment
                    .tasks
                    .precheck
                    .as_deref()
                    .ok_or_else(|| Self::invalid_operation(operation))?,
                BTreeMap::new(),
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
            DeploymentLifecycleOperation::Validate
            | DeploymentLifecycleOperation::Prepare
            | DeploymentLifecycleOperation::CaptureRollback => {
                return Err(Self::invalid_operation(operation));
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
        preflight_rollback_tasks(
            self.remote.as_ref(),
            reference.target(),
            BTreeSet::from([
                snapshot.rollback_task().to_owned(),
                snapshot.restart_task().to_owned(),
                snapshot.health_check_task().to_owned(),
            ]),
        )
        .await
    }

    async fn execute_rollback(
        &self,
        reference: &RollbackReference,
        action: RollbackMechanismAction,
    ) -> RemoteExecutionResult<MechanismTaskExecution> {
        if reference.mechanism_kind() != self.kind() {
            return Err(invalid_mechanism_response(
                "rollback reference mechanism does not match jar_systemd",
            ));
        }
        let snapshot = reference.jar_systemd_snapshot().ok_or_else(|| {
            invalid_mechanism_response("rollback reference does not contain jar_systemd snapshot")
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

pub struct DockerComposeMechanism<R: RemoteExecutionPort + ?Sized> {
    remote: Arc<R>,
}

impl<R: RemoteExecutionPort + ?Sized> DockerComposeMechanism<R> {
    pub fn new(remote: Arc<R>) -> Self {
        Self { remote }
    }

    fn config<'a>(
        &self,
        environment: &'a EnvironmentConfig,
    ) -> RemoteExecutionResult<(&'a str, &'a str, &'a str, &'a DockerComposeTaskReferences)> {
        if environment.mechanism.kind != DeploymentMechanismKind::DockerCompose {
            return Err(invalid_mechanism_response(
                "environment is not configured for docker_compose",
            ));
        }
        Ok((
            environment
                .mechanism
                .image_repository
                .as_deref()
                .ok_or_else(|| {
                    invalid_mechanism_response("docker image repository is not configured")
                })?,
            environment
                .mechanism
                .compose_project
                .as_deref()
                .ok_or_else(|| {
                    invalid_mechanism_response("docker compose project is not configured")
                })?,
            environment
                .mechanism
                .service
                .as_deref()
                .ok_or_else(|| invalid_mechanism_response("docker service is not configured"))?,
            environment
                .mechanism
                .tasks
                .as_ref()
                .ok_or_else(|| invalid_mechanism_response("docker task set is not configured"))?,
        ))
    }

    fn image<'a>(
        &self,
        release: &'a ReleaseIdentity,
    ) -> RemoteExecutionResult<&'a crate::domain::ContainerImageReleaseIdentity> {
        release.container_image().ok_or_else(|| {
            invalid_mechanism_response("docker_compose requires a container_image release")
        })
    }

    fn candidate_parameters(
        &self,
        environment: &EnvironmentConfig,
        release: &ReleaseIdentity,
    ) -> RemoteExecutionResult<BTreeMap<String, Value>> {
        let (repository, project, service, _) = self.config(environment)?;
        let image = self.image(release)?;
        if image.repository() != repository {
            return Err(invalid_mechanism_response(
                "container release repository does not match trusted configuration",
            ));
        }
        Ok(BTreeMap::from([
            (
                "image_repository".to_owned(),
                Value::String(repository.to_owned()),
            ),
            (
                "digest".to_owned(),
                Value::String(image.digest().to_owned()),
            ),
            (
                "compose_project".to_owned(),
                Value::String(project.to_owned()),
            ),
            ("service".to_owned(), Value::String(service.to_owned())),
        ]))
    }

    fn service_parameters(
        &self,
        environment: &EnvironmentConfig,
    ) -> RemoteExecutionResult<BTreeMap<String, Value>> {
        let (_, project, service, _) = self.config(environment)?;
        Ok(BTreeMap::from([
            (
                "compose_project".to_owned(),
                Value::String(project.to_owned()),
            ),
            ("service".to_owned(), Value::String(service.to_owned())),
        ]))
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

    fn fingerprint(
        &self,
        environment: &EnvironmentConfig,
    ) -> RemoteExecutionResult<MechanismContractFingerprint> {
        let (repository, project, service, tasks) = self.config(environment)?;
        Ok(MechanismContractFingerprint::docker_compose(
            &environment.target,
            repository,
            project,
            service,
            tasks.precheck.as_deref(),
            &tasks.prepare,
            &tasks.capture_rollback,
            tasks.apply.as_str(),
            tasks.activate.as_str(),
            tasks.health_check.as_str(),
            &tasks.rollback,
        ))
    }

    fn parse_previous_digest(stdout: &str) -> RemoteExecutionResult<String> {
        let digest = stdout.trim();
        let Some(hex) = digest.strip_prefix("sha256:") else {
            return Err(invalid_mechanism_response(
                "docker capture_rollback did not return an immutable sha256 digest",
            ));
        };
        if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid_mechanism_response(
                "docker capture_rollback returned an invalid sha256 digest",
            ));
        }
        Ok(format!("sha256:{}", hex.to_ascii_lowercase()))
    }
}

#[async_trait]
impl<R: RemoteExecutionPort + ?Sized> DeploymentMechanismPort for DockerComposeMechanism<R> {
    fn kind(&self) -> DeploymentMechanismKind {
        DeploymentMechanismKind::DockerCompose
    }

    fn precheck_is_configured(&self, environment: &EnvironmentConfig) -> bool {
        environment
            .mechanism
            .tasks
            .as_ref()
            .is_some_and(|tasks| tasks.precheck.is_some())
    }

    fn rollback_is_configured(&self, environment: &EnvironmentConfig) -> bool {
        environment.mechanism.tasks.is_some()
    }

    fn rollback_contract_matches(
        &self,
        environment: &EnvironmentConfig,
        reference: &RollbackReference,
    ) -> bool {
        if reference.mechanism_kind() != self.kind() || environment.target != reference.target() {
            return false;
        }
        self.fingerprint(environment)
            .is_ok_and(|fingerprint| fingerprint == *reference.contract_fingerprint())
    }

    async fn preflight(
        &self,
        environment: &EnvironmentConfig,
    ) -> Result<RemotePreflightReport, RemotePreflightError> {
        let (_, _, _, tasks) = self
            .config(environment)
            .map_err(RemotePreflightError::Remote)?;
        let mut required = BTreeSet::from([
            tasks.prepare.clone(),
            tasks.capture_rollback.clone(),
            tasks.apply.clone(),
            tasks.activate.clone(),
            tasks.health_check.clone(),
            tasks.rollback.clone(),
        ]);
        if let Some(precheck) = &tasks.precheck {
            required.insert(precheck.clone());
        }
        preflight_named_tasks(self.remote.as_ref(), &environment.target, required).await
    }

    async fn prepare(
        &self,
        environment: &EnvironmentConfig,
        release: &ReleaseIdentity,
        _local_path: Option<&str>,
    ) -> RemoteExecutionResult<MechanismPrepareExecution> {
        let (_, _, _, tasks) = self.config(environment)?;
        let execution = self
            .run_task(
                &environment.target,
                &tasks.prepare,
                self.candidate_parameters(environment, release)?,
            )
            .await?;
        Ok(MechanismPrepareExecution::Task(execution))
    }

    async fn capture_rollback(
        &self,
        deployment: &Deployment,
        environment: &EnvironmentConfig,
    ) -> RemoteExecutionResult<MechanismRollbackCapture> {
        let (repository, project, service, tasks) = self.config(environment)?;
        let execution = self
            .run_task(
                &environment.target,
                &tasks.capture_rollback,
                self.service_parameters(environment)?,
            )
            .await?;
        if !execution.result.success {
            return Ok(MechanismRollbackCapture {
                execution,
                reference: None,
            });
        }
        let previous_digest = Self::parse_previous_digest(&execution.result.stdout)?;
        let snapshot = DockerComposeRollbackSnapshot::new(
            previous_digest,
            repository,
            project,
            service,
            &tasks.rollback,
            tasks.activate.as_str(),
            tasks.health_check.as_str(),
        )
        .map_err(rollback_error_response)?;
        let reference = RollbackReference::new_with_snapshot(
            deployment.id().clone(),
            deployment.application().clone(),
            deployment.environment().clone(),
            self.kind(),
            environment.target.clone(),
            self.fingerprint(environment)?,
            RollbackMechanismSnapshot::DockerCompose(snapshot),
        )
        .map_err(rollback_error_response)?;
        Ok(MechanismRollbackCapture {
            execution,
            reference: Some(reference),
        })
    }

    async fn execute(
        &self,
        environment: &EnvironmentConfig,
        release: &ReleaseIdentity,
        operation: DeploymentLifecycleOperation,
    ) -> RemoteExecutionResult<MechanismTaskExecution> {
        let (_, _, _, tasks) = self.config(environment)?;
        let (task, parameters) = match operation {
            DeploymentLifecycleOperation::Precheck => (
                tasks.precheck.as_deref().ok_or_else(|| {
                    invalid_mechanism_response("docker precheck task is not configured")
                })?,
                self.service_parameters(environment)?,
            ),
            DeploymentLifecycleOperation::Apply => (
                tasks.apply.as_str(),
                self.candidate_parameters(environment, release)?,
            ),
            DeploymentLifecycleOperation::Activate => (
                tasks.activate.as_str(),
                self.service_parameters(environment)?,
            ),
            DeploymentLifecycleOperation::Verify => (
                tasks.health_check.as_str(),
                self.service_parameters(environment)?,
            ),
            DeploymentLifecycleOperation::Validate
            | DeploymentLifecycleOperation::Prepare
            | DeploymentLifecycleOperation::CaptureRollback => {
                return Err(invalid_mechanism_response(format!(
                    "lifecycle operation {operation:?} is not executable through execute"
                )))
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
        let Some(snapshot) = reference.docker_compose_snapshot() else {
            return Err(RollbackPreflightError::MechanismMismatch);
        };
        preflight_rollback_tasks(
            self.remote.as_ref(),
            reference.target(),
            BTreeSet::from([
                snapshot.rollback_task().to_owned(),
                snapshot.activate_task().to_owned(),
                snapshot.health_check_task().to_owned(),
            ]),
        )
        .await
    }

    async fn execute_rollback(
        &self,
        reference: &RollbackReference,
        action: RollbackMechanismAction,
    ) -> RemoteExecutionResult<MechanismTaskExecution> {
        if reference.mechanism_kind() != self.kind() {
            return Err(invalid_mechanism_response(
                "rollback reference mechanism does not match docker_compose",
            ));
        }
        let snapshot = reference.docker_compose_snapshot().ok_or_else(|| {
            invalid_mechanism_response(
                "rollback reference does not contain docker_compose snapshot",
            )
        })?;
        let service_parameters = || {
            BTreeMap::from([
                (
                    "compose_project".to_owned(),
                    Value::String(snapshot.compose_project().to_owned()),
                ),
                (
                    "service".to_owned(),
                    Value::String(snapshot.service().to_owned()),
                ),
            ])
        };
        let (task, parameters) = match action {
            RollbackMechanismAction::Restore => (
                snapshot.rollback_task(),
                BTreeMap::from([
                    (
                        "image_repository".to_owned(),
                        Value::String(snapshot.image_repository().to_owned()),
                    ),
                    (
                        "digest".to_owned(),
                        Value::String(snapshot.previous_digest().to_owned()),
                    ),
                    (
                        "compose_project".to_owned(),
                        Value::String(snapshot.compose_project().to_owned()),
                    ),
                    (
                        "service".to_owned(),
                        Value::String(snapshot.service().to_owned()),
                    ),
                ]),
            ),
            RollbackMechanismAction::Activate => (snapshot.activate_task(), service_parameters()),
            RollbackMechanismAction::Verify => (snapshot.health_check_task(), service_parameters()),
        };
        self.run_task(reference.target(), task, parameters).await
    }
}

async fn preflight_named_tasks<R: RemoteExecutionPort + ?Sized>(
    remote: &R,
    target: &str,
    required: BTreeSet<String>,
) -> Result<RemotePreflightReport, RemotePreflightError> {
    let check = remote.check_target(target).await?;
    if !check.reachable {
        return Err(RemotePreflightError::TargetUnreachable(target.to_owned()));
    }
    let available_tasks = remote.list_tasks(target).await?;
    let missing = required
        .difference(&available_tasks)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(RemotePreflightError::MissingCapabilities {
            target: target.to_owned(),
            tasks: missing,
        });
    }
    Ok(RemotePreflightReport {
        target: target.to_owned(),
        remote_identity: check.remote_identity,
        available_tasks,
    })
}

async fn preflight_rollback_tasks<R: RemoteExecutionPort + ?Sized>(
    remote: &R,
    target: &str,
    required: BTreeSet<String>,
) -> Result<RemotePreflightReport, RollbackPreflightError> {
    let check = remote
        .check_target(target)
        .await
        .map_err(RollbackPreflightError::TargetCheck)?;
    if !check.reachable {
        return Err(RollbackPreflightError::TargetUnreachable(target.to_owned()));
    }
    let available_tasks = remote
        .list_tasks(target)
        .await
        .map_err(RollbackPreflightError::CapabilityDiscovery)?;
    let missing = required
        .difference(&available_tasks)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(RollbackPreflightError::MissingCapabilities {
            target: target.to_owned(),
            tasks: missing,
        });
    }
    Ok(RemotePreflightReport {
        target: target.to_owned(),
        remote_identity: check.remote_identity,
        available_tasks,
    })
}

fn invalid_mechanism_response(message: impl Into<String>) -> RemoteExecutionError {
    RemoteExecutionError::InvalidResponse {
        tool: "deployment_mechanism".to_owned(),
        message: message.into(),
    }
}

fn rollback_error_response(error: RollbackError) -> RemoteExecutionError {
    invalid_mechanism_response(format!("invalid rollback capability snapshot: {error}"))
}
