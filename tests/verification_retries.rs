use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use deploy_mcp::application::{DeployRequest, DeploymentApi, DeploymentApplication};
use deploy_mcp::config::Config;
use deploy_mcp::domain::{DeploymentState, DeploymentStep, RollbackOperationState};
use deploy_mcp::error::ErrorCode;
use deploy_mcp::persistence::SqliteDeploymentRepository;
use deploy_mcp::ports::{
    DeploymentRepository, RemoteExecutionPort, RemoteExecutionResult, RemoteTargetCheck,
    RemoteTaskResult, RemoteTransferResult, RollbackRepository, StepAttemptStatus,
};
use deploy_mcp::rollback_persistence::SqliteRollbackRepository;
use serde_json::Value;
use tempfile::TempDir;

const STAGING_PATH: &str = "/opt/staging/demo.jar";
const INSTALL_PATH: &str = "/opt/apps/demo/demo.jar";
const BACKUP_PATH: &str = "/opt/apps/demo/backup/demo.jar";

type Application = DeploymentApplication<SequencedRemote, SqliteDeploymentRepository>;

#[derive(Clone)]
struct SequencedRemote {
    state: Arc<Mutex<RemoteState>>,
}

struct RemoteState {
    upload_bytes: u64,
    health_results: VecDeque<RemoteExecutionResult<RemoteTaskResult>>,
    task_calls: Vec<String>,
}

impl SequencedRemote {
    fn new(upload_bytes: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(RemoteState {
                upload_bytes,
                health_results: VecDeque::new(),
                task_calls: Vec::new(),
            })),
        }
    }

    fn set_health_results(&self, results: Vec<RemoteExecutionResult<RemoteTaskResult>>) {
        self.state.lock().unwrap().health_results = results.into();
    }

    fn task_call_count(&self, task: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .task_calls
            .iter()
            .filter(|called| called.as_str() == task)
            .count()
    }
}

#[async_trait]
impl RemoteExecutionPort for SequencedRemote {
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
        _local_path: &str,
        _remote_path: &str,
        _overwrite: bool,
    ) -> RemoteExecutionResult<RemoteTransferResult> {
        Ok(RemoteTransferResult {
            bytes_transferred: self.state.lock().unwrap().upload_bytes,
        })
    }

    async fn run_task(
        &self,
        _target: &str,
        task: &str,
        _parameters: BTreeMap<String, Value>,
    ) -> RemoteExecutionResult<RemoteTaskResult> {
        let mut state = self.state.lock().unwrap();
        state.task_calls.push(task.to_owned());
        if task == "demo-health" {
            state
                .health_results
                .pop_front()
                .unwrap_or_else(|| Ok(success_task()))
        } else {
            Ok(success_task())
        }
    }
}

struct Fixture {
    _directory: TempDir,
    database_path: PathBuf,
    artifact_path: String,
    remote: SequencedRemote,
    application: Application,
}

fn config(max_attempts: u32, retry_delay_ms: u64, artifact_root: &Path) -> Arc<Config> {
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
  deployment_step_timeout_ms: 1000
  explicit_rollback_timeout_ms: 5000
  verification_max_attempts: {max_attempts}
  verification_retry_delay_ms: {retry_delay_ms}
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
    remote: SequencedRemote,
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

fn fixture(max_attempts: u32) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("deployments.sqlite");
    let artifact = directory.path().join("demo.jar");
    let bytes = b"verification-retry-test-jar";
    fs::write(&artifact, bytes).unwrap();
    let artifact_path = artifact.to_string_lossy().into_owned();
    let remote = SequencedRemote::new(bytes.len() as u64);
    let application = application_for_database(
        config(max_attempts, 0, directory.path()),
        remote.clone(),
        &database_path,
    );
    Fixture {
        _directory: directory,
        database_path,
        artifact_path,
        remote,
        application,
    }
}

fn request(artifact_path: &str, version: &str) -> DeployRequest {
    DeployRequest {
        application: "demo".to_owned(),
        environment: "test".to_owned(),
        version: version.to_owned(),
        artifact_path: artifact_path.to_owned(),
        idempotency_key: None,
    }
}

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

#[tokio::test]
async fn deployment_verification_retries_deterministically_until_success() {
    let fixture = fixture(3);
    fixture.remote.set_health_results(vec![
        Ok(failed_task("warming-1")),
        Ok(failed_task("warming-2")),
        Ok(success_task()),
    ]);

    let result = fixture
        .application
        .deploy_application(request(&fixture.artifact_path, "1.0.0"))
        .await
        .unwrap();
    assert_eq!(
        result.outcome.deployment.state(),
        DeploymentState::Succeeded
    );
    assert_eq!(fixture.remote.task_call_count("demo-health"), 3);
    assert_eq!(fixture.remote.task_call_count("demo-install"), 1);
    assert_eq!(fixture.remote.task_call_count("demo-restart"), 1);
    assert_eq!(fixture.remote.task_call_count("demo-rollback"), 0);

    let repository = SqliteDeploymentRepository::open(&fixture.database_path).unwrap();
    let attempts = repository
        .step_attempts(result.outcome.deployment.id())
        .unwrap()
        .into_iter()
        .filter(|attempt| attempt.step == DeploymentStep::Verify)
        .map(|attempt| attempt.status)
        .collect::<Vec<_>>();
    assert_eq!(
        attempts,
        vec![
            StepAttemptStatus::Failed,
            StepAttemptStatus::Failed,
            StepAttemptStatus::Succeeded,
        ]
    );
}

