use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use deploy_mcp::adapters::{FakeRemoteCall, FakeRemoteExecution};
use deploy_mcp::application::{
    DeployRequest, DeploymentApi, DeploymentApplication, DeploymentExecutionResult,
};
use deploy_mcp::config::Config;
use deploy_mcp::domain::{
    ApplicationId, Artifact, Deployment, DeploymentId, EnvironmentId, RollbackOperation,
    RollbackOperationId, RollbackOperationState, RollbackReferenceState,
};
use deploy_mcp::error::ErrorCode;
use deploy_mcp::persistence::SqliteDeploymentRepository;
use deploy_mcp::ports::{
    DeploymentRepository, RemoteTargetCheck, RemoteTaskResult, RemoteTransferResult,
    RollbackRepository,
};
use deploy_mcp::rollback_persistence::SqliteRollbackRepository;
use serde_json::Value;
use tempfile::TempDir;

const STAGING_PATH: &str = "/opt/staging/demo.jar";
const INSTALL_PATH: &str = "/opt/apps/demo/demo.jar";
const BACKUP_PATH: &str = "/opt/apps/demo/backup/demo.jar";
const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

type Application = DeploymentApplication<FakeRemoteExecution, SqliteDeploymentRepository>;

struct Fixture {
    _directory: TempDir,
    database_path: PathBuf,
    artifact_path: String,
    config: Arc<Config>,
    remote: FakeRemoteExecution,
    application: Application,
}

fn config(install_path: &str) -> Arc<Config> {
    Arc::new(
        Config::from_yaml(&format!(
            r#"
remote_exec:
  command: remote-exec-mcp
applications:
  demo:
    artifact_type: jar
    environments:
      test:
        target: test-server
        staging_path: {STAGING_PATH}
        install_path: {install_path}
        backup_path: {BACKUP_PATH}
        tasks:
          backup: demo-backup
          install: demo-install
          restart: demo-restart
          health_check: demo-health
          rollback: demo-rollback
"#
        ))
        .unwrap(),
    )
}

fn application_for_database(
    config: Arc<Config>,
    remote: FakeRemoteExecution,
    database_path: &Path,
) -> Application {
    let deployments = Arc::new(Mutex::new(
        SqliteDeploymentRepository::open(database_path).unwrap(),
    ));
    let rollbacks: Arc<Mutex<Box<dyn RollbackRepository + Send>>> = Arc::new(Mutex::new(Box::new(
        SqliteRollbackRepository::open(database_path).unwrap(),
    )));
    DeploymentApplication::new(config, Arc::new(remote), deployments, rollbacks)
}

fn successful_task() -> RemoteTaskResult {
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
    let remote = FakeRemoteExecution::default();
    remote.set_target_check(
        "test-server",
        Ok(RemoteTargetCheck {
            reachable: true,
            remote_identity: Some("test-host".to_owned()),
        }),
    );
    remote.set_tasks(
        "test-server",
        Ok(BTreeSet::from([
            "demo-backup".to_owned(),
            "demo-install".to_owned(),
            "demo-restart".to_owned(),
            "demo-health".to_owned(),
            "demo-rollback".to_owned(),
        ])),
    );
    remote.set_upload_result(
        "test-server",
        artifact_path,
        STAGING_PATH,
        true,
        Ok(RemoteTransferResult {
            bytes_transferred: artifact_size,
        }),
    );
    for task in [
        "demo-backup",
        "demo-install",
        "demo-restart",
        "demo-health",
        "demo-rollback",
    ] {
        remote.set_task_result("test-server", task, Ok(successful_task()));
    }
    remote
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("deployments.sqlite");
    let artifact = directory.path().join("demo.jar");
    let bytes = b"explicit-rollback-test-jar";
    fs::write(&artifact, bytes).unwrap();
    let artifact_path = artifact.to_string_lossy().into_owned();
    let config = config(INSTALL_PATH);
    let remote = configured_remote(&artifact_path, bytes.len() as u64);
    let application = application_for_database(Arc::clone(&config), remote.clone(), &database_path);
    Fixture {
        _directory: directory,
        database_path,
        artifact_path,
        config,
        remote,
        application,
    }
}

async fn deploy_success(fixture: &Fixture) -> DeploymentExecutionResult {
    let result = fixture
        .application
        .deploy_application(DeployRequest {
            application: "demo".to_owned(),
            environment: "test".to_owned(),
            version: "1.0.0".to_owned(),
            artifact_path: fixture.artifact_path.clone(),
            idempotency_key: None,
        })
        .await
        .unwrap();
    assert!(result.rollback_reference_available);
    assert!(result.rollback_reference_error.is_none());
    result
}

