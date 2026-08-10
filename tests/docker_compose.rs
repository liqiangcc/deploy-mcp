use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use deploy_mcp::adapters::{FakeRemoteCall, FakeRemoteExecution};
use deploy_mcp::application::{
    ContainerDeployRequest, DeploymentApi, DeploymentApplication, DeploymentExecutionResult,
};
use deploy_mcp::config::Config;
use deploy_mcp::domain::{DeploymentState, RollbackOperationState};
use deploy_mcp::error::ErrorCode;
use deploy_mcp::persistence::SqliteDeploymentRepository;
use deploy_mcp::ports::{RemoteTargetCheck, RemoteTaskResult, RollbackRepository};
use deploy_mcp::rollback_persistence::SqliteRollbackRepository;
use serde_json::Value;
use tempfile::TempDir;

const TARGET: &str = "docker-host";
const REPOSITORY: &str = "registry.example.com/demo-service";
const PROJECT: &str = "demo-project";
const SERVICE: &str = "app";
const OLD_DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NEW_DIGEST: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OTHER_DIGEST: &str =
    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

const TASKS: [&str; 6] = [
    "compose-prepare",
    "compose-current",
    "compose-apply",
    "compose-up",
    "compose-health",
    "compose-rollback",
];

type Application = DeploymentApplication<FakeRemoteExecution, SqliteDeploymentRepository>;

struct Fixture {
    _directory: TempDir,
    database_path: std::path::PathBuf,
    remote: FakeRemoteExecution,
    application: Application,
}

fn config(service: &str) -> Arc<Config> {
    Arc::new(
        Config::from_yaml(&format!(
            r#"
remote_exec:
  command: remote-exec-mcp
runtime:
  deployment_step_timeout_ms: 1000
  explicit_rollback_timeout_ms: 5000
  verification_max_attempts: 2
  verification_retry_delay_ms: 0
applications:
  demo:
    display_name: Docker Demo
    artifact_type: container_image
    environments:
      test:
        mechanism:
          type: docker_compose
          image_repository: {REPOSITORY}
          compose_project: {PROJECT}
          service: {service}
          tasks:
            prepare: compose-prepare
            capture_rollback: compose-current
            apply: compose-apply
            activate: compose-up
            health_check: compose-health
            rollback: compose-rollback
        target: {TARGET}
"#
        ))
        .unwrap(),
    )
}

fn success(stdout: &str) -> RemoteTaskResult {
    RemoteTaskResult {
        success: true,
        exit_code: Some(0),
        stdout: stdout.to_owned(),
        stderr: String::new(),
        duration_ms: 1,
        stdout_truncated: false,
        stderr_truncated: false,
    }
}

fn failure(message: &str) -> RemoteTaskResult {
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

fn configured_remote() -> FakeRemoteExecution {
    let remote = FakeRemoteExecution::default();
    remote.set_target_check(
        TARGET,
        Ok(RemoteTargetCheck {
            reachable: true,
            remote_identity: Some("docker-test-host".to_owned()),
        }),
    );
    remote.set_tasks(
        TARGET,
        Ok(TASKS
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()),
    );
    for task in TASKS {
        let stdout = if task == "compose-current" {
            OLD_DIGEST
        } else {
            ""
        };
        remote.set_task_result(TARGET, task, Ok(success(stdout)));
    }
    remote
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

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("deployments.sqlite");
    let remote = configured_remote();
    let application = application_for_database(config(SERVICE), remote.clone(), &database_path);
    Fixture {
        _directory: directory,
        database_path,
        remote,
        application,
    }
}

fn request(digest: &str, key: &str) -> ContainerDeployRequest {
    ContainerDeployRequest {
        application: "demo".to_owned(),
        environment: "test".to_owned(),
        version: "1.2.3".to_owned(),
        digest: digest.to_owned(),
        idempotency_key: Some(key.to_owned()),
    }
}

fn candidate_parameters(digest: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            "image_repository".to_owned(),
            Value::String(REPOSITORY.to_owned()),
        ),
        ("digest".to_owned(), Value::String(digest.to_owned())),
        (
            "compose_project".to_owned(),
            Value::String(PROJECT.to_owned()),
        ),
        ("service".to_owned(), Value::String(SERVICE.to_owned())),
    ])
}

fn service_parameters(service: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([
        (
            "compose_project".to_owned(),
            Value::String(PROJECT.to_owned()),
        ),
        ("service".to_owned(), Value::String(service.to_owned())),
    ])
}

async fn deploy_success(fixture: &Fixture) -> DeploymentExecutionResult {
    let result = fixture
        .application
        .deploy_container_application(request(NEW_DIGEST, "docker-release-1"))
        .await
        .unwrap();
    assert_eq!(
        result.outcome.deployment.state(),
        DeploymentState::Succeeded
    );
    assert!(result.rollback_reference_available);
    assert!(result.rollback_reference_error.is_none());
    result
}

