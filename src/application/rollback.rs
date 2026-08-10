use std::sync::{Arc, Mutex};

use tokio::time::{sleep, timeout, Duration};
use uuid::Uuid;

use super::{
    mechanism::{
        DeploymentMechanismPort, JarSystemdMechanism, RollbackMechanismAction,
        RollbackPreflightError,
    },
    DeploymentLockManager,
};
use crate::config::{Config, EnvironmentConfig};
use crate::domain::{
    DeploymentId, DeploymentState, RollbackOperation, RollbackOperationId, RollbackOperationState,
    RollbackReference,
};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ports::{
    DeploymentRepository, RemoteExecutionError, RemoteExecutionPort, RemoteTaskResult,
    RepositoryError, RepositoryResult, RollbackRepository,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackRequest {
    pub deployment_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackFailure {
    pub code: ErrorCode,
    pub message: String,
    pub remote_code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RollbackOutcome {
    pub operation: RollbackOperation,
    pub failure: Option<RollbackFailure>,
}

pub struct RollbackService<M, D>
where
    M: DeploymentMechanismPort + ?Sized,
    D: DeploymentRepository + Send,
{
    config: Arc<Config>,
    mechanism: Arc<M>,
    deployments: Arc<Mutex<D>>,
    rollbacks: Arc<Mutex<Box<dyn RollbackRepository + Send>>>,
    locks: DeploymentLockManager,
}

impl<R, D> RollbackService<JarSystemdMechanism<R>, D>
where
    R: RemoteExecutionPort + ?Sized,
    D: DeploymentRepository + Send,
{
    pub fn new(
        config: Arc<Config>,
        remote: Arc<R>,
        deployments: Arc<Mutex<D>>,
        rollbacks: Arc<Mutex<Box<dyn RollbackRepository + Send>>>,
        locks: DeploymentLockManager,
    ) -> Self {
        Self::with_mechanism(
            config,
            Arc::new(JarSystemdMechanism::new(remote)),
            deployments,
            rollbacks,
            locks,
        )
    }
}

impl<M, D> RollbackService<M, D>
where
    M: DeploymentMechanismPort + ?Sized,
    D: DeploymentRepository + Send,
{
    pub fn with_mechanism(
        config: Arc<Config>,
        mechanism: Arc<M>,
        deployments: Arc<Mutex<D>>,
        rollbacks: Arc<Mutex<Box<dyn RollbackRepository + Send>>>,
        locks: DeploymentLockManager,
    ) -> Self {
        Self {
            config,
            mechanism,
            deployments,
            rollbacks,
            locks,
        }
    }

    pub async fn rollback(&self, request: RollbackRequest) -> AppResult<RollbackOutcome> {
        let deployment_id = DeploymentId::new(request.deployment_id.clone()).map_err(|_| {
            AppError::new(
                ErrorCode::UnknownDeployment,
                format!("unknown deployment: {}", request.deployment_id),
            )
        })?;
        let deployment = self
            .deployment_repository(|repository| repository.get(&deployment_id))?
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::UnknownDeployment,
                    format!("unknown deployment: {}", request.deployment_id),
                )
            })?;

        if !matches!(
            deployment.state(),
            DeploymentState::Succeeded | DeploymentState::RollbackFailed
        ) {
            return Err(AppError::new(
                ErrorCode::RollbackUnavailable,
                format!(
                    "deployment {} in state {:?} is not eligible for explicit rollback",
                    deployment.id().as_str(),
                    deployment.state()
                ),
            ));
        }

        let environment = self
            .config
            .environment(
                deployment.application().as_str(),
                deployment.environment().as_str(),
            )
            .map_err(|_| {
                AppError::new(
                    ErrorCode::RollbackUnavailable,
                    "deployment environment is no longer configured",
                )
            })?;
        let reference = self
            .rollback_repository(|repository| repository.get_reference(&deployment_id))?
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::RollbackUnavailable,
                    format!(
                        "no durable rollback reference exists for {}",
                        deployment.id().as_str()
                    ),
                )
            })?;
        ensure_environment_contract(environment, &reference)?;

        let _lease = self
            .locks
            .try_acquire(
                deployment.application().as_str(),
                deployment.environment().as_str(),
            )
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::ConflictingDeployment,
                    format!(
                        "mutation already active for {}/{}",
                        deployment.application().as_str(),
                        deployment.environment().as_str()
                    ),
                )
            })?;

        let operation_id =
            RollbackOperationId::new(Uuid::new_v4().to_string()).map_err(|error| {
                AppError::new(
                    ErrorCode::InvalidStateTransition,
                    format!("failed to create rollback operation id: {error}"),
                )
            })?;
        let operation = RollbackOperation::new(
            operation_id.clone(),
            deployment.id().clone(),
            deployment.application().clone(),
            deployment.environment().clone(),
        );
        let reference =
            self.rollback_repository(|repository| repository.begin_operation(&operation))?;

        let timeout_ms = self.config.runtime.explicit_rollback_timeout_ms;
        let operation_result = timeout(Duration::from_millis(timeout_ms), async {
            if let Some(failure) = self.preflight(&reference).await {
                return Some(failure);
            }
            if let Some(failure) = self
                .run_task(
                    &reference,
                    RollbackMechanismAction::Restore,
                    "rollback restore task failed",
                )
                .await
            {
                return Some(failure);
            }
            if let Some(failure) = self
                .run_task(
                    &reference,
                    RollbackMechanismAction::Activate,
                    "rollback restart task failed",
                )
                .await
            {
                return Some(failure);
            }
            if let Some(failure) = self.run_verification_with_retry(&reference).await {
                return Some(failure);
            }
            None
        })
        .await;

        match operation_result {
            Ok(Some(failure)) => {
                return self.finish_failed(operation_id, deployment_id, failure);
            }
            Ok(None) => {}
            Err(_) => {
                return Err(AppError::new(
                    ErrorCode::OperationTimedOut,
                    format!(
                        "explicit rollback operation {} exceeded {timeout_ms} ms; durable operation remains STARTED and further mutation is blocked until recovery/reconciliation",
                        operation_id.as_str()
                    ),
                ));
            }
        }

        self.rollback_repository(|repository| {
            repository.finish_operation(
                &operation_id,
                &deployment_id,
                RollbackOperationState::Succeeded,
                None,
            )
        })?;
        let operation = self.load_operation(&operation_id)?;
        Ok(RollbackOutcome {
            operation,
            failure: None,
        })
    }

    async fn preflight(&self, reference: &RollbackReference) -> Option<RollbackFailure> {
        match self.mechanism.preflight_rollback(reference).await {
            Ok(_) => None,
            Err(RollbackPreflightError::TargetUnreachable(target)) => Some(RollbackFailure::new(
                ErrorCode::PrecheckFailed,
                format!("rollback target is not reachable: {target}"),
            )),
            Err(RollbackPreflightError::MissingCapabilities { tasks, .. }) => {
                Some(RollbackFailure::new(
                    ErrorCode::RemoteCapabilityMissing,
                    format!("rollback target is missing capabilities: {tasks:?}"),
                ))
            }
            Err(RollbackPreflightError::TargetCheck(error)) => Some(RollbackFailure::from_remote(
                ErrorCode::PrecheckFailed,
                "rollback target preflight failed",
                error,
            )),
            Err(RollbackPreflightError::CapabilityDiscovery(error)) => {
                Some(RollbackFailure::from_remote(
                    ErrorCode::PrecheckFailed,
                    "rollback capability preflight failed",
                    error,
                ))
            }
        }
    }

    async fn run_task(
        &self,
        reference: &RollbackReference,
        mechanism_action: RollbackMechanismAction,
        action: &str,
    ) -> Option<RollbackFailure> {
        match self
            .mechanism
            .execute_rollback(reference, mechanism_action)
            .await
        {
            Ok(execution) if execution.result.success => None,
            Ok(execution) => Some(task_failure(action, &execution.task, &execution.result)),
            Err(error) => Some(RollbackFailure::from_remote(
                ErrorCode::RollbackFailed,
                action,
                error,
            )),
        }
    }

    async fn run_verification_with_retry(
        &self,
        reference: &RollbackReference,
    ) -> Option<RollbackFailure> {
        let max_attempts = self.config.runtime.verification_max_attempts;
        let retry_delay_ms = self.config.runtime.verification_retry_delay_ms;
        for attempt in 1..=max_attempts {
            let failure = self
                .run_task(
                    reference,
                    RollbackMechanismAction::Verify,
                    "rollback verification task failed",
                )
                .await;
            match failure {
                None => return None,
                Some(failure) if attempt == max_attempts => return Some(failure),
                Some(_) => sleep(Duration::from_millis(retry_delay_ms)).await,
            }
        }
        unreachable!("validated verification policy always has at least one attempt")
    }

    fn finish_failed(
        &self,
        operation_id: RollbackOperationId,
        deployment_id: DeploymentId,
        failure: RollbackFailure,
    ) -> AppResult<RollbackOutcome> {
        self.rollback_repository(|repository| {
            repository.finish_operation(
                &operation_id,
                &deployment_id,
                RollbackOperationState::Failed,
                Some(&failure.persisted_message()),
            )
        })?;
        let operation = self.load_operation(&operation_id)?;
        Ok(RollbackOutcome {
            operation,
            failure: Some(failure),
        })
    }

    fn load_operation(&self, id: &RollbackOperationId) -> AppResult<RollbackOperation> {
        self.rollback_repository(|repository| repository.get_operation(id))?
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::PersistenceFailed,
                    format!("rollback operation disappeared: {}", id.as_str()),
                )
            })
    }

    fn deployment_repository<T>(
        &self,
        operation: impl FnOnce(&D) -> RepositoryResult<T>,
    ) -> AppResult<T> {
        let repository = self.deployments.lock().map_err(|_| {
            AppError::new(
                ErrorCode::PersistenceFailed,
                "deployment repository lock poisoned",
            )
        })?;
        operation(&*repository).map_err(repository_error)
    }

    fn rollback_repository<T>(
        &self,
        operation: impl FnOnce(&mut dyn RollbackRepository) -> RepositoryResult<T>,
    ) -> AppResult<T> {
        let mut repository = self.rollbacks.lock().map_err(|_| {
            AppError::new(
                ErrorCode::PersistenceFailed,
                "rollback repository lock poisoned",
            )
        })?;
        operation(repository.as_mut()).map_err(repository_error)
    }
}

