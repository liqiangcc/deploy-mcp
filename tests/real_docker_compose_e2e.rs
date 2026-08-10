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
async fn real_remote_exec_mcp_deploys_and_rolls_back_docker_compose_by_digest() {
    if env::var_os("DEPLOY_MCP_REAL_DOCKER_E2E").is_none() {
        eprintln!("skipping real Docker Compose E2E; set DEPLOY_MCP_REAL_DOCKER_E2E=1 to enable");
        return;
    }

    let remote_exec_bin = required_env("DEPLOY_MCP_E2E_REMOTE_EXEC_BIN");
    let ssh_port: u16 = required_env("DEPLOY_MCP_DOCKER_E2E_SSH_PORT")
        .parse()
        .expect("DEPLOY_MCP_DOCKER_E2E_SSH_PORT must be a u16");
    let known_hosts = required_env("DEPLOY_MCP_DOCKER_E2E_KNOWN_HOSTS");
    let repository = required_env("DEPLOY_MCP_DOCKER_E2E_REPOSITORY");
    let digest_v0 = required_env("DEPLOY_MCP_DOCKER_E2E_DIGEST_V0");
    let digest_v1 = required_env("DEPLOY_MCP_DOCKER_E2E_DIGEST_V1");
    let running_version = required_env("DEPLOY_MCP_DOCKER_E2E_RUNNING_VERSION");
    let remote_audit = required_env("DEPLOY_MCP_DOCKER_E2E_REMOTE_EXEC_AUDIT");

    assert_ne!(digest_v0, digest_v1);
    assert_digest(&digest_v0);
    assert_digest(&digest_v1);

    let sandbox = tempfile::tempdir().expect("create Docker E2E sandbox");
    let remote_config = sandbox.path().join("remote-exec.json");
    let deploy_config = sandbox.path().join("deploy-mcp.json");
    let database = sandbox.path().join("deployments.sqlite");

    let exact_repository_pattern = regex_escape(&repository);
    let remote_config_json = json!({
        "runtime": {
            "max_concurrency": 4,
            "transfer_timeout_seconds": 30,
            "audit_path": remote_audit,
            "allowed_local_upload_roots": [],
            "allowed_local_download_roots": []
        },
        "targets": {
            "local-docker": {
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
                        "compose-precheck",
                        "compose-prepare",
                        "compose-current",
                        "compose-apply",
                        "compose-up",
                        "compose-health",
                        "compose-rollback"
                    ],
                    "allowed_upload_roots": [],
                    "allowed_download_roots": [],
                    "max_transfer_bytes": 1048576
                }
            }
        },
        "tasks": {
            "compose-precheck": service_task(
                "Validate only the configured disposable Compose project",
                "/home/deploy/e2e-bin/compose-precheck"
            ),
            "compose-prepare": candidate_task(
                "Pull only the configured immutable candidate image",
                "/home/deploy/e2e-bin/compose-prepare",
                &exact_repository_pattern
            ),
            "compose-current": service_task(
                "Read only the current immutable digest for rollback capture",
                "/home/deploy/e2e-bin/compose-current"
            ),
            "compose-apply": candidate_task(
                "Set only the configured service candidate image",
                "/home/deploy/e2e-bin/compose-apply",
                &exact_repository_pattern
            ),
            "compose-up": service_task(
                "Activate only the configured disposable Compose service",
                "/home/deploy/e2e-bin/compose-up"
            ),
            "compose-health": service_task(
                "Verify only the configured disposable Compose service",
                "/home/deploy/e2e-bin/compose-health"
            ),
            "compose-rollback": candidate_task(
                "Restore only the deployment-bound immutable image digest",
                "/home/deploy/e2e-bin/compose-rollback",
                &exact_repository_pattern
            )
        }
    });
    write_json(&remote_config, &remote_config_json);

    let deploy_config_json = json!({
        "remote_exec": {
            "command": remote_exec_bin,
            "args": ["--config", remote_config.to_string_lossy()]
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
                "display_name": "Disposable real-Docker E2E service",
                "artifact_type": "container_image",
                "environments": {
                    "test": {
                        "mechanism": {
                            "type": "docker_compose",
                            "image_repository": repository,
                            "compose_project": "deploy-mcp-e2e",
                            "service": "app",
                            "tasks": {
                                "precheck": "compose-precheck",
                                "prepare": "compose-prepare",
                                "capture_rollback": "compose-current",
                                "apply": "compose-apply",
                                "activate": "compose-up",
                                "health_check": "compose-health",
                                "rollback": "compose-rollback"
                            }
                        },
                        "target": "local-docker"
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
    assert_eq!(applications["applications"][0]["artifact_type"], "container_image");

    let deployment = call_tool(
        &client,
        "deploy_application",
        json!({
            "application": "demo-service",
            "environment": "test",
            "version": "1.0.0-e2e",
            "release": {
                "type": "container_image",
                "digest": digest_v1
            },
            "idempotency_key": "real-docker-compose-e2e-v1"
        }),
    )
    .await;
    assert_eq!(deployment["deployment"]["state"], "succeeded");
    assert_eq!(deployment["deployment"]["mechanism"], "docker_compose");
    assert_eq!(deployment["deployment"]["release"]["type"], "container_image");
    assert_eq!(deployment["deployment"]["release"]["repository"], repository);
    assert_eq!(deployment["deployment"]["release"]["digest"], digest_v1);
    assert!(deployment["failure"].is_null());
    assert_eq!(deployment["rollback_reference_available"], true);
    let deployment_id = deployment["deployment"]["id"]
        .as_str()
        .expect("deployment id")
        .to_owned();

    wait_for_version(&running_version, "v1").await;

    let history = call_tool(
        &client,
        "get_deployment_history",
        json!({"deployment_id": deployment_id, "limit": 200}),
    )
    .await;
    let history_text = history.to_string();
    assert!(!history_text.contains("/home/deploy/"));
    assert!(!history_text.contains("compose-apply"));
    assert!(!history_text.contains("compose-rollback"));

    let rollback = call_tool(
        &client,
        "rollback_deployment",
        json!({"deployment_id": deployment_id}),
    )
    .await;
    assert_eq!(rollback["operation"]["state"], "succeeded");
    assert!(rollback["failure"].is_null());

    wait_for_version(&running_version, "v0").await;

    let audit = fs::read_to_string(required_env("DEPLOY_MCP_DOCKER_E2E_REMOTE_EXEC_AUDIT"))
        .expect("read remote-exec audit log");
    for task in [
        "compose-prepare",
        "compose-current",
        "compose-apply",
        "compose-up",
        "compose-health",
        "compose-rollback",
    ] {
        assert!(audit.contains(task), "remote-exec audit did not contain {task}");
    }
    assert!(!audit.contains("upload_file"));
    assert!(!audit.contains("REMOTE_EXEC_SSH_KEY"));
}

fn service_task(description: &str, program: &str) -> Value {
    json!({
        "description": description,
        "parameters": {
            "compose_project": {
                "type": "string",
                "pattern": "^deploy-mcp-e2e$",
                "required": true
            },
            "service": {
                "type": "string",
                "pattern": "^app$",
                "required": true
            }
        },
        "execution": {
            "type": "command",
            "program": program,
            "args": ["{{compose_project}}", "{{service}}"]
        },
        "timeout_seconds": 30
    })
}

fn candidate_task(description: &str, program: &str, repository_pattern: &str) -> Value {
    json!({
        "description": description,
        "parameters": {
            "image_repository": {
                "type": "string",
                "pattern": format!("^{repository_pattern}$"),
                "required": true
            },
            "digest": {
                "type": "string",
                "pattern": "^sha256:[0-9a-f]{64}$",
                "required": true
            },
            "compose_project": {
                "type": "string",
                "pattern": "^deploy-mcp-e2e$",
                "required": true
            },
            "service": {
                "type": "string",
                "pattern": "^app$",
                "required": true
            }
        },
        "execution": {
            "type": "command",
            "program": program,
            "args": [
                "{{image_repository}}",
                "{{digest}}",
                "{{compose_project}}",
                "{{service}}"
            ]
        },
        "timeout_seconds": 30
    })
}

fn regex_escape(value: &str) -> String {
    let mut escaped = String::new();
    for character in value.chars() {
        if matches!(
            character,
            '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn assert_digest(value: &str) {
    let hex = value.strip_prefix("sha256:").expect("digest prefix");
    assert_eq!(hex.len(), 64);
    assert!(hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
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
    assert_ne!(
        is_error,
        Some(true),
        "{tool} returned MCP error: {structured}"
    );
    structured
}

async fn wait_for_version(path: &str, expected: &str) {
    for _ in 0..100 {
        if fs::read_to_string(path)
            .ok()
            .is_some_and(|value| value.trim() == expected)
        {
            return;
        }
        sleep(Duration::from_millis(250)).await;
    }
    let actual = fs::read_to_string(path).unwrap_or_else(|_| "<missing>".to_owned());
    panic!("Docker Compose service did not report version {expected}; actual={actual:?}");
}

fn write_json(path: &Path, value: &Value) {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("serialize E2E config"),
    )
    .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn required_env(name: &str) -> String {
    env::var(name)
        .unwrap_or_else(|_| panic!("required E2E environment variable is missing: {name}"))
}
