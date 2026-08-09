use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio::time::{sleep, timeout, Duration};
use uuid::Uuid;

use super::{preflight_remote_capabilities, DeploymentLockManager, RemotePreflightError};
use crate::config::{Config, EnvironmentConfig};
use crate::domain::{
    ApplicationId, Artifact, Deployment, DeploymentId, DeploymentPlan, DeploymentState,
    DeploymentStep, EnvironmentId,
};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ports::{
    DeploymentRepository, DeploymentReservation, RemoteExecutionError, RemoteExecutionPort,
    RemoteTaskResult, RepositoryError, RepositoryResult, StepAttemptId, StepAttemptStatus,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployRequest {
    pub application: String,
    pub environment: String,
    pub version: String,
    pub artifact_path: String,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentFailure {
    pub step: DeploymentStep,
    pub code: ErrorCode,
    pub message: String,
    pub remote_code: Option<String>,
}

impl DeploymentFailure {
    fn new(step: DeploymentStep, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            step,
            code,
            message: message.into(),
            remote_code: None,
        }
    }

    fn from_remote(
        step: DeploymentStep,
        code: ErrorCode,
        action: &str,
        error: RemoteExecutionError,
    ) -> Self {
        match error {
            RemoteExecutionError::Remote {
                code: remote_code,
                message,
            } => Self {
                step,
                code,
                message: format!("{action}: {message}"),
                remote_code: Some(remote_code),
            },
            other => Self::new(step, code, format!("{action}: {other}")),
        }
    }

    fn persisted_message(&self) -> String {
        match &self.remote_code {
            Some(remote_code) => format!(
                "{}: {} (remote_code={remote_code})",
                self.code, self.message
            ),
            None => format!("{}: {}", self.code, self.message),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeploymentOutcome {
    pub deployment: Deployment,
    pub failure: Option<DeploymentFailure>,
    pub rollback_failure: Option<DeploymentFailure>,
    pub idempotent_replay: bool,
}

pub struct DeployService<R, D>
where
    R: RemoteExecutionPort + ?Sized,
    D: DeploymentRepository + Send,
{
    config: Arc<Config>,
    remote: Arc<R>,
    repository: Arc<Mutex<D>>,
    locks: DeploymentLockManager,
}

impl<R, D> DeployService<R, D>
where
    R: RemoteExecutionPort + ?Sized,
    D: DeploymentRepository + Send,
{
    pub fn new(config: Arc<Config>, remote: Arc<R>, repository: Arc<Mutex<D>>) -> Self {
        Self {
            config,
            remote,
            repository,
            locks: DeploymentLockManager::default(),
        }
    }

    pub fn with_lock_manager(mut self, locks: DeploymentLockManager) -> Self {
        self.locks = locks;
        self
    }

    pub async fn deploy(&self, request: DeployRequest) -> AppResult<DeploymentOutcome> {
        if request.version.trim().is_empty() {
            return Err(AppError::new(
                ErrorCode::InvalidVersion,
                "version must not be empty",
            ));
        }
        validate_idempotency_key(request.idempotency_key.as_deref())?;

        let environment = self
            .config
            .environment(&request.application, &request.environment)?
            .clone();

        let _lease = self
            .locks
            .try_acquire(&request.application, &request.environment)
            .ok_or_else(|| conflict_error(&request.application, &request.environment))?;

        self.reject_durable_conflict(&request.application, &request.environment)?;

        let artifact = load_artifact(&request.version, &request.artifact_path).await?;
        let deployment_id = DeploymentId::new(Uuid::new_v4().to_string()).map_err(|error| {
            AppError::new(
                ErrorCode::InvalidStateTransition,
                format!("failed to create deployment id: {error}"),
            )
        })?;
        let application_id = ApplicationId::new(request.application.clone()).map_err(|error| {
            AppError::invalid_configuration(format!("invalid configured application id: {error}"))
        })?;
        let environment_id = EnvironmentId::new(request.environment.clone()).map_err(|error| {
            AppError::invalid_configuration(format!("invalid configured environment id: {error}"))
        })?;

        let mut deployment = Deployment::new(
            deployment_id.clone(),
            application_id,
            environment_id,
            artifact.clone(),
        );
        match self.repository(|repository| {
            repository.reserve(&deployment, request.idempotency_key.as_deref())
        })? {
            DeploymentReservation::Created => {}
            DeploymentReservation::Reused(existing) => {
                return Ok(DeploymentOutcome {
                    deployment: existing,
                    failure: None,
                    rollback_failure: None,
                    idempotent_replay: true,
                });
            }
        }

        let plan = DeploymentPlan::jar_systemd(
            deployment_id,
            environment.target.clone(),
            artifact,
            environment.tasks.rollback.is_some(),
        )
        .map_err(|error| {
            AppError::new(
                ErrorCode::InvalidStateTransition,
                format!("invalid deployment plan: {error}"),
            )
        })?;

        self.transition(&mut deployment, DeploymentState::Prechecking)?;
        if let Some(failure) = self
            .execute_capability_preflight(deployment.id(), &environment)
            .await?
        {
            return self
                .finish_primary_failure(deployment, &plan, &environment, failure)
                .await;
        }
        if let Some(precheck) = &environment.tasks.precheck {
            if let Some(failure) = self
                .execute_named_task(
                    deployment.id(),
                    DeploymentStep::Precheck,
                    &environment.target,
                    precheck,
                    BTreeMap::new(),
                    ErrorCode::PrecheckFailed,
                    "configured precheck task failed",
                )
                .await?
            {
                return self
                    .finish_primary_failure(deployment, &plan, &environment, failure)
                    .await;
            }
        }

        self.transition(&mut deployment, DeploymentState::StagingArtifact)?;
        if let Some(failure) = self
            .execute_upload(
                deployment.id(),
                &environment,
                &request.artifact_path,
                deployment.artifact().size_bytes(),
            )
            .await?
        {
            return self
                .finish_primary_failure(deployment, &plan, &environment, failure)
                .await;
        }

        self.transition(&mut deployment, DeploymentState::BackingUp)?;
        if let Some(failure) = self
            .execute_named_task(
                deployment.id(),
                DeploymentStep::BackupCurrent,
                &environment.target,
                &environment.tasks.backup,
                backup_parameters(&environment),
                ErrorCode::RemoteExecutionFailed,
                "backup task failed",
            )
            .await?
        {
            return self
                .finish_primary_failure(deployment, &plan, &environment, failure)
                .await;
        }

        self.transition(&mut deployment, DeploymentState::Installing)?;
        if let Some(failure) = self
            .execute_named_task(
                deployment.id(),
                DeploymentStep::Install,
                &environment.target,
                &environment.tasks.install,
                install_parameters(&environment),
                ErrorCode::RemoteExecutionFailed,
                "install task failed",
            )
            .await?
        {
            return self
                .finish_primary_failure(deployment, &plan, &environment, failure)
                .await;
        }

        self.transition(&mut deployment, DeploymentState::Restarting)?;
        if let Some(failure) = self
            .execute_named_task(
                deployment.id(),
                DeploymentStep::Restart,
                &environment.target,
                &environment.tasks.restart,
                BTreeMap::new(),
                ErrorCode::RemoteExecutionFailed,
                "restart task failed",
            )
            .await?
        {
            return self
                .finish_primary_failure(deployment, &plan, &environment, failure)
                .await;
        }

        self.transition(&mut deployment, DeploymentState::Verifying)?;
        if let Some(failure) = self
            .execute_verification(
                deployment.id(),
                &environment.target,
                &environment.tasks.health_check,
                ErrorCode::VerificationFailed,
                "health-check task failed",
            )
            .await?
        {
            return self
                .finish_primary_failure(deployment, &plan, &environment, failure)
                .await;
        }

        self.transition(&mut deployment, DeploymentState::Succeeded)?;
        Ok(DeploymentOutcome {
            deployment,
            failure: None,
            rollback_failure: None,
            idempotent_replay: false,
        })
    }

    fn reject_durable_conflict(&self, application: &str, environment: &str) -> AppResult<()> {
        let active = self.repository(|repository| repository.list_non_terminal())?;
        if active.iter().any(|deployment| {
            deployment.application().as_str() == application
                && deployment.environment().as_str() == environment
        }) {
            return Err(conflict_error(application, environment));
        }
        Ok(())
    }

    async fn execute_capability_preflight(
        &self,
        deployment_id: &DeploymentId,
        environment: &EnvironmentConfig,
    ) -> AppResult<Option<DeploymentFailure>> {
        let attempt = self.start_attempt(deployment_id, DeploymentStep::Precheck)?;
        let timeout_ms = self.config.runtime.deployment_step_timeout_ms;
        let result = timeout(
            Duration::from_millis(timeout_ms),
            preflight_remote_capabilities(self.remote.as_ref(), environment),
        )
        .await;
        let failure = match result {
            Ok(Ok(_)) => None,
            Ok(Err(error)) => Some(preflight_failure(error)),
            Err(_) => Some(deployment_timeout_failure(
                DeploymentStep::Precheck,
                timeout_ms,
            )),
        };
        self.finish_from_failure(attempt, failure.as_ref())?;
        Ok(failure)
    }

    async fn execute_upload(
        &self,
        deployment_id: &DeploymentId,
        environment: &EnvironmentConfig,
        local_path: &str,
        expected_bytes: u64,
    ) -> AppResult<Option<DeploymentFailure>> {
        let attempt = self.start_attempt(deployment_id, DeploymentStep::StageArtifact)?;
        let timeout_ms = self.config.runtime.deployment_step_timeout_ms;
        let result = timeout(
            Duration::from_millis(timeout_ms),
            self.remote.upload_file(
                &environment.target,
                local_path,
                &environment.staging_path,
                true,
            ),
        )
        .await;

        let failure = match result {
            Err(_) => Some(deployment_timeout_failure(
                DeploymentStep::StageArtifact,
                timeout_ms,
            )),
            Ok(Ok(result)) if result.bytes_transferred == expected_bytes => None,
            Ok(Ok(result)) => Some(DeploymentFailure::new(
                DeploymentStep::StageArtifact,
                ErrorCode::ArtifactChanged,
                format!(
                    "staged byte count changed: expected {expected_bytes}, transferred {}",
                    result.bytes_transferred
                ),
            )),
            Ok(Err(error)) => Some(DeploymentFailure::from_remote(
                DeploymentStep::StageArtifact,
                ErrorCode::RemoteExecutionFailed,
                "artifact staging failed",
                error,
            )),
        };

        self.finish_from_failure(attempt, failure.as_ref())?;
        Ok(failure)
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_named_task(
        &self,
        deployment_id: &DeploymentId,
        step: DeploymentStep,
        target: &str,
        task: &str,
        parameters: BTreeMap<String, Value>,
        failure_code: ErrorCode,
        action: &str,
    ) -> AppResult<Option<DeploymentFailure>> {
        let attempt = self.start_attempt(deployment_id, step)?;
        let timeout_ms = self.config.runtime.deployment_step_timeout_ms;
        let result = timeout(
            Duration::from_millis(timeout_ms),
            self.remote.run_task(target, task, parameters),
        )
        .await;
        let failure = match result {
            Err(_) => Some(deployment_timeout_failure(step, timeout_ms)),
            Ok(Ok(result)) if result.success => None,
            Ok(Ok(result)) => Some(task_result_failure(
                step,
                failure_code,
                action,
                task,
                &result,
            )),
            Ok(Err(error)) => Some(DeploymentFailure::from_remote(
                step,
                failure_code,
                action,
                error,
            )),
        };

        self.finish_from_failure(attempt, failure.as_ref())?;
        Ok(failure)
    }

    async fn execute_verification(
        &self,
        deployment_id: &DeploymentId,
        target: &str,
        task: &str,
        failure_code: ErrorCode,
        action: &str,
    ) -> AppResult<Option<DeploymentFailure>> {
        let max_attempts = self.config.runtime.verification_max_attempts;
        let retry_delay_ms = self.config.runtime.verification_retry_delay_ms;
        for attempt in 1..=max_attempts {
            let failure = self
                .execute_named_task(
                    deployment_id,
                    DeploymentStep::Verify,
                    target,
                    task,
                    BTreeMap::new(),
                    failure_code,
                    action,
                )
                .await?;
            match failure {
                None => return Ok(None),
                Some(failure) if failure.code == ErrorCode::OperationTimedOut => {
                    return Ok(Some(failure));
                }
                Some(failure) if attempt == max_attempts => return Ok(Some(failure)),
                Some(_) => sleep(Duration::from_millis(retry_delay_ms)).await,
            }
        }
        unreachable!("validated verification policy always has at least one attempt")
    }

    async fn finish_primary_failure(
        &self,
        mut deployment: Deployment,
        plan: &DeploymentPlan,
        environment: &EnvironmentConfig,
        failure: DeploymentFailure,
    ) -> AppResult<DeploymentOutcome> {
        if failure.code == ErrorCode::OperationTimedOut
            && deployment.state().has_live_mutation_started()
        {
            return Ok(DeploymentOutcome {
                deployment,
                failure: Some(failure),
                rollback_failure: None,
                idempotent_replay: false,
            });
        }

        if !plan.requires_rollback_after_failure(failure.step) {
            self.transition(&mut deployment, DeploymentState::Failed)?;
            return Ok(DeploymentOutcome {
                deployment,
                failure: Some(failure),
                rollback_failure: None,
                idempotent_replay: false,
            });
        }

        self.transition(&mut deployment, DeploymentState::RollingBack)?;
        let rollback_failure = self.execute_rollback(deployment.id(), environment).await?;
        if rollback_failure
            .as_ref()
            .is_some_and(|failure| failure.code == ErrorCode::OperationTimedOut)
        {
            return Ok(DeploymentOutcome {
                deployment,
                failure: Some(failure),
                rollback_failure,
                idempotent_replay: false,
            });
        }
        match rollback_failure {
            None => self.transition(&mut deployment, DeploymentState::RolledBack)?,
            Some(_) => self.transition(&mut deployment, DeploymentState::RollbackFailed)?,
        }

        Ok(DeploymentOutcome {
            deployment,
            failure: Some(failure),
            rollback_failure,
            idempotent_replay: false,
        })
    }

    async fn execute_rollback(
        &self,
        deployment_id: &DeploymentId,
        environment: &EnvironmentConfig,
    ) -> AppResult<Option<DeploymentFailure>> {
        let rollback_task = match environment.tasks.rollback.as_deref() {
            Some(task) => task,
            None => {
                return Ok(Some(DeploymentFailure::new(
                    DeploymentStep::Install,
                    ErrorCode::RollbackUnavailable,
                    "rollback was required but no rollback task is configured",
                )))
            }
        };

        if let Some(failure) = self
            .execute_named_task(
                deployment_id,
                DeploymentStep::Install,
                &environment.target,
                rollback_task,
                rollback_parameters(environment),
                ErrorCode::RollbackFailed,
                "rollback restore task failed",
            )
            .await?
        {
            return Ok(Some(failure));
        }

        if let Some(failure) = self
            .execute_named_task(
                deployment_id,
                DeploymentStep::Restart,
                &environment.target,
                &environment.tasks.restart,
                BTreeMap::new(),
                ErrorCode::RollbackFailed,
                "rollback restart task failed",
            )
            .await?
        {
            return Ok(Some(failure));
        }

        self.execute_verification(
            deployment_id,
            &environment.target,
            &environment.tasks.health_check,
            ErrorCode::RollbackFailed,
            "rollback verification task failed",
        )
        .await
    }

    fn transition(&self, deployment: &mut Deployment, next: DeploymentState) -> AppResult<()> {
        let from = deployment.state();
        let mut candidate = deployment.clone();
        candidate
            .transition(next)
            .map_err(|error| AppError::new(ErrorCode::InvalidStateTransition, error.to_string()))?;
        self.repository(|repository| repository.persist_transition(deployment.id(), from, next))?;
        *deployment = candidate;
        Ok(())
    }

    fn start_attempt(
        &self,
        deployment_id: &DeploymentId,
        step: DeploymentStep,
    ) -> AppResult<StepAttemptId> {
        self.repository(|repository| repository.start_step(deployment_id, step))
    }

    fn finish_attempt(
        &self,
        attempt: StepAttemptId,
        status: StepAttemptStatus,
        error: Option<&str>,
    ) -> AppResult<()> {
        self.repository(|repository| repository.finish_step(attempt, status, error))
    }

    fn finish_from_failure(
        &self,
        attempt: StepAttemptId,
        failure: Option<&DeploymentFailure>,
    ) -> AppResult<()> {
        match failure {
            None => self.finish_attempt(attempt, StepAttemptStatus::Succeeded, None),
            Some(failure) => {
                let message = failure.persisted_message();
                self.finish_attempt(attempt, StepAttemptStatus::Failed, Some(&message))
            }
        }
    }

    fn repository<T>(&self, operation: impl FnOnce(&mut D) -> RepositoryResult<T>) -> AppResult<T> {
        let mut repository = self.repository.lock().map_err(|_| {
            AppError::new(
                ErrorCode::PersistenceFailed,
                "deployment repository lock poisoned",
            )
        })?;
        operation(&mut *repository).map_err(repository_error)
    }
}

fn validate_idempotency_key(key: Option<&str>) -> AppResult<()> {
    let Some(key) = key else {
        return Ok(());
    };
    if key.is_empty() || key.len() > 128 {
        return Err(AppError::new(
            ErrorCode::InvalidRequest,
            "idempotency_key must contain between 1 and 128 bytes",
        ));
    }
    if key.trim() != key || key.chars().any(char::is_control) {
        return Err(AppError::new(
            ErrorCode::InvalidRequest,
            "idempotency_key must not contain leading/trailing whitespace or control characters",
        ));
    }
    Ok(())
}

async fn load_artifact(version: &str, path: &str) -> AppResult<Artifact> {
    let mut file = tokio::fs::File::open(path).await.map_err(|error| {
        AppError::new(
            ErrorCode::ArtifactNotFound,
            format!("cannot open artifact {path}: {error}"),
        )
    })?;
    let metadata = file.metadata().await.map_err(|error| {
        AppError::new(
            ErrorCode::ArtifactNotFound,
            format!("cannot stat artifact {path}: {error}"),
        )
    })?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(AppError::new(
            ErrorCode::InvalidArtifact,
            format!("artifact must be a non-empty regular file: {path}"),
        ));
    }

    let expected_size = metadata.len();
    let mut actual_size = 0_u64;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).await.map_err(|error| {
            AppError::new(
                ErrorCode::ArtifactNotFound,
                format!("cannot read artifact {path}: {error}"),
            )
        })?;
        if read == 0 {
            break;
        }
        actual_size = actual_size.saturating_add(read as u64);
        digest.update(&buffer[..read]);
    }

    if actual_size != expected_size {
        return Err(AppError::new(
            ErrorCode::ArtifactChanged,
            format!(
                "artifact size changed while hashing: metadata={expected_size}, read={actual_size}"
            ),
        ));
    }

    let sha256 = format!("{:x}", digest.finalize());
    Artifact::new(version, actual_size, sha256)
        .map_err(|error| AppError::new(ErrorCode::InvalidArtifact, error.to_string()))
}

fn deployment_timeout_failure(step: DeploymentStep, timeout_ms: u64) -> DeploymentFailure {
    DeploymentFailure::new(
        step,
        ErrorCode::OperationTimedOut,
        format!("deployment step {step:?} exceeded {timeout_ms} ms; remote completion is unknown"),
    )
}

fn preflight_failure(error: RemotePreflightError) -> DeploymentFailure {
    match error {
        RemotePreflightError::MissingCapabilities { target, tasks } => DeploymentFailure::new(
            DeploymentStep::Precheck,
            ErrorCode::RemoteCapabilityMissing,
            format!("target {target} is missing capabilities: {tasks:?}"),
        ),
        RemotePreflightError::TargetUnreachable(target) => DeploymentFailure::new(
            DeploymentStep::Precheck,
            ErrorCode::PrecheckFailed,
            format!("target is not reachable: {target}"),
        ),
        RemotePreflightError::Remote(error) => DeploymentFailure::from_remote(
            DeploymentStep::Precheck,
            ErrorCode::PrecheckFailed,
            "remote capability preflight failed",
            error,
        ),
    }
}

fn task_result_failure(
    step: DeploymentStep,
    code: ErrorCode,
    action: &str,
    task: &str,
    result: &RemoteTaskResult,
) -> DeploymentFailure {
    let detail = if !result.stderr.trim().is_empty() {
        result.stderr.trim()
    } else if !result.stdout.trim().is_empty() {
        result.stdout.trim()
    } else {
        "no remote output"
    };
    DeploymentFailure::new(
        step,
        code,
        format!(
            "{action}: task={task}, exit_code={:?}, detail={detail}",
            result.exit_code
        ),
    )
}

fn backup_parameters(environment: &EnvironmentConfig) -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            "install_path".to_owned(),
            Value::String(environment.install_path.clone()),
        ),
        (
            "backup_path".to_owned(),
            Value::String(environment.backup_path.clone()),
        ),
    ])
}

