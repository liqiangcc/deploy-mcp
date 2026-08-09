use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use deploy_mcp::application::{DeployRequest, DeploymentApi, DeploymentApplication};
use deploy_mcp::config::Config;
use deploy_mcp::domain::{DeploymentState, DeploymentStep, RollbackReferenceState};
use deploy_mcp::error::ErrorCode;
use deploy_mcp::persistence::SqliteDeploymentRepository;
use deploy_mcp::ports::{
    RemoteExecutionPort, RemoteExecutionResult, RemoteTargetCheck, RemoteTaskResult,
    RemoteTransferResult, RollbackRepository, StepAttemptStatus,
};
use deploy_mcp::rollback_persistence::SqliteRollbackRepository;
use serde_json::Value;
use tempfile::TempDir;

const STAGING_PATH: &str = "/opt/staging/demo.jar";
const INSTALL_PATH: &str = "/opt/apps/demo/demo.jar";
const BACKUP_PATH: &str = "/opt/apps/demo/backup/demo.jar";

type Application = DeploymentApplication<ControlledRemote, SqliteDeploymentRepository>;

#[derive(Clone, Default)]
struct ControlledRemote {
    slow_upload: Arc<Mutex<bool>>,
    slow_task: Arc<Mutex<Option<String>>>,
    failed_task: Arc<Mutex<Option<String>>>,
    task_calls: Arc<Mutex<Vec<String>>>,
}

impl ControlledRemote {
    fn set_slow_upload(&self, slow: bool) {
        *self.slow_upload.lock().expect("slow upload lock poisoned") = slow;
    }

    fn set_slow_task(&self, task: Option<&str>) {
        *self.slow_task.lock().expect("slow task lock poisoned") = task.map(str::to_owned);
    }

    fn set_failed_task(&self, task: Option<&str>) {
        *self.failed_task.lock().expect("failed task lock poisoned") = task.map(str::to_owned);
    }

    fn task_calls(&self) -> Vec<String> {
        self.task_calls
            .lock()
            .expect("task call lock poisoned")
            .clone()
    }
}

#[async_trait]
impl RemoteExecutionPort for ControlledRemote {
    async fn check_target(&self, _target: &str) -> RemoteExecutionResult<RemoteTargetCheck> {
        Ok(RemoteTargetCheck {
            reachable: true,
            remote_identity: Some("test-host".to_owned()),
        })
    }

    async fn list_tasks(&self, _target: &str) -> RemoteExecutionResult<BTreeSet<String>> {
        Ok(BTreeSet::from([
            "demo-backup".to_owned(),
            "demo-install".to_owned(),
            "demo-restart".to_owned(),
            "demo-health".to_owned(),
            "demo-rollback".to_owned(),
        ]))
    }

