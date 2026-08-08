//! Infrastructure adapters.
//!
//! `RemoteExecMcpAdapter` speaks MCP stdio to `remote-exec-mcp`. It does not
//! embed SSH/SFTP, host-key, credential, shell-planning, or remote-path policy.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::child_process::TokioChildProcess;
use rmcp::ServiceExt;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::process::Command;

use crate::ports::{
    RemoteExecutionError, RemoteExecutionPort, RemoteExecutionResult, RemoteTargetCheck,
    RemoteTaskResult, RemoteTransferResult,
};

pub struct RemoteExecMcpAdapter {
    client: RunningService<RoleClient, ()>,
}

impl RemoteExecMcpAdapter {
    pub async fn spawn(command: &str, args: &[String]) -> RemoteExecutionResult<Self> {
        let mut child = Command::new(command);
        child.args(args);
        let transport = TokioChildProcess::new(child)
            .map_err(|error| RemoteExecutionError::Transport(error.to_string()))?;
        let client = ().serve(transport).await.map_err(|error| {
            RemoteExecutionError::Transport(format!("failed to initialize remote-exec MCP: {error}"))
        })?;
        Ok(Self { client })
    }

    async fn call(&self, tool: &'static str, arguments: Map<String, Value>) -> RemoteExecutionResult<Value> {
        let result = self
            .client
            .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
            .await
            .map_err(|error| RemoteExecutionError::Transport(error.to_string()))?;
        decode_tool_result(tool, result)
    }
}

#[async_trait]
impl RemoteExecutionPort for RemoteExecMcpAdapter {
    async fn check_target(&self, target: &str) -> RemoteExecutionResult<RemoteTargetCheck> {
        let response: CheckTargetResponse = decode_value(
            "check_target",
            self.call("check_target", json_object(json!({ "target": target })))
                .await?,
        )?;
        Ok(RemoteTargetCheck {
            reachable: response.reachable,
            remote_identity: response.remote_identity,
        })
    }

    async fn list_tasks(&self, target: &str) -> RemoteExecutionResult<BTreeSet<String>> {
        let response: ListTasksResponse = decode_value(
            "list_tasks",
            self.call("list_tasks", json_object(json!({ "target": target })))
                .await?,
        )?;
        Ok(response.tasks.into_iter().map(|task| task.name).collect())
    }

    async fn upload_file(
        &self,
        target: &str,
        local_path: &str,
        remote_path: &str,
        overwrite: bool,
    ) -> RemoteExecutionResult<RemoteTransferResult> {
        let response: UploadResponse = decode_value(
            "upload_file",
            self.call(
                "upload_file",
                json_object(json!({
                    "target": target,
                    "local_path": local_path,
                    "remote_path": remote_path,
                    "overwrite": overwrite
                })),
            )
            .await?,
        )?;
        Ok(RemoteTransferResult {
            bytes_transferred: response.bytes_transferred,
        })
    }

    async fn run_task(
        &self,
        target: &str,
        task: &str,
        parameters: BTreeMap<String, Value>,
    ) -> RemoteExecutionResult<RemoteTaskResult> {
        let response: RunTaskResponse = decode_value(
            "run_task",
            self.call(
                "run_task",
                json_object(json!({
                    "target": target,
                    "task": task,
                    "parameters": parameters
                })),
            )
            .await?,
        )?;
        Ok(response.result.into())
    }
}