fn install_parameters(environment: &EnvironmentConfig) -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            "staging_path".to_owned(),
            Value::String(environment.staging_path.clone()),
        ),
        (
            "install_path".to_owned(),
            Value::String(environment.install_path.clone()),
        ),
    ])
}

fn rollback_parameters(environment: &EnvironmentConfig) -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            "backup_path".to_owned(),
            Value::String(environment.backup_path.clone()),
        ),
        (
            "install_path".to_owned(),
            Value::String(environment.install_path.clone()),
        ),
    ])
}

fn repository_error(error: RepositoryError) -> AppError {
    match error {
        RepositoryError::AlreadyExists(_) | RepositoryError::MutationConflict { .. } => {
            AppError::new(
                ErrorCode::ConflictingDeployment,
                "another active mutation already exists for this application/environment",
            )
        }
        RepositoryError::IdempotencyConflict(key) => AppError::new(
            ErrorCode::IdempotencyConflict,
            format!("idempotency key is already bound to a different deployment intent: {key}"),
        ),
        RepositoryError::ArtifactVersionConflict {
            application,
            environment,
            version,
            existing_sha256,
            requested_sha256,
        } => AppError::new(
            ErrorCode::ArtifactVersionConflict,
            format!(
                "version {version} for {application}/{environment} is already bound to checksum {existing_sha256}; requested checksum is {requested_sha256}"
            ),
        ),
        other => AppError::new(ErrorCode::PersistenceFailed, other.to_string()),
    }
}

