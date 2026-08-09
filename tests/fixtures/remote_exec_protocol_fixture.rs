use std::collections::BTreeMap;

use anyhow::Result;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{schemars, tool, tool_router, transport::stdio, ServiceExt};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Clone, Copy)]
enum FixtureMode {
    Normal,
    MalformedUpload,
}

#[derive(Clone)]
struct ProtocolFixture {
    mode: FixtureMode,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TargetArgs {
    target: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UploadFileArgs {
    target: String,
    local_path: String,
    remote_path: String,
    #[serde(default)]
    overwrite: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RunTaskArgs {
    target: String,
    task: String,
    #[serde(default)]
    parameters: BTreeMap<String, Value>,
}

#[tool_router(server_handler)]
impl ProtocolFixture {
    #[tool]
    async fn check_target(&self, Parameters(args): Parameters<TargetArgs>) -> CallToolResult {
        if args.target != "test-server" {
            return fixture_error("unknown_target", "fixture target is not configured");
        }
        CallToolResult::structured(json!({
            "reachable": true,
            "remote_identity": "fixture-host"
        }))
    }

    #[tool]
    async fn list_tasks(&self, Parameters(args): Parameters<TargetArgs>) -> CallToolResult {
        if args.target != "test-server" {
            return fixture_error("unknown_target", "fixture target is not configured");
        }
        CallToolResult::structured(json!({
            "tasks": [
                {"name": "demo-backup", "description": null, "parameters": {}, "timeout_seconds": 30},
                {"name": "demo-install", "description": null, "parameters": {}, "timeout_seconds": 30},
                {"name": "demo-restart", "description": null, "parameters": {}, "timeout_seconds": 30},
                {"name": "demo-health", "description": null, "parameters": {}, "timeout_seconds": 30},
                {"name": "demo-rollback", "description": null, "parameters": {}, "timeout_seconds": 30}
            ]
        }))
    }

    #[tool]
    async fn upload_file(&self, Parameters(args): Parameters<UploadFileArgs>) -> CallToolResult {
        if args.target != "test-server" {
            return fixture_error("unknown_target", "fixture target is not configured");
        }
        if args.remote_path != "/opt/staging/demo.jar" || !args.overwrite {
            return fixture_error(
                "invalid_upload",
                "fixture upload contract was not respected",
            );
        }
        if matches!(self.mode, FixtureMode::MalformedUpload) {
            return CallToolResult::structured(json!({}));
        }
        match std::fs::metadata(&args.local_path) {
            Ok(metadata) => CallToolResult::structured(json!({
                "bytes_transferred": metadata.len()
            })),
            Err(error) => fixture_error("fixture_io_error", &error.to_string()),
        }
    }

    #[tool]
    async fn run_task(&self, Parameters(args): Parameters<RunTaskArgs>) -> CallToolResult {
        if args.target != "test-server" {
            return fixture_error("unknown_target", "fixture target is not configured");
        }
        if !matches!(
            args.task.as_str(),
            "demo-backup" | "demo-install" | "demo-restart" | "demo-health" | "demo-rollback"
        ) {
            return fixture_error("unknown_task", "fixture task is not configured");
        }
        CallToolResult::structured(json!({
            "result": {
                "success": true,
                "exit_code": 0,
                "stdout": format!("{}:{}", args.task, args.parameters.len()),
                "stderr": "",
                "duration_ms": 1,
                "stdout_truncated": false,
                "stderr_truncated": false
            }
        }))
    }
}

fn fixture_error(code: &str, message: &str) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "code": code,
        "message": message
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let mode = if std::env::args().any(|arg| arg == "--malformed-upload") {
        FixtureMode::MalformedUpload
    } else {
        FixtureMode::Normal
    };
    let service = ProtocolFixture { mode }.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