    async fn upload_file(
        &self,
        _target: &str,
        local_path: &str,
        _remote_path: &str,
        _overwrite: bool,
    ) -> RemoteExecutionResult<RemoteTransferResult> {
        let should_sleep = *self
            .slow_upload
            .lock()
            .expect("slow upload lock poisoned");
        if should_sleep {
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
        Ok(RemoteTransferResult {
            bytes_transferred: fs::metadata(local_path)
                .expect("test artifact metadata")
                .len(),
        })
    }

    async fn run_task(
        &self,
        _target: &str,
        task: &str,
        _parameters: BTreeMap<String, Value>,
    ) -> RemoteExecutionResult<RemoteTaskResult> {
        self.task_calls
            .lock()
            .expect("task call lock poisoned")
            .push(task.to_owned());
        let should_sleep = self
            .slow_task
            .lock()
            .expect("slow task lock poisoned")
            .as_deref()
            == Some(task);
        if should_sleep {
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
        let success = self
            .failed_task
            .lock()
            .expect("failed task lock poisoned")
            .as_deref()
            != Some(task);
        Ok(RemoteTaskResult {
            success,
            exit_code: Some(if success { 0 } else { 1 }),
            stdout: String::new(),
            stderr: if success {
                String::new()
            } else {
                format!("{task} failed")
            },
            duration_ms: 1,
            stdout_truncated: false,
            stderr_truncated: false,
        })
    }
}

struct Fixture {
    _directory: TempDir,
    database_path: std::path::PathBuf,
    artifact_path: String,
    config: Arc<Config>,
    remote: ControlledRemote,
    application: Application,
}

fn config(step_timeout_ms: u64, rollback_timeout_ms: u64, artifact_root: &Path) -> Arc<Config> {
    let artifact_root = serde_json::to_string(artifact_root.to_string_lossy().as_ref()).unwrap();
    Arc::new(
        Config::from_yaml(&format!(
            r#"
remote_exec:
  command: remote-exec-mcp
local_artifacts:
  allowed_roots:
    - {artifact_root}
runtime:
  deployment_step_timeout_ms: {step_timeout_ms}
  explicit_rollback_timeout_ms: {rollback_timeout_ms}
applications:
  demo:
    artifact_type: jar
    environments:
      test:
        target: test-server
        staging_path: {STAGING_PATH}
        install_path: {INSTALL_PATH}
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
    remote: ControlledRemote,
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

fn fixture(step_timeout_ms: u64, rollback_timeout_ms: u64) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("deployments.sqlite");
    let artifact = directory.path().join("demo.jar");
    fs::write(&artifact, b"timeout-test-jar").unwrap();
    let artifact_path = artifact.to_string_lossy().into_owned();
    let config = config(step_timeout_ms, rollback_timeout_ms, directory.path());
    let remote = ControlledRemote::default();
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

fn deploy_request(version: &str, artifact_path: &str) -> DeployRequest {
    DeployRequest {
        application: "demo".to_owned(),
        environment: "test".to_owned(),
        version: version.to_owned(),
        artifact_path: artifact_path.to_owned(),
        idempotency_key: None,
    }
}

#[tokio::test]
async fn staging_timeout_keeps_durable_guard_until_recovery() {
    let fixture = fixture(10, 1_000);
    fixture.remote.set_slow_upload(true);

    let result = fixture
        .application
        .deploy_application(deploy_request("1.0.0", &fixture.artifact_path))
        .await
        .unwrap();
    assert_eq!(
        result.outcome.deployment.state(),
        DeploymentState::StagingArtifact
    );
    let deployment_id = result.outcome.deployment.id().clone();
    let failure = result.outcome.failure.as_ref().unwrap();
    assert_eq!(failure.step, DeploymentStep::StageArtifact);
    assert_eq!(failure.code, ErrorCode::OperationTimedOut);
    assert!(result.outcome.rollback_failure.is_none());
    assert!(fixture.remote.task_calls().is_empty());

    let details = fixture
        .application
        .get_deployment(deployment_id.as_str())
        .unwrap();
    let attempt = details
        .step_attempts
        .iter()
        .find(|attempt| attempt.step == DeploymentStep::StageArtifact)
        .unwrap();
    assert_eq!(attempt.status, StepAttemptStatus::Failed);
    assert!(attempt
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("operation_timed_out"));

    let second = application_for_database(
        Arc::clone(&fixture.config),
        fixture.remote.clone(),
        &fixture.database_path,
    );
    let conflict = second
        .deploy_application(deploy_request("2.0.0", &fixture.artifact_path))
        .await
        .unwrap_err();
    assert_eq!(conflict.code, ErrorCode::ConflictingDeployment);
}

#[tokio::test]
async fn backup_timeout_keeps_durable_guard_until_recovery() {
    let fixture = fixture(10, 1_000);
    fixture.remote.set_slow_task(Some("demo-backup"));

    let result = fixture
        .application
        .deploy_application(deploy_request("1.0.0", &fixture.artifact_path))
        .await
        .unwrap();
    assert_eq!(
        result.outcome.deployment.state(),
        DeploymentState::BackingUp
    );
    let deployment_id = result.outcome.deployment.id().clone();
    let failure = result.outcome.failure.as_ref().unwrap();
    assert_eq!(failure.step, DeploymentStep::BackupCurrent);
    assert_eq!(failure.code, ErrorCode::OperationTimedOut);
    assert!(result.outcome.rollback_failure.is_none());
    assert_eq!(fixture.remote.task_calls(), vec!["demo-backup"]);

    let details = fixture
        .application
        .get_deployment(deployment_id.as_str())
        .unwrap();
    let attempt = details
        .step_attempts
        .iter()
        .find(|attempt| attempt.step == DeploymentStep::BackupCurrent)
        .unwrap();
    assert_eq!(attempt.status, StepAttemptStatus::Failed);
    assert!(attempt
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("operation_timed_out"));

    let second = application_for_database(
        Arc::clone(&fixture.config),
        fixture.remote.clone(),
        &fixture.database_path,
    );
    let conflict = second
        .deploy_application(deploy_request("2.0.0", &fixture.artifact_path))
        .await
        .unwrap_err();
    assert_eq!(conflict.code, ErrorCode::ConflictingDeployment);
    assert_eq!(fixture.remote.task_calls(), vec!["demo-backup"]);
}

#[tokio::test]
async fn post_mutation_step_timeout_keeps_durable_guard_and_does_not_start_rollback() {
    let fixture = fixture(10, 1_000);
    fixture.remote.set_slow_task(Some("demo-install"));

    let result = fixture
        .application
        .deploy_application(deploy_request("1.0.0", &fixture.artifact_path))
        .await
        .unwrap();
    assert_eq!(
        result.outcome.deployment.state(),
        DeploymentState::Installing
    );
    let failure = result.outcome.failure.as_ref().unwrap();
    assert_eq!(failure.step, DeploymentStep::Install);
    assert_eq!(failure.code, ErrorCode::OperationTimedOut);
    assert!(failure.message.contains("remote completion is unknown"));
    assert!(result.outcome.rollback_failure.is_none());
    assert_eq!(
        fixture.remote.task_calls(),
        vec!["demo-backup", "demo-install"]
    );

    let details = fixture
        .application
        .get_deployment(result.outcome.deployment.id().as_str())
        .unwrap();
    assert_eq!(details.deployment.state(), DeploymentState::Installing);
    let attempt = details
        .step_attempts
        .iter()
        .find(|attempt| attempt.step == DeploymentStep::Install)
        .unwrap();
    assert_eq!(attempt.status, StepAttemptStatus::Failed);
    assert!(attempt
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("operation_timed_out"));

    let second = application_for_database(
        Arc::clone(&fixture.config),
        fixture.remote.clone(),
        &fixture.database_path,
    );
    let conflict = second
        .deploy_application(deploy_request("2.0.0", &fixture.artifact_path))
        .await
        .unwrap_err();
    assert_eq!(conflict.code, ErrorCode::ConflictingDeployment);
    assert_eq!(
        fixture.remote.task_calls(),
        vec!["demo-backup", "demo-install"]
    );
}

#[tokio::test]
async fn automatic_rollback_timeout_keeps_rolling_back_guard_and_stops_followup_tasks() {
    let fixture = fixture(10, 1_000);
    fixture.remote.set_failed_task(Some("demo-install"));
    fixture.remote.set_slow_task(Some("demo-rollback"));

    let result = fixture
        .application
        .deploy_application(deploy_request("1.0.0", &fixture.artifact_path))
        .await
        .unwrap();
    assert_eq!(
        result.outcome.deployment.state(),
        DeploymentState::RollingBack
    );
    assert_eq!(
        result.outcome.failure.as_ref().unwrap().code,
        ErrorCode::RemoteExecutionFailed
    );
    let rollback_failure = result.outcome.rollback_failure.as_ref().unwrap();
    assert_eq!(rollback_failure.code, ErrorCode::OperationTimedOut);
    assert!(rollback_failure
        .message
        .contains("remote completion is unknown"));
    assert_eq!(
        fixture.remote.task_calls(),
        vec!["demo-backup", "demo-install", "demo-rollback"]
    );

    let details = fixture
        .application
        .get_deployment(result.outcome.deployment.id().as_str())
        .unwrap();
    assert_eq!(details.deployment.state(), DeploymentState::RollingBack);
}

#[tokio::test]
async fn verification_timeout_is_not_retried_and_keeps_verifying_guard() {
    let fixture = fixture(10, 1_000);
    fixture.remote.set_slow_task(Some("demo-health"));

    let result = fixture
        .application
        .deploy_application(deploy_request("1.0.0", &fixture.artifact_path))
        .await
        .unwrap();
    assert_eq!(
        result.outcome.deployment.state(),
        DeploymentState::Verifying
    );
    assert_eq!(
        result.outcome.failure.as_ref().unwrap().code,
        ErrorCode::OperationTimedOut
    );
    assert_eq!(
        fixture
            .remote
            .task_calls()
            .iter()
            .filter(|task| task.as_str() == "demo-health")
            .count(),
        1
    );
}

#[tokio::test]
async fn explicit_rollback_timeout_fails_closed_until_recovery() {
    let fixture = fixture(1_000, 10);
    let deployed = fixture
        .application
        .deploy_application(deploy_request("1.0.0", &fixture.artifact_path))
        .await
        .unwrap();
    let deployment_id = deployed.outcome.deployment.id().clone();
    assert!(deployed.rollback_reference_available);

    fixture.remote.set_slow_task(Some("demo-rollback"));
    let error = fixture
        .application
        .rollback_deployment(deployment_id.as_str())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::OperationTimedOut);
    assert!(error.message.contains("remains STARTED"));

    let rollback_repository = SqliteRollbackRepository::open(&fixture.database_path).unwrap();
    assert_eq!(
        rollback_repository
            .get_reference(&deployment_id)
            .unwrap()
            .unwrap()
            .state(),
        RollbackReferenceState::Active
    );
    drop(rollback_repository);

    let second = application_for_database(
        Arc::clone(&fixture.config),
        fixture.remote.clone(),
        &fixture.database_path,
    );
    let conflict = second
        .deploy_application(deploy_request("2.0.0", &fixture.artifact_path))
        .await
        .unwrap_err();
    assert_eq!(conflict.code, ErrorCode::ConflictingDeployment);
}