#[tokio::test]
async fn explicit_rollback_uses_bound_reference_and_consumes_it_on_success() {
    let fixture = fixture();
    let deployment = deploy_success(&fixture).await;
    let deployment_id = deployment.outcome.deployment.id().clone();
    let before = fixture.remote.calls().len();

    let rollback = fixture
        .application
        .rollback_deployment(deployment_id.as_str())
        .await
        .unwrap();
    assert_eq!(
        rollback.operation.state(),
        RollbackOperationState::Succeeded
    );
    assert!(rollback.failure.is_none());

    let calls = fixture.remote.calls();
    assert_eq!(
        &calls[before..],
        &[
            FakeRemoteCall::CheckTarget {
                target: "test-server".to_owned(),
            },
            FakeRemoteCall::ListTasks {
                target: "test-server".to_owned(),
            },
            FakeRemoteCall::RunTask {
                target: "test-server".to_owned(),
                task: "demo-rollback".to_owned(),
                parameters: BTreeMap::from([
                    (
                        "backup_path".to_owned(),
                        Value::String(BACKUP_PATH.to_owned()),
                    ),
                    (
                        "install_path".to_owned(),
                        Value::String(INSTALL_PATH.to_owned()),
                    ),
                ]),
            },
            FakeRemoteCall::RunTask {
                target: "test-server".to_owned(),
                task: "demo-restart".to_owned(),
                parameters: BTreeMap::new(),
            },
            FakeRemoteCall::RunTask {
                target: "test-server".to_owned(),
                task: "demo-health".to_owned(),
                parameters: BTreeMap::new(),
            },
        ]
    );

    let repository = SqliteRollbackRepository::open(&fixture.database_path).unwrap();
    assert_eq!(
        repository
            .get_reference(&deployment_id)
            .unwrap()
            .unwrap()
            .state(),
        RollbackReferenceState::Consumed
    );
}

#[tokio::test]
async fn failed_explicit_rollback_keeps_reference_active_for_retry() {
    let fixture = fixture();
    let deployment = deploy_success(&fixture).await;
    let deployment_id = deployment.outcome.deployment.id().clone();
    fixture.remote.set_task_result(
        "test-server",
        "demo-rollback",
        Ok(failed_task("restore failed")),
    );

    let failed = fixture
        .application
        .rollback_deployment(deployment_id.as_str())
        .await
        .unwrap();
    assert_eq!(failed.operation.state(), RollbackOperationState::Failed);
    assert_eq!(failed.failure.unwrap().code, ErrorCode::RollbackFailed);

    let repository = SqliteRollbackRepository::open(&fixture.database_path).unwrap();
    assert_eq!(
        repository
            .get_reference(&deployment_id)
            .unwrap()
            .unwrap()
            .state(),
        RollbackReferenceState::Active
    );
    drop(repository);

    fixture
        .remote
        .set_task_result("test-server", "demo-rollback", Ok(successful_task()));
    let retry = fixture
        .application
        .rollback_deployment(deployment_id.as_str())
        .await
        .unwrap();
    assert_eq!(retry.operation.state(), RollbackOperationState::Succeeded);
}

#[tokio::test]
async fn newer_deployment_record_invalidates_historical_rollback_before_remote_work() {
    let fixture = fixture();
    let deployment = deploy_success(&fixture).await;
    let deployment_id = deployment.outcome.deployment.id().clone();
    let before = fixture.remote.calls().len();

    let mut repository = SqliteDeploymentRepository::open(&fixture.database_path).unwrap();
    repository
        .create(&Deployment::new(
            DeploymentId::new("newer-deployment").unwrap(),
            ApplicationId::new("demo").unwrap(),
            EnvironmentId::new("test").unwrap(),
            Artifact::new("2.0.0", 1, SHA256).unwrap(),
        ))
        .unwrap();

    let error = fixture
        .application
        .rollback_deployment(deployment_id.as_str())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RollbackUnavailable);
    assert!(error.message.contains("newer deployment"));
    assert_eq!(fixture.remote.calls().len(), before);
}

#[tokio::test]
async fn started_rollback_blocks_deploy_from_a_separate_application_instance() {
    let fixture = fixture();
    let deployment = deploy_success(&fixture).await;
    let source = &deployment.outcome.deployment;

    let mut rollbacks = SqliteRollbackRepository::open(&fixture.database_path).unwrap();
    let operation = RollbackOperation::new(
        RollbackOperationId::new("manual-operation").unwrap(),
        source.id().clone(),
        source.application().clone(),
        source.environment().clone(),
    );
    rollbacks.begin_operation(&operation).unwrap();

    let second_remote = FakeRemoteExecution::default();
    let second = application_for_database(
        Arc::clone(&fixture.config),
        second_remote.clone(),
        &fixture.database_path,
    );
    let error = second
        .deploy_application(DeployRequest {
            application: "demo".to_owned(),
            environment: "test".to_owned(),
            version: "2.0.0".to_owned(),
            artifact_path: fixture.artifact_path.clone(),
            idempotency_key: None,
        })
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConflictingDeployment);
    assert!(second_remote.calls().is_empty());
}

#[tokio::test]
async fn environment_contract_drift_rejects_rollback_before_remote_work() {
    let fixture = fixture();
    let deployment = deploy_success(&fixture).await;
    let deployment_id = deployment.outcome.deployment.id().clone();

    let changed_remote = FakeRemoteExecution::default();
    let changed = application_for_database(
        config("/opt/apps/demo/changed.jar"),
        changed_remote.clone(),
        &fixture.database_path,
    );
    let error = changed
        .rollback_deployment(deployment_id.as_str())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RollbackUnavailable);
    assert!(error.message.contains("contract changed"));
    assert!(changed_remote.calls().is_empty());
}
