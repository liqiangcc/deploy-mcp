#![cfg(target_os = "linux")]

use std::env;
use std::fs;
use std::path::Path;
use std::time::Duration;

use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::child_process::TokioChildProcess;
use rmcp::ServiceExt;
use serde_json::{json, Map, Value};
use tokio::process::Command;
use tokio::time::sleep;

#[tokio::test]
async fn real_remote_exec_mcp_deploys_and_rolls_back_a_systemd_service() {
    if env::var_os("DEPLOY_MCP_REAL_E2E").is_none() {
        eprintln!("skipping real systemd E2E; set DEPLOY_MCP_REAL_E2E=1 to enable");
        return;
    }

    let remote_exec_bin = required_env("DEPLOY_MCP_E2E_REMOTE_EXEC_BIN");
    let ssh_port: u16 = required_env("DEPLOY_MCP_E2E_SSH_PORT")
        .parse()
        .expect("DEPLOY_MCP_E2E_SSH_PORT must be a u16");
    let known_hosts = required_env("DEPLOY_MCP_E2E_KNOWN_HOSTS");
    let artifact_root = required_env("DEPLOY_MCP_E2E_ARTIFACT_ROOT");
    let artifact_path = required_env("DEPLOY_MCP_E2E_ARTIFACT");
    let running_version = required_env("DEPLOY_MCP_E2E_RUNNING_VERSION");
    let remote_audit = required_env("DEPLOY_MCP_E2E_REMOTE_EXEC_AUDIT");

    let sandbox = tempfile::tempdir().expect("create E2E sandbox");
    let remote_config = sandbox.path().join("remote-exec.json");
    let deploy_config = sandbox.path().join("deploy-mcp.json");
    let database = sandbox.path().join("deployments.sqlite");

    let remote_config_json = json!({
        "runtime": {
            "max_concurrency": 4,
            "transfer_timeout_seconds": 30,
            "audit_path": remote_audit,
            "allowed_local_upload_roots": [artifact_root],
            "allowed_local_download_roots": []
        },
        "targets": {
            "local-systemd": {
                "transport": {
                    "type": "ssh",
                    "host": "127.0.0.1",
                    "port": ssh_port,
                    "user": "deploy",
                    "auth": {
                        "type": "key",
                        "secret_ref": "env:REMOTE_EXEC_SSH_KEY"
                    },
                    "host_key_policy": "strict",
                    "known_hosts_path": known_hosts,
                    "connect_timeout_seconds": 10
                },
                "policy": {
                    "allowed_tasks": [
                        "demo-precheck",
                        "demo-backup",
                        "demo-install",
                        "demo-restart",
                        "demo-health",
                        "demo-rollback"
                    ],
                    "allowed_upload_roots": ["/home/deploy/staging"],
                    "allowed_download_roots": [],
                    "max_transfer_bytes": 10485760
                }
            }
        },
        "tasks": {
            "demo-precheck": {
                "description": "Confirm the seeded live artifact exists",
                "execution": {
                    "type": "command",
                    "program": "/usr/bin/test",
                    "args": ["-r", "/home/deploy/app/demo.jar"]
                },
                "timeout_seconds": 10
            },
            "demo-backup": {
                "description": "Create the deployment rollback point",
                "parameters": {
                    "install_path": {
                        "type": "string",
                        "pattern": "^/home/deploy/app/demo[.]jar$",
                        "required": true
                    },
                    "backup_path": {
                        "type": "string",
                        "pattern": "^/home/deploy/backup/demo[.]jar$",
                        "required": true
                    }
                },
                "execution": {
                    "type": "command",
                    "program": "/usr/bin/cp",
                    "args": ["{{install_path}}", "{{backup_path}}"]
                },
                "timeout_seconds": 10
            },
            "demo-install": {
                "description": "Install the staged JAR",
                "parameters": {
                    "staging_path": {
                        "type": "string",
                        "pattern": "^/home/deploy/staging/demo[.]jar$",
                        "required": true
                    },
                    "install_path": {
                        "type": "string",
                        "pattern": "^/home/deploy/app/demo[.]jar$",
                        "required": true
                    }
                },
                "execution": {
                    "type": "command",
                    "program": "/usr/bin/cp",
                    "args": ["{{staging_path}}", "{{install_path}}"]
                },
                "timeout_seconds": 10
            },
            "demo-restart": {
                "description": "Restart only the disposable E2E systemd unit",
                "execution": {
                    "type": "command",
                    "program": "/usr/bin/sudo",
                    "args": [
                        "/usr/bin/systemctl",
                        "restart",
                        "deploy-mcp-e2e.service"
                    ]
                },
                "timeout_seconds": 15
            },
            "demo-health": {
                "description": "Require the disposable systemd unit to be active",
                "execution": {
                    "type": "command",
                    "program": "/usr/bin/systemctl",
                    "args": [
                        "is-active",
                        "--quiet",
                        "deploy-mcp-e2e.service"
                    ]
                },
                "timeout_seconds": 10
            },
            "demo-rollback": {
                "description": "Restore the deployment-bound backup",
                "parameters": {
                    "backup_path": {
                        "type": "string",
                        "pattern": "^/home/deploy/backup/demo[.]jar$",
                        "required": true
                    },
                    "install_path": {
                        "type": "string",
                        "pattern": "^/home/deploy/app/demo[.]jar$",
                        "required": true
                    }
                },
                "execution": {
                    "type": "command",
                    "program": "/usr/bin/cp",
                    "args": ["{{backup_path}}", "{{install_path}}"]
                },
                "timeout_seconds": 10
            }
        }
    });
    write_json(&remote_config, &remote_config_json);

    let deploy_config_json = json!({
        "remote_exec": {
            "command": remote_exec_bin,
            "args": ["--config", remote_config.to_string_lossy()]
        },
        "local_artifacts": {
            "allowed_roots": [artifact_root]
        },
        "runtime": {
            "deployment_step_timeout_ms": 30000,
            "explicit_rollback_timeout_ms": 30000,
            "verification_max_attempts": 5,
            "verification_retry_delay_ms": 250,
            "rollback_reference_retention_days": 30,
            "rollback_reference_cleanup_batch_size": 500
        },
        "applications": {
            "demo-service": {
                "display_name": "Disposable real-systemd E2E service",
                "artifact_type": "jar",
                "environments": {
                    "test": {
                        "target": "local-systemd",
                        "staging_path": "/home/deploy/staging/demo.jar",
                        "install_path": "/home/deploy/app/demo.jar",
                        "backup_path": "/home/deploy/backup/demo.jar",
                        "tasks": {
                            "precheck": "demo-precheck",
                            "backup": "demo-backup",
                            "install": "demo-install",
                            "restart": "demo-restart",
                            "health_check": "demo-health",
                            "rollback": "demo-rollback"
                        }
                    }
                }
            }
        }
    });
    write_json(&deploy_config, &deploy_config_json);

    let mut child = Command::new(env!("CARGO_BIN_EXE_deploy-mcp"));
    child
        .arg("--config")
        .arg(&deploy_config)
        .arg("--database")
        .arg(&database);
    let transport = TokioChildProcess::new(child).expect("spawn deploy-mcp child transport");
    let client = ().serve(transport).await.expect("initialize deploy-mcp MCP");

    let applications = call_tool(&client, "list_applications", json!({})).await;
    assert_eq!(applications["applications"][0]["id"], "demo-service");

    let deployment = call_tool(
        &client,
        "deploy_application",
        json!({
            "application": "demo-service",
            "environment": "test",
            "version": "1.0.0-e2e",
            "artifact_path": artifact_path,
            "idempotency_key": "real-systemd-e2e-v1"
        }),
    )
    .await;
    assert_eq!(deployment["deployment"]["state"], "succeeded");
    assert!(deployment["failure"].is_null());
    assert_eq!(deployment["rollback_reference_available"], true);
    let deployment_id = deployment["deployment"]["id"]
        .as_str()
        .expect("deployment id")
        .to_owned();

    wait_for_version(&running_version, "v1").await;

    let details = call_tool(
        &client,
        "get_deployment",
        json!({"deployment_id": deployment_id}),
    )
    .await;
    assert_eq!(details["deployment"]["state"], "succeeded");
    assert!(details["step_attempts"]
        .as_array()
        .expect("step attempts")
        .iter()
        .all(|attempt| attempt["status"] == "succeeded"));

    let history = call_tool(
        &client,
        "get_deployment_history",
        json!({"deployment_id": deployment_id, "limit": 200}),
    )
    .await;
    assert!(!history["events"]
        .as_array()
        .expect("history events")
        .is_empty());
    let history_text = history.to_string();
    assert!(!history_text.contains("/home/deploy/"));
    assert!(!history_text.contains("demo-install"));

    let rollback = call_tool(
        &client,
        "rollback_deployment",
        json!({"deployment_id": deployment_id}),
    )
    .await;
    assert_eq!(rollback["operation"]["state"], "succeeded");
    assert!(rollback["failure"].is_null());

    wait_for_version(&running_version, "v0").await;

    let audit = fs::read_to_string(required_env("DEPLOY_MCP_E2E_REMOTE_EXEC_AUDIT"))
        .expect("read remote-exec audit log");
    assert!(audit.contains("upload_file"));
    assert!(audit.contains("demo-install"));
    assert!(audit.contains("demo-rollback"));
    assert!(!audit.contains("REMOTE_EXEC_SSH_KEY"));
}

async fn call_tool(
    client: &RunningService<RoleClient, ()>,
    tool: &'static str,
    arguments: Value,
) -> Value {
    let arguments: Map<String, Value> = arguments
        .as_object()
        .expect("tool arguments must be an object")
        .clone();
    let result = client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await
        .unwrap_or_else(|error| panic!("{tool} transport failure: {error}"));
    let is_error = result.is_error;
    let structured = result
        .structured_content
        .unwrap_or_else(|| panic!("{tool} returned no structured content"));
    assert_ne!(is_error, Some(true), "{tool} returned MCP error: {structured}");
    structured
}

async fn wait_for_version(path: &str, expected: &str) {
    for _ in 0..80 {
        if fs::read_to_string(path)
            .ok()
            .is_some_and(|value| value.trim() == expected)
        {
            return;
        }
        sleep(Duration::from_millis(250)).await;
    }
    let actual = fs::read_to_string(path).unwrap_or_else(|_| "<missing>".to_owned());
    panic!("systemd service did not report version {expected}; actual={actual:?}");
}

fn write_json(path: &Path, value: &Value) {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("serialize E2E config"),
    )
    .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("required E2E environment variable is missing: {name}"))
}
