use std::path::Path;
use std::sync::{Arc, Mutex};

use deploy_mcp::adapters::FakeRemoteExecution;
use deploy_mcp::application::{DeployRequest, DeployService};
use deploy_mcp::config::Config;
use deploy_mcp::error::ErrorCode;
use deploy_mcp::persistence::SqliteDeploymentRepository;
use deploy_mcp::ports::{RemoteTargetCheck, RemoteTaskResult, RemoteTransferResult};

const CONFIG: &str = r#"
remote_exec:
  command: remote-exec-mcp
local_artifacts:
  allowed_roots:
    - __LOCAL_ARTIFACT_ROOT__
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
          backup: demo-backup
          install: demo-install
          restart: demo-restart
          health_check: demo-health
          rollback: demo-rollback
"#;

fn config(root: &Path) -> Arc<Config> {
    let root = serde_json::to_string(root.to_string_lossy().as_ref()).unwrap();
    Arc::new(Config::from_yaml(&CONFIG.replace("__LOCAL_ARTIFACT_ROOT__", &root)).unwrap())
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

fn configured_remote(artifact_path: &str, size: u64) -> FakeRemoteExecution {
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
    let canonical_artifact = std::fs::canonicalize(artifact_path)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    fake.set_upload_result(
        "test-server",
        &canonical_artifact,
        "/opt/staging/demo.jar",
        true,
        Ok(RemoteTransferResult {
            bytes_transferred: size,
        }),
    );
    for task in [
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

fn request(path: &str, version: &str, key: &str) -> DeployRequest {
    DeployRequest {
        application: "demo".to_owned(),
        environment: "test".to_owned(),
        version: version.to_owned(),
        artifact_path: path.to_owned(),
        idempotency_key: Some(key.to_owned()),
    }
}

#[tokio::test]
async fn exact_idempotent_replay_returns_same_deployment_without_remote_work() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("deployments.sqlite");
    let artifact = directory.path().join("demo.jar");
    std::fs::write(&artifact, b"jar-content").unwrap();
    let artifact = artifact.to_string_lossy().into_owned();

    let fake = configured_remote(&artifact, 11);
    let repository = Arc::new(Mutex::new(
        SqliteDeploymentRepository::open(&database).unwrap(),
    ));
    let service = DeployService::new(config(directory.path()), Arc::new(fake.clone()), repository);

    let first = service
        .deploy(request(&artifact, "1.2.3", "release-123"))
        .await
        .unwrap();
    assert!(!first.idempotent_replay);
    let first_id = first.deployment.id().as_str().to_owned();
    let calls_after_first = fake.calls().len();

    let replay = service
        .deploy(request(&artifact, "1.2.3", "release-123"))
        .await
        .unwrap();
    assert!(replay.idempotent_replay);
    assert_eq!(replay.deployment.id().as_str(), first_id);
    assert_eq!(fake.calls().len(), calls_after_first);
}

#[tokio::test]
async fn same_key_with_changed_intent_is_rejected_before_remote_work() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("deployments.sqlite");
    let artifact = directory.path().join("demo.jar");
    std::fs::write(&artifact, b"jar-content").unwrap();
    let artifact = artifact.to_string_lossy().into_owned();

    let fake = configured_remote(&artifact, 11);
    let repository = Arc::new(Mutex::new(
        SqliteDeploymentRepository::open(&database).unwrap(),
    ));
    let service = DeployService::new(config(directory.path()), Arc::new(fake.clone()), repository);

    service
        .deploy(request(&artifact, "1.2.3", "release-123"))
        .await
        .unwrap();
    let calls_after_first = fake.calls().len();

    let error = service
        .deploy(request(&artifact, "1.2.4", "release-123"))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IdempotencyConflict);
    assert_eq!(fake.calls().len(), calls_after_first);
}

#[tokio::test]
async fn same_version_with_changed_artifact_is_rejected_before_remote_work() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("deployments.sqlite");
    let first_artifact = directory.path().join("demo-a.jar");
    let changed_artifact = directory.path().join("demo-b.jar");
    std::fs::write(&first_artifact, b"jar-content-a").unwrap();
    std::fs::write(&changed_artifact, b"jar-content-b").unwrap();
    let first_artifact = first_artifact.to_string_lossy().into_owned();
    let changed_artifact = changed_artifact.to_string_lossy().into_owned();

    let fake = configured_remote(&first_artifact, 13);
    let repository = Arc::new(Mutex::new(
        SqliteDeploymentRepository::open(&database).unwrap(),
    ));
    let service = DeployService::new(config(directory.path()), Arc::new(fake.clone()), repository);

    service
        .deploy(request(&first_artifact, "1.2.3", "release-a"))
        .await
        .unwrap();
    let calls_after_first = fake.calls().len();

    let error = service
        .deploy(request(&changed_artifact, "1.2.3", "release-b"))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ArtifactVersionConflict);
    assert_eq!(fake.calls().len(), calls_after_first);
}