fn conflict_error(application: &str, environment: &str) -> AppError {
    AppError::new(
        ErrorCode::ConflictingDeployment,
        format!("deployment already active for {application}/{environment}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{FakeRemoteCall, FakeRemoteExecution};
    use crate::persistence::SqliteDeploymentRepository;
    use crate::ports::{
        RemoteTargetCheck, RemoteTaskResult, RemoteTransferResult, StepAttemptStatus,
    };
    use tempfile::tempdir;

    const CONFIG: &str = r#"
remote_exec:
  command: remote-exec-mcp
applications:
  demo:
    artifact_type: jar
    environments:
      test:
        target: test-server
        staging_path: /opt/staging/demo.jar
        install_path: /opt/apps/demo/demo.jar
        backup_path: /opt/apps/demo/backup/demo.jar
        tasks:
          precheck: demo-precheck
          backup: demo-backup
          install: demo-install
          restart: demo-restart
          health_check: demo-health
          rollback: demo-rollback
"#;

    fn success_task() -> RemoteTaskResult {
        RemoteTaskResult {
            success: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            duration_ms: 1,
            stdout_truncated: false,
            stderr_truncated: false,
        }
    }

    fn failed_task(message: &str) -> RemoteTaskResult {
        RemoteTaskResult {
            success: false,
            exit_code: Some(1),
            stdout: String::new(),
            stderr: message.to_owned(),
            duration_ms: 1,
            stdout_truncated: false,
            stderr_truncated: false,
        }
    }

    fn configured_remote(artifact_path: &str, artifact_size: u64) -> FakeRemoteExecution {
        let fake = FakeRemoteExecution::default();
        fake.set_target_check(
            "test-server",
            Ok(RemoteTargetCheck {
                reachable: true,
                remote_identity: Some("test-host".to_owned()),
            }),
        );
        fake.set_tasks(
            "test-server",
            Ok([
                "demo-precheck",
                "demo-backup",
                "demo-install",
                "demo-restart",
                "demo-health",
                "demo-rollback",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()),
        );
        fake.set_upload_result(
            "test-server",
            artifact_path,
            "/opt/staging/demo.jar",
            true,
            Ok(RemoteTransferResult {
                bytes_transferred: artifact_size,
            }),
        );
        for task in [
            "demo-precheck",
            "demo-backup",
            "demo-install",
            "demo-restart",
            "demo-health",
            "demo-rollback",
        ] {
            fake.set_task_result("test-server", task, Ok(success_task()));
        }
        fake
    }

    fn request(artifact_path: &str) -> DeployRequest {
        DeployRequest {
            application: "demo".to_owned(),
            environment: "test".to_owned(),
            version: "1.2.3".to_owned(),
            artifact_path: artifact_path.to_owned(),
            idempotency_key: None,
        }
    }

    fn config() -> Arc<Config> {
        Arc::new(Config::from_yaml(CONFIG).unwrap())
    }

    fn repository() -> Arc<Mutex<SqliteDeploymentRepository>> {
        Arc::new(Mutex::new(SqliteDeploymentRepository::in_memory().unwrap()))
    }

    #[tokio::test]
    async fn jar_systemd_success_path_is_durable_and_ordered() {
        let directory = tempdir().unwrap();
        let artifact_path = directory.path().join("demo.jar");
        std::fs::write(&artifact_path, b"jar-content").unwrap();
        let artifact_path = artifact_path.to_string_lossy().into_owned();
        let fake = configured_remote(&artifact_path, 11);
        let repository = repository();
        let service = DeployService::new(config(), Arc::new(fake.clone()), Arc::clone(&repository));

        let outcome = service.deploy(request(&artifact_path)).await.unwrap();
        assert_eq!(outcome.deployment.state(), DeploymentState::Succeeded);
        assert!(!outcome.idempotent_replay);
        assert!(outcome.failure.is_none());
        assert!(outcome.rollback_failure.is_none());

        let stored = repository
            .lock()
            .unwrap()
            .get(outcome.deployment.id())
            .unwrap()
            .unwrap();
        assert_eq!(stored.state(), DeploymentState::Succeeded);
        let attempts = repository
            .lock()
            .unwrap()
            .step_attempts(outcome.deployment.id())
            .unwrap();
        assert_eq!(attempts.len(), 7);
        assert!(attempts
            .iter()
            .all(|attempt| attempt.status == StepAttemptStatus::Succeeded));

        let calls = fake.calls();
        assert!(matches!(calls[0], FakeRemoteCall::CheckTarget { .. }));
        assert!(matches!(calls[1], FakeRemoteCall::ListTasks { .. }));
        assert!(
            matches!(calls[2], FakeRemoteCall::RunTask { ref task, .. } if task == "demo-precheck")
        );
        assert!(matches!(calls[3], FakeRemoteCall::UploadFile { .. }));
        assert!(
            matches!(calls[4], FakeRemoteCall::RunTask { ref task, .. } if task == "demo-backup")
        );
        assert!(
            matches!(calls[5], FakeRemoteCall::RunTask { ref task, .. } if task == "demo-install")
        );
        assert!(
            matches!(calls[6], FakeRemoteCall::RunTask { ref task, .. } if task == "demo-restart")
        );
        assert!(
            matches!(calls[7], FakeRemoteCall::RunTask { ref task, .. } if task == "demo-health")
        );

        match &calls[4] {
            FakeRemoteCall::RunTask { parameters, .. } => {
                assert_eq!(
                    parameters.get("install_path"),
                    Some(&Value::String("/opt/apps/demo/demo.jar".to_owned()))
                );
                assert_eq!(
                    parameters.get("backup_path"),
                    Some(&Value::String("/opt/apps/demo/backup/demo.jar".to_owned()))
                );
            }
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn install_failure_rolls_back_then_restarts_and_verifies_previous_release() {
        let directory = tempdir().unwrap();
        let artifact_path = directory.path().join("demo.jar");
        std::fs::write(&artifact_path, b"jar-content").unwrap();
        let artifact_path = artifact_path.to_string_lossy().into_owned();
        let fake = configured_remote(&artifact_path, 11);
        fake.set_task_result(
            "test-server",
            "demo-install",
            Ok(failed_task("install failed")),
        );
        let repository = repository();
        let service = DeployService::new(config(), Arc::new(fake.clone()), Arc::clone(&repository));

        let outcome = service.deploy(request(&artifact_path)).await.unwrap();
        assert_eq!(outcome.deployment.state(), DeploymentState::RolledBack);
        let failure = outcome.failure.unwrap();
        assert_eq!(failure.step, DeploymentStep::Install);
        assert_eq!(failure.code, ErrorCode::RemoteExecutionFailed);
        assert!(outcome.rollback_failure.is_none());

        let calls = fake.calls();
        let tasks = calls
            .iter()
            .filter_map(|call| match call {
                FakeRemoteCall::RunTask { task, .. } => Some(task.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            tasks,
            [
                "demo-precheck",
                "demo-backup",
                "demo-install",
                "demo-rollback",
                "demo-restart",
                "demo-health"
            ]
        );
    }

    #[tokio::test]
    async fn rollback_failure_is_preserved_separately_from_original_failure() {
        let directory = tempdir().unwrap();
        let artifact_path = directory.path().join("demo.jar");
        std::fs::write(&artifact_path, b"jar-content").unwrap();
        let artifact_path = artifact_path.to_string_lossy().into_owned();
        let fake = configured_remote(&artifact_path, 11);
        fake.set_task_result(
            "test-server",
            "demo-install",
            Ok(failed_task("install failed")),
        );
        fake.set_task_result(
            "test-server",
            "demo-rollback",
            Ok(failed_task("restore failed")),
        );
        let repository = repository();
        let service = DeployService::new(config(), Arc::new(fake), Arc::clone(&repository));

        let outcome = service.deploy(request(&artifact_path)).await.unwrap();
        assert_eq!(outcome.deployment.state(), DeploymentState::RollbackFailed);
        assert_eq!(
            outcome.failure.as_ref().unwrap().code,
            ErrorCode::RemoteExecutionFailed
        );
        assert_eq!(
            outcome.rollback_failure.as_ref().unwrap().code,
            ErrorCode::RollbackFailed
        );
        assert!(outcome
            .failure
            .as_ref()
            .unwrap()
            .message
            .contains("install failed"));
        assert!(outcome
            .rollback_failure
            .as_ref()
            .unwrap()
            .message
            .contains("restore failed"));
    }

    #[tokio::test]
    async fn backup_failure_is_terminal_without_rollback() {
        let directory = tempdir().unwrap();
        let artifact_path = directory.path().join("demo.jar");
        std::fs::write(&artifact_path, b"jar-content").unwrap();
        let artifact_path = artifact_path.to_string_lossy().into_owned();
        let fake = configured_remote(&artifact_path, 11);
        fake.set_task_result(
            "test-server",
            "demo-backup",
            Ok(failed_task("backup failed")),
        );
        let service = DeployService::new(config(), Arc::new(fake.clone()), repository());

        let outcome = service.deploy(request(&artifact_path)).await.unwrap();
        assert_eq!(outcome.deployment.state(), DeploymentState::Failed);
        assert_eq!(outcome.failure.unwrap().step, DeploymentStep::BackupCurrent);
        assert!(!fake.calls().iter().any(|call| {
            matches!(call, FakeRemoteCall::RunTask { task, .. } if task == "demo-rollback")
        }));
    }

    #[tokio::test]
    async fn durable_non_terminal_deployment_rejects_new_deploy_before_remote_work() {
        let repository = repository();
        let existing = Deployment::new(
            DeploymentId::new("existing").unwrap(),
            ApplicationId::new("demo").unwrap(),
            EnvironmentId::new("test").unwrap(),
            Artifact::new(
                "1.0.0",
                1,
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .unwrap(),
        );
        repository.lock().unwrap().create(&existing).unwrap();
        let fake = FakeRemoteExecution::default();
        let service = DeployService::new(config(), Arc::new(fake.clone()), repository);

        let error = service.deploy(request("/missing.jar")).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::ConflictingDeployment);
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn idempotency_key_validation_is_bounded_and_canonical() {
        assert!(validate_idempotency_key(None).is_ok());
        assert!(validate_idempotency_key(Some("request-123")).is_ok());
        assert_eq!(
            validate_idempotency_key(Some(" request-123"))
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            validate_idempotency_key(Some("")).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            validate_idempotency_key(Some(&"x".repeat(129)))
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
    }
}
