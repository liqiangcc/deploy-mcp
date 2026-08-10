use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{
    ContainerDeployRequest, DeployRequest, DeployService, DeploymentLockManager, DeploymentOutcome,
    DockerComposeMechanism, JarSystemdMechanism, RollbackOutcome, RollbackRequest, RollbackService,
};
use crate::config::{ArtifactType, Config};
use crate::domain::{Deployment, DeploymentId, DeploymentMechanismKind, DeploymentState};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ports::{
    AuditEvent, AuditRepository, DeploymentRepository, DeploymentTransition, RemoteExecutionPort,
    RepositoryError, RepositoryResult, RollbackRepository, StepAttemptRecord,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationSummary {
    pub id: String,
    pub display_name: Option<String>,
    pub artifact_type: String,
    pub environments: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DeploymentDetails {
    pub deployment: Deployment,
    pub transitions: Vec<DeploymentTransition>,
    pub step_attempts: Vec<StepAttemptRecord>,
}

#[derive(Debug, Clone)]
pub struct DeploymentExecutionResult {
    pub outcome: DeploymentOutcome,
    pub rollback_reference_available: bool,
    pub rollback_reference_error: Option<String>,
}

#[async_trait]
pub trait DeploymentApi: Send + Sync {
    fn list_applications(&self) -> Vec<ApplicationSummary>;
    async fn deploy_application(
        &self,
        request: DeployRequest,
    ) -> AppResult<DeploymentExecutionResult>;
    async fn deploy_container_application(
        &self,
        request: ContainerDeployRequest,
    ) -> AppResult<DeploymentExecutionResult>;
    fn get_deployment(&self, deployment_id: &str) -> AppResult<DeploymentDetails>;
    fn list_deployments(
        &self,
        application: Option<&str>,
        environment: Option<&str>,
        limit: usize,
    ) -> AppResult<Vec<DeploymentDetails>>;
    fn get_deployment_history(
        &self,
        deployment_id: &str,
        limit: usize,
    ) -> AppResult<Vec<AuditEvent>>;
    async fn rollback_deployment(&self, deployment_id: &str) -> AppResult<RollbackOutcome>;
}

pub struct DeploymentApplication<R, D>
where
    R: RemoteExecutionPort + ?Sized,
    D: DeploymentRepository + Send,
{
    config: Arc<Config>,
    remote: Arc<R>,
    locks: DeploymentLockManager,
    repository: Arc<Mutex<D>>,
    rollback_repository: Arc<Mutex<Box<dyn RollbackRepository + Send>>>,
    audit_repository: Option<Arc<dyn AuditRepository>>,
}

impl<R, D> DeploymentApplication<R, D>
where
    R: RemoteExecutionPort + ?Sized,
    D: DeploymentRepository + Send,
{
    pub fn new(
        config: Arc<Config>,
        remote: Arc<R>,
        repository: Arc<Mutex<D>>,
        rollback_repository: Arc<Mutex<Box<dyn RollbackRepository + Send>>>,
    ) -> Self {
        Self {
            config,
            remote,
            locks: DeploymentLockManager::default(),
            repository,
            rollback_repository,
            audit_repository: None,
        }
    }

    pub fn with_audit_repository(mut self, audit_repository: Arc<dyn AuditRepository>) -> Self {
        self.audit_repository = Some(audit_repository);
        self
    }

    fn repository<T>(&self, operation: impl FnOnce(&D) -> RepositoryResult<T>) -> AppResult<T> {
        let repository = self.repository.lock().map_err(|_| {
            AppError::new(
                ErrorCode::PersistenceFailed,
                "deployment repository lock poisoned",
            )
        })?;
        operation(&*repository).map_err(repository_error)
    }

    fn repository_mut<T>(
        &self,
        operation: impl FnOnce(&mut D) -> RepositoryResult<T>,
    ) -> AppResult<T> {
        let mut repository = self.repository.lock().map_err(|_| {
            AppError::new(
                ErrorCode::PersistenceFailed,
                "deployment repository lock poisoned",
            )
        })?;
        operation(&mut *repository).map_err(repository_error)
    }

    fn rollback_repository<T>(
        &self,
        operation: impl FnOnce(&mut dyn RollbackRepository) -> RepositoryResult<T>,
    ) -> AppResult<T> {
        let mut repository = self.rollback_repository.lock().map_err(|_| {
            AppError::new(
                ErrorCode::PersistenceFailed,
                "rollback repository lock poisoned",
            )
        })?;
        operation(repository.as_mut()).map_err(repository_error)
    }

    fn audit_repository<T>(
        &self,
        operation: impl FnOnce(&dyn AuditRepository) -> RepositoryResult<T>,
    ) -> AppResult<T> {
        let repository = self.audit_repository.as_deref().ok_or_else(|| {
            AppError::new(
                ErrorCode::PersistenceFailed,
                "structured audit repository is not configured",
            )
        })?;
        operation(repository).map_err(repository_error)
    }

    fn details(&self, deployment: Deployment) -> AppResult<DeploymentDetails> {
        let id = deployment.id().clone();
        self.repository(|repository| {
            Ok(DeploymentDetails {
                deployment,
                transitions: repository.transitions(&id)?,
                step_attempts: repository.step_attempts(&id)?,
            })
        })
    }

    fn validate_filter(
        &self,
        application: Option<&str>,
        environment: Option<&str>,
        limit: usize,
    ) -> AppResult<()> {
        if limit == 0 || limit > 200 {
            return Err(AppError::new(
                ErrorCode::InvalidRequest,
                "list_deployments limit must be between 1 and 200",
            ));
        }
        match (application, environment) {
            (None, Some(_)) => Err(AppError::new(
                ErrorCode::InvalidRequest,
                "environment filter requires an application filter",
            )),
            (Some(application), Some(environment)) => {
                self.config.environment(application, environment)?;
                Ok(())
            }
            (Some(application), None) => {
                self.config.application(application)?;
                Ok(())
            }
            (None, None) => Ok(()),
        }
    }

    fn promote_rollback_capture(&self, deployment_id: &DeploymentId) -> AppResult<bool> {
        if self
            .rollback_repository(|repository| repository.get_reference(deployment_id))?
            .is_some()
        {
            // Any stale pending capture can be safely removed once explicit rollback
            // authority is already durable.
            self.repository_mut(|repository| repository.clear_rollback_capture(deployment_id))?;
            return Ok(true);
        }

        let Some(capture) =
            self.repository(|repository| repository.get_rollback_capture(deployment_id))?
        else {
            return Ok(false);
        };
        self.rollback_repository(|repository| repository.record_reference(&capture))?;
        self.repository_mut(|repository| repository.clear_rollback_capture(deployment_id))?;
        Ok(true)
    }

    fn record_rollback_reference(&self, outcome: &DeploymentOutcome) -> (bool, Option<String>) {
        if !matches!(
            outcome.deployment.state(),
            DeploymentState::Succeeded | DeploymentState::RollbackFailed
        ) {
            return (false, None);
        }

        if let Some(reference) = &outcome.rollback_reference {
            if let Err(error) =
                self.rollback_repository(|repository| repository.record_reference(reference))
            {
                // Keep the pending capture when promotion fails so the dynamic
                // rollback point is not lost.
                return (false, Some(error.to_string()));
            }
            return match self.repository_mut(|repository| {
                repository.clear_rollback_capture(outcome.deployment.id())
            }) {
                Ok(()) => (true, None),
                Err(error) => (true, Some(error.to_string())),
            };
        }

        // This also closes the crash window between persisting SUCCEEDED and
        // promoting the already-captured rollback point into explicit authority.
        match self.promote_rollback_capture(outcome.deployment.id()) {
            Ok(true) => (true, None),
            Ok(false) if outcome.idempotent_replay => (false, None),
            Ok(false) => (
                false,
                Some(
                    "rollback_unavailable: no deployment-bound rollback point was captured"
                        .to_owned(),
                ),
            ),
            Err(error) => (false, Some(error.to_string())),
        }
    }
}

#[async_trait]
impl<R, D> DeploymentApi for DeploymentApplication<R, D>
where
    R: RemoteExecutionPort + ?Sized + 'static,
    D: DeploymentRepository + Send + 'static,
{
    fn list_applications(&self) -> Vec<ApplicationSummary> {
        self.config
            .applications
            .iter()
            .map(|(id, application)| ApplicationSummary {
                id: id.clone(),
                display_name: application.display_name.clone(),
                artifact_type: match application.artifact_type {
                    ArtifactType::Jar => "jar".to_owned(),
                    ArtifactType::ContainerImage => "container_image".to_owned(),
                },
                environments: application.environments.keys().cloned().collect(),
            })
            .collect()
    }

    async fn deploy_application(
        &self,
        request: DeployRequest,
    ) -> AppResult<DeploymentExecutionResult> {
        let deploy = DeployService::with_mechanism(
            Arc::clone(&self.config),
            Arc::new(JarSystemdMechanism::new(Arc::clone(&self.remote))),
            Arc::clone(&self.repository),
        )
        .with_lock_manager(self.locks.clone());
        let outcome = deploy.deploy(request).await?;
        let (rollback_reference_available, rollback_reference_error) =
            self.record_rollback_reference(&outcome);
        Ok(DeploymentExecutionResult {
            outcome,
            rollback_reference_available,
            rollback_reference_error,
        })
    }

    async fn deploy_container_application(
        &self,
        request: ContainerDeployRequest,
    ) -> AppResult<DeploymentExecutionResult> {
        let deploy = DeployService::with_mechanism(
            Arc::clone(&self.config),
            Arc::new(DockerComposeMechanism::new(Arc::clone(&self.remote))),
            Arc::clone(&self.repository),
        )
        .with_lock_manager(self.locks.clone());
        let outcome = deploy.deploy_container(request).await?;
        let (rollback_reference_available, rollback_reference_error) =
            self.record_rollback_reference(&outcome);
        Ok(DeploymentExecutionResult {
            outcome,
            rollback_reference_available,
            rollback_reference_error,
        })
    }

    fn get_deployment(&self, deployment_id: &str) -> AppResult<DeploymentDetails> {
        let id = DeploymentId::new(deployment_id.to_owned()).map_err(|_| {
            AppError::new(
                ErrorCode::UnknownDeployment,
                format!("unknown deployment: {deployment_id}"),
            )
        })?;
        let deployment = self
            .repository(|repository| repository.get(&id))?
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::UnknownDeployment,
                    format!("unknown deployment: {deployment_id}"),
                )
            })?;
        self.details(deployment)
    }

    fn list_deployments(
        &self,
        application: Option<&str>,
        environment: Option<&str>,
        limit: usize,
    ) -> AppResult<Vec<DeploymentDetails>> {
        self.validate_filter(application, environment, limit)?;
        let deployments =
            self.repository(|repository| repository.list(application, environment, limit))?;
        deployments
            .into_iter()
            .map(|deployment| self.details(deployment))
            .collect()
    }

    fn get_deployment_history(
        &self,
        deployment_id: &str,
        limit: usize,
    ) -> AppResult<Vec<AuditEvent>> {
        if limit == 0 || limit > 500 {
            return Err(AppError::new(
                ErrorCode::InvalidRequest,
                "get_deployment_history limit must be between 1 and 500",
            ));
        }
        let id = DeploymentId::new(deployment_id.to_owned()).map_err(|_| {
            AppError::new(
                ErrorCode::UnknownDeployment,
                format!("unknown deployment: {deployment_id}"),
            )
        })?;
        if self.repository(|repository| repository.get(&id))?.is_none() {
            return Err(AppError::new(
                ErrorCode::UnknownDeployment,
                format!("unknown deployment: {deployment_id}"),
            ));
        }
        self.audit_repository(|repository| repository.events_for_deployment(&id, limit))
    }

    async fn rollback_deployment(&self, deployment_id: &str) -> AppResult<RollbackOutcome> {
        let id = DeploymentId::new(deployment_id.to_owned()).map_err(|_| {
            AppError::new(
                ErrorCode::UnknownDeployment,
                format!("unknown deployment: {deployment_id}"),
            )
        })?;
        let deployment = self
            .repository(|repository| repository.get(&id))?
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::UnknownDeployment,
                    format!("unknown deployment: {deployment_id}"),
                )
            })?;
        if matches!(
            deployment.state(),
            DeploymentState::Succeeded | DeploymentState::RollbackFailed
        ) {
            // If the process crashed after terminal deployment persistence but
            // before rollback-reference promotion, explicit rollback can recover
            // the pending capture without guessing any remote state.
            let _ = self.promote_rollback_capture(&id)?;
        }
        let request = RollbackRequest {
            deployment_id: deployment_id.to_owned(),
        };
        match deployment.mechanism_kind() {
            DeploymentMechanismKind::JarSystemd => {
                RollbackService::with_mechanism(
                    Arc::clone(&self.config),
                    Arc::new(JarSystemdMechanism::new(Arc::clone(&self.remote))),
                    Arc::clone(&self.repository),
                    Arc::clone(&self.rollback_repository),
                    self.locks.clone(),
                )
                .rollback(request)
                .await
            }
            DeploymentMechanismKind::DockerCompose => {
                RollbackService::with_mechanism(
                    Arc::clone(&self.config),
                    Arc::new(DockerComposeMechanism::new(Arc::clone(&self.remote))),
                    Arc::clone(&self.repository),
                    Arc::clone(&self.rollback_repository),
                    self.locks.clone(),
                )
                .rollback(request)
                .await
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::FakeRemoteExecution;
    use crate::domain::{ApplicationId, Artifact, EnvironmentId};
    use crate::persistence::SqliteDeploymentRepository;
    use crate::rollback_persistence::SqliteRollbackRepository;

    const CONFIG: &str = r#"
remote_exec:
  command: remote-exec-mcp
applications:
  demo:
    display_name: Demo Service
    artifact_type: jar
    environments:
      test:
        target: test-server
        staging_path: /opt/staging/demo.jar
        install_path: /opt/apps/demo/demo.jar
        backup_path: /opt/apps/demo/backup/demo.jar
        tasks:
          backup: demo-backup
          install: demo-install
          restart: demo-restart
          health_check: demo-health
          rollback: demo-rollback
"#;
    const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn application() -> DeploymentApplication<FakeRemoteExecution, SqliteDeploymentRepository> {
        let config = Arc::new(Config::from_yaml(CONFIG).unwrap());
        let repository = Arc::new(Mutex::new(SqliteDeploymentRepository::in_memory().unwrap()));
        let rollback_repository: Arc<Mutex<Box<dyn RollbackRepository + Send>>> = Arc::new(
            Mutex::new(Box::new(SqliteRollbackRepository::in_memory().unwrap())),
        );
        DeploymentApplication::new(
            config,
            Arc::new(FakeRemoteExecution::default()),
            repository,
            rollback_repository,
        )
    }

    #[test]
    fn lists_configured_applications_without_remote_access() {
        let application = application();
        assert_eq!(
            application.list_applications(),
            vec![ApplicationSummary {
                id: "demo".to_owned(),
                display_name: Some("Demo Service".to_owned()),
                artifact_type: "jar".to_owned(),
                environments: vec!["test".to_owned()],
            }]
        );
    }

    #[test]
    fn queries_deployment_with_history_through_repository_port() {
        let application = application();
        let deployment = Deployment::new(
            DeploymentId::new("d1").unwrap(),
            ApplicationId::new("demo").unwrap(),
            EnvironmentId::new("test").unwrap(),
            Artifact::new("1.0.0", 42, SHA256).unwrap(),
        );
        application
            .repository
            .lock()
            .unwrap()
            .create(&deployment)
            .unwrap();
        let details = application.get_deployment("d1").unwrap();
        assert_eq!(details.deployment.id().as_str(), "d1");
        assert!(details.transitions.is_empty());
        assert!(details.step_attempts.is_empty());
        assert_eq!(
            application
                .list_deployments(Some("demo"), Some("test"), 50)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn environment_filter_requires_application_and_limit_is_bounded() {
        let application = application();
        assert_eq!(
            application
                .list_deployments(None, Some("test"), 50)
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            application
                .list_deployments(None, None, 0)
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn history_query_is_bounded_and_requires_a_configured_projection() {
        let application = application();
        let deployment = Deployment::new(
            DeploymentId::new("d-history").unwrap(),
            ApplicationId::new("demo").unwrap(),
            EnvironmentId::new("test").unwrap(),
            Artifact::new("1.0.0", 42, SHA256).unwrap(),
        );
        application
            .repository
            .lock()
            .unwrap()
            .create(&deployment)
            .unwrap();
        assert_eq!(
            application
                .get_deployment_history("d-history", 0)
                .unwrap_err()
                .code,
            ErrorCode::InvalidRequest
        );
        assert_eq!(
            application
                .get_deployment_history("d-history", 100)
                .unwrap_err()
                .code,
            ErrorCode::PersistenceFailed
        );
    }
}
