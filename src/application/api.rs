use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{DeployRequest, DeployService, DeploymentOutcome};
use crate::config::{ArtifactType, Config};
use crate::domain::{Deployment, DeploymentId};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ports::{
    DeploymentRepository, DeploymentTransition, RemoteExecutionPort, RepositoryError,
    RepositoryResult, StepAttemptRecord,
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

#[async_trait]
pub trait DeploymentApi: Send + Sync {
    fn list_applications(&self) -> Vec<ApplicationSummary>;

    async fn deploy_application(&self, request: DeployRequest) -> AppResult<DeploymentOutcome>;

    fn get_deployment(&self, deployment_id: &str) -> AppResult<DeploymentDetails>;

    fn list_deployments(
        &self,
        application: Option<&str>,
        environment: Option<&str>,
        limit: usize,
    ) -> AppResult<Vec<DeploymentDetails>>;
}

pub struct DeploymentApplication<R, D>
where
    R: RemoteExecutionPort + ?Sized,
    D: DeploymentRepository + Send,
{
    config: Arc<Config>,
    deploy: DeployService<R, D>,
    repository: Arc<Mutex<D>>,
}

impl<R, D> DeploymentApplication<R, D>
where
    R: RemoteExecutionPort + ?Sized,
    D: DeploymentRepository + Send,
{
    pub fn new(config: Arc<Config>, remote: Arc<R>, repository: Arc<Mutex<D>>) -> Self {
        let deploy = DeployService::new(Arc::clone(&config), remote, Arc::clone(&repository));
        Self {
            config,
            deploy,
            repository,
        }
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
                },
                environments: application.environments.keys().cloned().collect(),
            })
            .collect()
    }

    async fn deploy_application(&self, request: DeployRequest) -> AppResult<DeploymentOutcome> {
        self.deploy.deploy(request).await
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
}

fn repository_error(error: RepositoryError) -> AppError {
    AppError::new(ErrorCode::PersistenceFailed, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::FakeRemoteExecution;
    use crate::domain::{ApplicationId, Artifact, EnvironmentId};
    use crate::persistence::SqliteDeploymentRepository;

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
        DeploymentApplication::new(config, Arc::new(FakeRemoteExecution::default()), repository)
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
        {
            let mut repository = application.repository.lock().unwrap();
            repository.create(&deployment).unwrap();
        }

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
}
