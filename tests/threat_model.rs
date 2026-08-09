use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use deploy_mcp::adapters::{FakeRemoteCall, FakeRemoteExecution};
use deploy_mcp::application::{DeployRequest, DeployService};
use deploy_mcp::config::Config;
use deploy_mcp::error::ErrorCode;
use deploy_mcp::mcp::{
    DeployApplicationArgs, GetDeploymentArgs, GetDeploymentHistoryArgs, ListDeploymentsArgs,
    RollbackDeploymentArgs,
};
use deploy_mcp::persistence::SqliteDeploymentRepository;
use deploy_mcp::ports::{
    DeploymentRepository, RemoteTargetCheck, RemoteTaskResult, RemoteTransferResult,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};

const STAGING_PATH: &str = "/opt/staging/demo.jar";
const INSTALL_PATH: &str = "/opt/apps/demo/demo.jar";
const BACKUP_PATH: &str = "/opt/apps/demo/backup/demo.jar";

fn config_yaml(artifact_root: &str) -> String {
    format!(
        r#"
remote_exec:
  command: remote-exec-mcp
local_artifacts:
  allowed_roots:
    - {artifact_root}
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
    )
}

fn config(artifact_root: &str) -> Arc<Config> {
    Arc::new(Config::from_yaml(&config_yaml(artifact_root)).unwrap())
}

fn config_without_local_artifact_capability() -> Arc<Config> {
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
        remote.set_task_result("test-server", task, Ok(success_task()));
    }
    remote
}

fn request(path: &str) -> DeployRequest {
    DeployRequest {
        application: "demo".to_owned(),
        environment: "test".to_owned(),
        version: "1.2.3".to_owned(),
        artifact_path: path.to_owned(),
        idempotency_key: None,
    }
}

fn assert_rejects_unknown_field<T: DeserializeOwned + std::fmt::Debug>(
    mut base: Map<String, Value>,
    field: &str,
) {
    base.insert(
        field.to_owned(),
        Value::String("attacker-controlled".to_owned()),
    );
    let error = serde_json::from_value::<T>(Value::Object(base)).unwrap_err();
    assert!(
        error.to_string().contains("unknown field"),
        "field {field} unexpectedly reached a tool schema: {error}"
    );
}

#[test]
fn ai_facing_mutation_schemas_reject_remote_capability_injection() {
    let deploy = json!({
        "application": "demo",
        "environment": "test",
        "version": "1.2.3",
        "artifact_path": "/var/lib/deploy-mcp/artifacts/demo.jar"
    })
    .as_object()
    .unwrap()
    .clone();
    for field in [
        "target",
        "remote_path",
        "staging_path",
        "install_path",
        "backup_path",
        "task",
        "service",
        "shell",
        "command",
        "argv",
        "ssh_password",
        "ssh_private_key",
    ] {
        assert_rejects_unknown_field::<DeployApplicationArgs>(deploy.clone(), field);
    }

    let rollback = json!({ "deployment_id": "deployment-1" })
        .as_object()
        .unwrap()
        .clone();
    for field in [
        "target",
        "backup_path",
        "install_path",
        "task",
        "restart_task",
        "health_check_task",
        "shell",
        "command",
        "ssh_password",
    ] {
        assert_rejects_unknown_field::<RollbackDeploymentArgs>(rollback.clone(), field);
    }
}

#[test]
fn ai_facing_read_schemas_do_not_accept_remote_execution_controls() {
    let get = json!({ "deployment_id": "deployment-1" })
        .as_object()
        .unwrap()
        .clone();
    let history = json!({ "deployment_id": "deployment-1", "limit": 100 })
        .as_object()
        .unwrap()
        .clone();
    let list = json!({ "application": "demo", "environment": "test", "limit": 50 })
        .as_object()
        .unwrap()
        .clone();

    for field in ["target", "task", "shell", "command", "ssh_password"] {
        assert_rejects_unknown_field::<GetDeploymentArgs>(get.clone(), field);
        assert_rejects_unknown_field::<GetDeploymentHistoryArgs>(history.clone(), field);
        assert_rejects_unknown_field::<ListDeploymentsArgs>(list.clone(), field);
    }
}

#[test]
fn unsafe_artifact_capability_roots_are_rejected_at_configuration_boundary() {
    for root in ["relative/artifacts", "/", "/tmp/../etc"] {
        let error = Config::from_yaml(&config_yaml(root)).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfiguration, "root={root}");
    }
}