impl RollbackFailure {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            remote_code: None,
        }
    }

    fn from_remote(code: ErrorCode, action: &str, error: RemoteExecutionError) -> Self {
        match error {
            RemoteExecutionError::Remote {
                code: remote_code,
                message,
            } => Self {
                code,
                message: format!("{action}: {message}"),
                remote_code: Some(remote_code),
            },
            other => Self::new(code, format!("{action}: {other}")),
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

fn task_failure(action: &str, task: &str, result: &RemoteTaskResult) -> RollbackFailure {
    let detail = if !result.stderr.trim().is_empty() {
        result.stderr.trim()
    } else if !result.stdout.trim().is_empty() {
        result.stdout.trim()
    } else {
        "no remote output"
    };
    RollbackFailure::new(
        ErrorCode::RollbackFailed,
        format!(
            "{action}: task={task}, exit_code={:?}, detail={detail}",
            result.exit_code
        ),
    )
}

fn ensure_environment_contract(
    environment: &EnvironmentConfig,
    reference: &RollbackReference,
) -> AppResult<()> {
    let rollback_task = environment.tasks.rollback.as_deref().ok_or_else(|| {
        AppError::new(
            ErrorCode::RollbackUnavailable,
            "rollback task is no longer configured",
        )
    })?;
    let unchanged = environment.target == reference.target()
        && environment.backup_path == reference.backup_path()
        && environment.install_path == reference.install_path()
        && rollback_task == reference.rollback_task()
        && environment.tasks.restart == reference.restart_task()
        && environment.tasks.health_check == reference.health_check_task();
    if unchanged {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::RollbackUnavailable,
            "deployment environment contract changed since the rollback reference was created",
        ))
    }
}

fn repository_error(error: RepositoryError) -> AppError {
    match error {
        RepositoryError::RollbackUnavailable(message) => {
            AppError::new(ErrorCode::RollbackUnavailable, message)
        }
        RepositoryError::MutationConflict {
            application,
            environment,
        } => AppError::new(
            ErrorCode::ConflictingDeployment,
            format!("mutation already active for {application}/{environment}"),
        ),
        other => AppError::new(ErrorCode::PersistenceFailed, other.to_string()),
    }
}