#[derive(Debug, Deserialize)]
struct CheckTargetResponse {
    reachable: bool,
    remote_identity: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListTasksResponse {
    tasks: Vec<RemoteTaskDefinitionWire>,
}

#[derive(Debug, Deserialize)]
struct RemoteTaskDefinitionWire {
    name: String,
}

#[derive(Debug, Deserialize)]
struct UploadResponse {
    bytes_transferred: u64,
}

#[derive(Debug, Deserialize)]
struct RunTaskResponse {
    result: RemoteTaskResultWire,
}

#[derive(Debug, Deserialize)]
struct RemoteTaskResultWire {
    success: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    duration_ms: u128,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

impl From<RemoteTaskResultWire> for RemoteTaskResult {
    fn from(value: RemoteTaskResultWire) -> Self {
        Self {
            success: value.success,
            exit_code: value.exit_code,
            stdout: value.stdout,
            stderr: value.stderr,
            duration_ms: value.duration_ms,
            stdout_truncated: value.stdout_truncated,
            stderr_truncated: value.stderr_truncated,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RemoteErrorEnvelope {
    code: String,
    message: String,
}

fn decode_tool_result(tool: &'static str, result: CallToolResult) -> RemoteExecutionResult<Value> {
    let value = result.structured_content.ok_or_else(|| {
        RemoteExecutionError::InvalidResponse {
            tool: tool.to_owned(),
            message: "missing structured_content".to_owned(),
        }
    })?;

    if result.is_error == Some(true) {
        let error: RemoteErrorEnvelope = decode_value(tool, value)?;
        Err(RemoteExecutionError::Remote {
            code: error.code,
            message: error.message,
        })
    } else {
        Ok(value)
    }
}

fn decode_value<T: DeserializeOwned>(tool: &'static str, value: Value) -> RemoteExecutionResult<T> {
    serde_json::from_value(value).map_err(|error| RemoteExecutionError::InvalidResponse {
        tool: tool.to_owned(),
        message: error.to_string(),
    })
}

fn json_object(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .expect("adapter tool arguments are always JSON objects")
        .clone()
}

/// Deterministic remote adapter for workflow/application tests.
#[derive(Clone, Default)]
pub struct FakeRemoteExecution {
    state: Arc<Mutex<FakeRemoteState>>,
}

#[derive(Default)]
struct FakeRemoteState {
    target_checks: BTreeMap<String, RemoteExecutionResult<RemoteTargetCheck>>,
    task_lists: BTreeMap<String, RemoteExecutionResult<BTreeSet<String>>>,
    upload_results:
        BTreeMap<(String, String, String, bool), RemoteExecutionResult<RemoteTransferResult>>,
    task_results: BTreeMap<(String, String), RemoteExecutionResult<RemoteTaskResult>>,
    calls: Vec<FakeRemoteCall>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FakeRemoteCall {
    CheckTarget {
        target: String,
    },
    ListTasks {
        target: String,
    },
    UploadFile {
        target: String,
        local_path: String,
        remote_path: String,
        overwrite: bool,
    },
    RunTask {
        target: String,
        task: String,
        parameters: BTreeMap<String, Value>,
    },
}

impl FakeRemoteExecution {
    pub fn set_target_check(
        &self,
        target: impl Into<String>,
        result: RemoteExecutionResult<RemoteTargetCheck>,
    ) {
        self.state
            .lock()
            .expect("fake remote lock poisoned")
            .target_checks
            .insert(target.into(), result);
    }

    pub fn set_tasks(
        &self,
        target: impl Into<String>,
        result: RemoteExecutionResult<BTreeSet<String>>,
    ) {
        self.state
            .lock()
            .expect("fake remote lock poisoned")
            .task_lists
            .insert(target.into(), result);
    }

    pub fn set_upload_result(
        &self,
        target: impl Into<String>,
        local_path: impl Into<String>,
        remote_path: impl Into<String>,
        overwrite: bool,
        result: RemoteExecutionResult<RemoteTransferResult>,
    ) {
        self.state
            .lock()
            .expect("fake remote lock poisoned")
            .upload_results
            .insert(
                (
                    target.into(),
                    local_path.into(),
                    remote_path.into(),
                    overwrite,
                ),
                result,
            );
    }

    pub fn set_task_result(
        &self,
        target: impl Into<String>,
        task: impl Into<String>,
        result: RemoteExecutionResult<RemoteTaskResult>,
    ) {
        self.state
            .lock()
            .expect("fake remote lock poisoned")
            .task_results
            .insert((target.into(), task.into()), result);
    }

    pub fn calls(&self) -> Vec<FakeRemoteCall> {
        self.state
            .lock()
            .expect("fake remote lock poisoned")
            .calls
            .clone()
    }
}

#[async_trait]
impl RemoteExecutionPort for FakeRemoteExecution {
    async fn check_target(&self, target: &str) -> RemoteExecutionResult<RemoteTargetCheck> {
        let mut state = self.state.lock().expect("fake remote lock poisoned");
        state.calls.push(FakeRemoteCall::CheckTarget {
            target: target.to_owned(),
        });
        state
            .target_checks
            .get(target)
            .cloned()
            .unwrap_or_else(|| Err(fake_missing("target check", target)))
    }

    async fn list_tasks(&self, target: &str) -> RemoteExecutionResult<BTreeSet<String>> {
        let mut state = self.state.lock().expect("fake remote lock poisoned");
        state.calls.push(FakeRemoteCall::ListTasks {
            target: target.to_owned(),
        });
        state
            .task_lists
            .get(target)
            .cloned()
            .unwrap_or_else(|| Err(fake_missing("task list", target)))
    }

    async fn upload_file(
        &self,
        target: &str,
        local_path: &str,
        remote_path: &str,
        overwrite: bool,
    ) -> RemoteExecutionResult<RemoteTransferResult> {
        let key = (
            target.to_owned(),
            local_path.to_owned(),
            remote_path.to_owned(),
            overwrite,
        );
        let mut state = self.state.lock().expect("fake remote lock poisoned");
        state.calls.push(FakeRemoteCall::UploadFile {
            target: key.0.clone(),
            local_path: key.1.clone(),
            remote_path: key.2.clone(),
            overwrite: key.3,
        });
        state
            .upload_results
            .get(&key)
            .cloned()
            .unwrap_or_else(|| Err(fake_missing("upload result", remote_path)))
    }

    async fn run_task(
        &self,
        target: &str,
        task: &str,
        parameters: BTreeMap<String, Value>,
    ) -> RemoteExecutionResult<RemoteTaskResult> {
        let key = (target.to_owned(), task.to_owned());
        let mut state = self.state.lock().expect("fake remote lock poisoned");
        state.calls.push(FakeRemoteCall::RunTask {
            target: target.to_owned(),
            task: task.to_owned(),
            parameters,
        });
        state
            .task_results
            .get(&key)
            .cloned()
            .unwrap_or_else(|| Err(fake_missing("task result", task)))
    }
}

fn fake_missing(kind: &str, key: &str) -> RemoteExecutionError {
    RemoteExecutionError::Remote {
        code: "fake_not_configured".to_owned(),
        message: format!("no fake {kind} configured for {key}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_remote_error_preserves_code_and_message() {
        let error = decode_tool_result(
            "check_target",
            CallToolResult::structured_error(json!({
                "code": "authentication_failed",
                "message": "authentication rejected"
            })),
        )
        .unwrap_err();
        assert_eq!(
            error,
            RemoteExecutionError::Remote {
                code: "authentication_failed".to_owned(),
                message: "authentication rejected".to_owned(),
            }
        );
    }

    #[test]
    fn malformed_success_response_is_distinct_from_remote_error() {
        let value = decode_tool_result("upload_file", CallToolResult::structured(json!({}))).unwrap();
        let error = decode_value::<UploadResponse>("upload_file", value).unwrap_err();
        assert!(matches!(
            error,
            RemoteExecutionError::InvalidResponse { .. }
        ));
    }

    #[tokio::test]
    async fn fake_records_typed_calls_without_remote_process() {
        let fake = FakeRemoteExecution::default();
        fake.set_target_check(
            "test-server",
            Ok(RemoteTargetCheck {
                reachable: true,
                remote_identity: Some("host-key".to_owned()),
            }),
        );
        fake.set_tasks(
            "test-server",
            Ok(["restart".to_owned()].into_iter().collect()),
        );

        assert!(fake.check_target("test-server").await.unwrap().reachable);
        assert!(fake
            .list_tasks("test-server")
            .await
            .unwrap()
            .contains("restart"));
        assert_eq!(
            fake.calls(),
            vec![
                FakeRemoteCall::CheckTarget {
                    target: "test-server".to_owned()
                },
                FakeRemoteCall::ListTasks {
                    target: "test-server".to_owned()
                }
            ]
        );
    }
}