#[tokio::test]
async fn exhausted_deployment_verification_uses_existing_rollback_path() {
    let fixture = fixture(3);
    fixture.remote.set_health_results(vec![
        Ok(failed_task("still-warming-1")),
        Ok(failed_task("still-warming-2")),
        Ok(failed_task("still-warming-3")),
        Ok(success_task()),
    ]);

    let result = fixture
        .application
        .deploy_application(request(&fixture.artifact_path, "1.0.0"))
        .await
        .unwrap();
    assert_eq!(
        result.outcome.deployment.state(),
        DeploymentState::RolledBack
    );
    assert_eq!(
        result.outcome.failure.as_ref().unwrap().code,
        ErrorCode::VerificationFailed
    );
    assert_eq!(fixture.remote.task_call_count("demo-health"), 4);
    assert_eq!(fixture.remote.task_call_count("demo-rollback"), 1);

    let repository = SqliteDeploymentRepository::open(&fixture.database_path).unwrap();
    let attempts = repository
        .step_attempts(result.outcome.deployment.id())
        .unwrap()
        .into_iter()
        .filter(|attempt| attempt.step == DeploymentStep::Verify)
        .map(|attempt| attempt.status)
        .collect::<Vec<_>>();
    assert_eq!(
        attempts,
        vec![
            StepAttemptStatus::Failed,
            StepAttemptStatus::Failed,
            StepAttemptStatus::Failed,
            StepAttemptStatus::Succeeded,
        ]
    );
}

#[tokio::test]
async fn explicit_rollback_retries_only_final_verification() {
    let fixture = fixture(3);
    fixture.remote.set_health_results(vec![Ok(success_task())]);
    let deployment = fixture
        .application
        .deploy_application(request(&fixture.artifact_path, "1.0.0"))
        .await
        .unwrap();
    assert_eq!(
        deployment.outcome.deployment.state(),
        DeploymentState::Succeeded
    );
    let before_health = fixture.remote.task_call_count("demo-health");
    let before_restore = fixture.remote.task_call_count("demo-rollback");
    let before_restart = fixture.remote.task_call_count("demo-restart");

    fixture.remote.set_health_results(vec![
        Ok(failed_task("rollback-health-warming")),
        Ok(success_task()),
    ]);
    let rollback = fixture
        .application
        .rollback_deployment(deployment.outcome.deployment.id().as_str())
        .await
        .unwrap();
    assert_eq!(
        rollback.operation.state(),
        RollbackOperationState::Succeeded
    );
    assert!(rollback.failure.is_none());
    assert_eq!(
        fixture.remote.task_call_count("demo-health") - before_health,
        2
    );
    assert_eq!(
        fixture.remote.task_call_count("demo-rollback") - before_restore,
        1
    );
    assert_eq!(
        fixture.remote.task_call_count("demo-restart") - before_restart,
        1
    );
}

#[test]
fn verification_retry_configuration_is_bounded() {
    let zero_attempts = r#"
remote_exec:
  command: remote-exec-mcp
runtime:
  verification_max_attempts: 0
applications:
  demo:
    artifact_type: jar
    environments:
      test:
        target: test-server
        staging_path: /tmp/staging.jar
        install_path: /tmp/install.jar
        backup_path: /tmp/backup.jar
        tasks:
          backup: backup
          install: install
          restart: restart
          health_check: health
"#;
    assert_eq!(
        Config::from_yaml(zero_attempts).unwrap_err().code,
        ErrorCode::InvalidConfiguration
    );

    let too_many = zero_attempts.replace(
        "verification_max_attempts: 0",
        "verification_max_attempts: 11",
    );
    assert_eq!(
        Config::from_yaml(&too_many).unwrap_err().code,
        ErrorCode::InvalidConfiguration
    );

    let too_slow = zero_attempts
        .replace(
            "verification_max_attempts: 0",
            "verification_max_attempts: 3",
        )
        .replace(
            "runtime:\n",
            "runtime:\n  verification_retry_delay_ms: 60001\n",
        );
    assert_eq!(
        Config::from_yaml(&too_slow).unwrap_err().code,
        ErrorCode::InvalidConfiguration
    );
}
