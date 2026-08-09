use deploy_mcp::domain::{
    ApplicationId, Artifact, Deployment, DeploymentId, DeploymentState, EnvironmentId,
};
use deploy_mcp::persistence::SqliteDeploymentRepository;
use deploy_mcp::ports::{DeploymentRepository, RepositoryError};
use tempfile::tempdir;

const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn deployment(id: &str) -> Deployment {
    Deployment::new(
        DeploymentId::new(id).unwrap(),
        ApplicationId::new("demo").unwrap(),
        EnvironmentId::new("test").unwrap(),
        Artifact::new("1.0.0", 1, SHA256).unwrap(),
    )
}

#[test]
fn sqlite_rejects_same_environment_active_deployments_across_repository_instances() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("deployments.sqlite");
    let mut first = SqliteDeploymentRepository::open(&path).unwrap();
    let mut second = SqliteDeploymentRepository::open(&path).unwrap();

    let first_deployment = deployment("d1");
    first.create(&first_deployment).unwrap();

    let second_deployment = deployment("d2");
    assert_eq!(
        second.create(&second_deployment).unwrap_err(),
        RepositoryError::AlreadyExists("d2".to_owned())
    );

    first
        .persist_transition(
            first_deployment.id(),
            DeploymentState::Created,
            DeploymentState::Prechecking,
        )
        .unwrap();
    first
        .persist_transition(
            first_deployment.id(),
            DeploymentState::Prechecking,
            DeploymentState::Failed,
        )
        .unwrap();

    second.create(&second_deployment).unwrap();
}