#[tokio::test]
async fn docker_deploy_and_explicit_rollback_use_only_trusted_named_tasks() {
    let fixture = fixture();
    let result = deploy_success(&fixture).await;

    assert_eq!(
        fixture.remote.calls(),
        vec![
            FakeRemoteCall::CheckTarget {
                target: TARGET.to_owned()
            },
            FakeRemoteCall::ListTasks {
                target: TARGET.to_owned()
            },
            FakeRemoteCall::RunTask {
                target: TARGET.to_owned(),
                task: "compose-prepare".to_owned(),
                parameters: candidate_parameters(NEW_DIGEST),
            },
            FakeRemoteCall::RunTask {
                target: TARGET.to_owned(),
                task: "compose-current".to_owned(),
                parameters: service_parameters(SERVICE),
            },
            FakeRemoteCall::RunTask {
                target: TARGET.to_owned(),
                task: "compose-apply".to_owned(),
                parameters: candidate_parameters(NEW_DIGEST),
            },
            FakeRemoteCall::RunTask {
                target: TARGET.to_owned(),
                task: "compose-up".to_owned(),
                parameters: service_parameters(SERVICE),
            },
            FakeRemoteCall::RunTask {
                target: TARGET.to_owned(),
                task: "compose-health".to_owned(),
                parameters: service_parameters(SERVICE),
            },
        ]
    );

    let before = fixture.remote.calls().len();
    let rollback = fixture
        .application
        .rollback_deployment(result.outcome.deployment.id().as_str())
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
                target: TARGET.to_owned()
            },
            FakeRemoteCall::ListTasks {
                target: TARGET.to_owned()
            },
            FakeRemoteCall::RunTask {
                target: TARGET.to_owned(),
                task: "compose-rollback".to_owned(),
                parameters: candidate_parameters(OLD_DIGEST),
            },
            FakeRemoteCall::RunTask {
                target: TARGET.to_owned(),
                task: "compose-up".to_owned(),
                parameters: service_parameters(SERVICE),
            },
            FakeRemoteCall::RunTask {
                target: TARGET.to_owned(),
                task: "compose-health".to_owned(),
                parameters: service_parameters(SERVICE),
            },
        ]
    );
    assert!(!calls
        .iter()
        .any(|call| matches!(call, FakeRemoteCall::UploadFile { .. })));
}

#[tokio::test]
async fn invalid_capture_digest_fails_before_apply() {
    let fixture = fixture();
    fixture
        .remote
        .set_task_result(TARGET, "compose-current", Ok(success("latest")));

    let result = fixture
        .application
        .deploy_container_application(request(NEW_DIGEST, "invalid-capture"))
        .await
        .unwrap();
    assert_eq!(result.outcome.deployment.state(), DeploymentState::Failed);
    assert_eq!(
        result.outcome.failure.as_ref().unwrap().code,
        ErrorCode::RemoteExecutionFailed
    );
    assert!(!fixture.remote.calls().iter().any(|call| {
        matches!(call, FakeRemoteCall::RunTask { task, .. } if task == "compose-apply")
    }));
}

#[tokio::test]
async fn apply_failure_automatically_rolls_back_captured_digest() {
    let fixture = fixture();
    fixture
        .remote
        .set_task_result(TARGET, "compose-apply", Ok(failure("apply failed")));

    let result = fixture
        .application
        .deploy_container_application(request(NEW_DIGEST, "apply-failure"))
        .await
        .unwrap();
    assert_eq!(
        result.outcome.deployment.state(),
        DeploymentState::RolledBack
    );
    assert_eq!(
        result.outcome.failure.as_ref().unwrap().code,
        ErrorCode::RemoteExecutionFailed
    );
    assert!(result.outcome.rollback_failure.is_none());
    assert!(fixture.remote.calls().iter().any(|call| {
        matches!(call, FakeRemoteCall::RunTask { task, parameters, .. }
            if task == "compose-rollback" && parameters == &candidate_parameters(OLD_DIGEST))
    }));
}

#[tokio::test]
async fn same_idempotency_key_with_changed_digest_is_rejected_before_remote_work() {
    let fixture = fixture();
    deploy_success(&fixture).await;
    let calls_after_first = fixture.remote.calls().len();

    let error = fixture
        .application
        .deploy_container_application(request(OTHER_DIGEST, "docker-release-1"))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IdempotencyConflict);
    assert_eq!(fixture.remote.calls().len(), calls_after_first);
}

#[tokio::test]
async fn changed_trusted_compose_contract_blocks_explicit_rollback_before_remote_work() {
    let fixture = fixture();
    let result = deploy_success(&fixture).await;
    let calls_after_deploy = fixture.remote.calls().len();

    let changed = application_for_database(
        config("changed-service"),
        fixture.remote.clone(),
        &fixture.database_path,
    );
    let error = changed
        .rollback_deployment(result.outcome.deployment.id().as_str())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RollbackUnavailable);
    assert_eq!(fixture.remote.calls().len(), calls_after_deploy);
}
