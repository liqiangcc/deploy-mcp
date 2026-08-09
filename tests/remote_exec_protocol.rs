#![cfg(feature = "protocol-fixture")]

use std::collections::BTreeMap;

use deploy_mcp::adapters::RemoteExecMcpAdapter;
use deploy_mcp::ports::{RemoteExecutionError, RemoteExecutionPort};
use serde_json::json;

fn fixture_binary() -> &'static str {
    env!("CARGO_BIN_EXE_remote-exec-protocol-fixture")
}

async fn spawn_fixture(args: &[&str]) -> RemoteExecMcpAdapter {
    RemoteExecMcpAdapter::spawn(
        fixture_binary(),
        &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
    )
    .await
    .expect("protocol fixture should initialize over MCP stdio")
}

#[tokio::test]
async fn remote_exec_adapter_round_trips_over_real_stdio_child_process() {
    let adapter = spawn_fixture(&[]).await;

    let target = adapter.check_target("test-server").await.unwrap();
    assert!(target.reachable);
    assert_eq!(target.remote_identity.as_deref(), Some("fixture-host"));

    let tasks = adapter.list_tasks("test-server").await.unwrap();
    for task in [
        "demo-backup",
        "demo-install",
        "demo-restart",
        "demo-health",
        "demo-rollback",
    ] {
        assert!(tasks.contains(task));
    }

    let directory = tempfile::tempdir().unwrap();
    let artifact = directory.path().join("demo.jar");
    let bytes = b"jar-content";
    std::fs::write(&artifact, bytes).unwrap();
    let transfer = adapter
        .upload_file(
            "test-server",
            &artifact.to_string_lossy(),
            "/opt/staging/demo.jar",
            true,
        )
        .await
        .unwrap();
    assert_eq!(transfer.bytes_transferred, bytes.len() as u64);

    let result = adapter
        .run_task(
            "test-server",
            "demo-install",
            BTreeMap::from([
                ("staging_path".to_owned(), json!("/opt/staging/demo.jar")),
                ("install_path".to_owned(), json!("/opt/apps/demo/demo.jar")),
            ]),
        )
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout, "demo-install:2");
}

#[tokio::test]
async fn remote_exec_adapter_preserves_structured_error_over_stdio() {
    let adapter = spawn_fixture(&[]).await;
    let error = adapter.check_target("missing-target").await.unwrap_err();
    assert_eq!(
        error,
        RemoteExecutionError::Remote {
            code: "unknown_target".to_owned(),
            message: "fixture target is not configured".to_owned(),
        }
    );
}

#[tokio::test]
async fn remote_exec_adapter_classifies_malformed_success_over_stdio() {
    let adapter = spawn_fixture(&["--malformed-upload"]).await;
    let directory = tempfile::tempdir().unwrap();
    let artifact = directory.path().join("demo.jar");
    std::fs::write(&artifact, b"jar-content").unwrap();

    let error = adapter
        .upload_file(
            "test-server",
            &artifact.to_string_lossy(),
            "/opt/staging/demo.jar",
            true,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        RemoteExecutionError::InvalidResponse { ref tool, .. } if tool == "upload_file"
    ));
}