#[tokio::test]
async fn missing_local_artifact_capability_denies_before_remote_or_durable_work() {
    let directory = tempfile::tempdir().unwrap();
    let artifact = directory.path().join("demo.jar");
    std::fs::write(&artifact, b"jar-content").unwrap();

    let remote = FakeRemoteExecution::default();
    let repository = Arc::new(Mutex::new(SqliteDeploymentRepository::in_memory().unwrap()));
    let service = DeployService::new(
        config_without_local_artifact_capability(),
        Arc::new(remote.clone()),
        Arc::clone(&repository),
    );

    let error = service
        .deploy(request(&artifact.to_string_lossy()))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ArtifactPathNotAllowed);
    assert!(remote.calls().is_empty());
    assert!(repository
        .lock()
        .unwrap()
        .list_non_terminal()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn artifact_outside_allowlist_is_rejected_before_remote_or_durable_work() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let artifact = outside.path().join("demo.jar");
    std::fs::write(&artifact, b"jar-content").unwrap();

    let root = allowed.path().to_string_lossy().into_owned();
    let remote = FakeRemoteExecution::default();
    let repository = Arc::new(Mutex::new(SqliteDeploymentRepository::in_memory().unwrap()));
    let service = DeployService::new(
        config(&root),
        Arc::new(remote.clone()),
        Arc::clone(&repository),
    );

    let error = service
        .deploy(request(&artifact.to_string_lossy()))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ArtifactPathNotAllowed);
    assert!(remote.calls().is_empty());
    assert!(repository
        .lock()
        .unwrap()
        .list_non_terminal()
        .unwrap()
        .is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_inside_allowlist_cannot_escape_before_remote_or_durable_work() {
    use std::os::unix::fs::symlink;

    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.jar");
    std::fs::write(&secret, b"outside-content").unwrap();
    let link = allowed.path().join("demo.jar");
    symlink(&secret, &link).unwrap();

    let root = allowed.path().to_string_lossy().into_owned();
    let remote = FakeRemoteExecution::default();
    let repository = Arc::new(Mutex::new(SqliteDeploymentRepository::in_memory().unwrap()));
    let service = DeployService::new(
        config(&root),
        Arc::new(remote.clone()),
        Arc::clone(&repository),
    );

    let error = service
        .deploy(request(&link.to_string_lossy()))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ArtifactPathNotAllowed);
    assert!(remote.calls().is_empty());
    assert!(repository
        .lock()
        .unwrap()
        .list_non_terminal()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn deployment_remote_calls_are_derived_only_from_configured_capabilities() {
    let directory = tempfile::tempdir().unwrap();
    let artifact = directory.path().join("caller-selected-name.jar");
    let bytes = b"jar-content";
    std::fs::write(&artifact, bytes).unwrap();
    let canonical_artifact = std::fs::canonicalize(&artifact)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let root = directory.path().to_string_lossy().into_owned();

    let remote = configured_remote(&canonical_artifact, bytes.len() as u64);
    let repository = Arc::new(Mutex::new(SqliteDeploymentRepository::in_memory().unwrap()));
    let service = DeployService::new(config(&root), Arc::new(remote.clone()), repository);

    let outcome = service
        .deploy(request(&artifact.to_string_lossy()))
        .await
        .unwrap();
    assert!(outcome.failure.is_none());

    let calls = remote.calls();
    assert!(matches!(
        &calls[0],
        FakeRemoteCall::CheckTarget { target } if target == "test-server"
    ));
    assert!(matches!(
        &calls[1],
        FakeRemoteCall::ListTasks { target } if target == "test-server"
    ));
    assert!(matches!(
        &calls[2],
        FakeRemoteCall::UploadFile {
            target,
            local_path,
            remote_path,
            overwrite: true,
        } if target == "test-server"
            && local_path == &canonical_artifact
            && remote_path == STAGING_PATH
    ));

    let task_calls = calls
        .iter()
        .filter_map(|call| match call {
            FakeRemoteCall::RunTask {
                target,
                task,
                parameters,
            } => Some((target.as_str(), task.as_str(), parameters)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        task_calls
            .iter()
            .map(|(_, task, _)| *task)
            .collect::<Vec<_>>(),
        vec!["demo-backup", "demo-install", "demo-restart", "demo-health"]
    );
    assert!(task_calls
        .iter()
        .all(|(target, _, _)| *target == "test-server"));

    for (_, _, parameters) in &task_calls {
        for forbidden in [
            "artifact_path",
            "version",
            "target",
            "task",
            "shell",
            "command",
            "ssh_password",
        ] {
            assert!(!parameters.contains_key(forbidden));
        }
    }

    assert_eq!(
        task_calls[0].2.get("install_path"),
        Some(&Value::String(INSTALL_PATH.to_owned()))
    );
    assert_eq!(
        task_calls[0].2.get("backup_path"),
        Some(&Value::String(BACKUP_PATH.to_owned()))
    );
    assert_eq!(
        task_calls[1].2.get("staging_path"),
        Some(&Value::String(STAGING_PATH.to_owned()))
    );
    assert_eq!(
        task_calls[1].2.get("install_path"),
        Some(&Value::String(INSTALL_PATH.to_owned()))
    );
    assert!(task_calls[2].2.is_empty());
    assert!(task_calls[3].2.is_empty());
}

#[tokio::test]
async fn unknown_environment_cannot_be_used_to_select_an_unconfigured_target() {
    let directory = tempfile::tempdir().unwrap();
    let artifact = directory.path().join("demo.jar");
    std::fs::write(&artifact, b"jar-content").unwrap();
    let root = directory.path().to_string_lossy().into_owned();

    let remote = FakeRemoteExecution::default();
    let repository = Arc::new(Mutex::new(SqliteDeploymentRepository::in_memory().unwrap()));
    let service = DeployService::new(config(&root), Arc::new(remote.clone()), repository);
    let mut attack = request(&artifact.to_string_lossy());
    attack.environment = "attacker-target".to_owned();

    let error = service.deploy(attack).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::UnknownEnvironment);
    assert!(remote.calls().is_empty());
}
