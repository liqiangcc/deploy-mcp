//! Thin MCP protocol adapter.
//!
//! Tool schemas and protocol conversion belong here. Deployment semantics do not.

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::application::{
    ApplicationSummary, DeployRequest, DeploymentApi, DeploymentDetails, DeploymentExecutionResult,
    DeploymentFailure, DeploymentOutcome, RollbackFailure, RollbackOutcome,
};
use crate::domain::{Deployment, RollbackOperation};
use crate::error::AppError;
use crate::ports::{DeploymentTransition, StepAttemptRecord, StepAttemptStatus};

#[derive(Clone)]
pub struct DeployMcp {
    application: Arc<dyn DeploymentApi>,
}
impl DeployMcp {
    pub fn new(application: Arc<dyn DeploymentApi>) -> Self { Self { application } }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeployApplicationArgs {
    pub application: String,
    pub environment: String,
    pub version: String,
    pub artifact_path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetDeploymentArgs { pub deployment_id: String }

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RollbackDeploymentArgs { pub deployment_id: String }

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDeploymentsArgs {
    pub application: Option<String>,
    pub environment: Option<String>,
    pub limit: Option<usize>,
}

#[tool_router(server_handler)]
impl DeployMcp {
    #[tool(description = "List configured deployable applications and environments. This is read-only and performs no remote access.")]
    async fn list_applications(&self) -> CallToolResult {
        CallToolResult::structured(json!({
            "applications": self.application.list_applications().iter().map(application_json).collect::<Vec<_>>()
        }))
    }

    #[tool(description = "Deploy one local JAR to a configured application/environment through the deterministic deployment workflow. The tool accepts deployment intent only; it never accepts shell commands, SSH credentials, service names, or remote install paths.")]
    async fn deploy_application(&self, Parameters(args): Parameters<DeployApplicationArgs>) -> CallToolResult {
        match self.application.deploy_application(DeployRequest {
            application: args.application,
            environment: args.environment,
            version: args.version,
            artifact_path: args.artifact_path,
        }).await {
            Ok(result) => CallToolResult::structured(execution_result_json(&result)),
            Err(error) => tool_error(error),
        }
    }

    #[tool(description = "Get one durable deployment record including state transitions and step attempts. This is read-only.")]
    async fn get_deployment(&self, Parameters(args): Parameters<GetDeploymentArgs>) -> CallToolResult {
        match self.application.get_deployment(&args.deployment_id) {
            Ok(details) => CallToolResult::structured(details_json(&details)),
            Err(error) => tool_error(error),
        }
    }

    #[tool(description = "List recent durable deployments, optionally filtered by configured application/environment. This is read-only.")]
    async fn list_deployments(&self, Parameters(args): Parameters<ListDeploymentsArgs>) -> CallToolResult {
        match self.application.list_deployments(
            args.application.as_deref(), args.environment.as_deref(), args.limit.unwrap_or(50),
        ) {
            Ok(deployments) => CallToolResult::structured(json!({
                "deployments": deployments.iter().map(details_json).collect::<Vec<_>>()
            })),
            Err(error) => tool_error(error),
        }
    }

    #[tool(description = "Explicitly roll back a historical deployment only when its durable deployment-bound rollback reference is still active and the configured environment contract has not changed. The tool accepts only a deployment id; remote paths and task names are never caller-controlled.")]
    async fn rollback_deployment(&self, Parameters(args): Parameters<RollbackDeploymentArgs>) -> CallToolResult {
        match self.application.rollback_deployment(&args.deployment_id).await {
            Ok(outcome) => CallToolResult::structured(rollback_outcome_json(&outcome)),
            Err(error) => tool_error(error),
        }
    }
}

fn application_json(application: &ApplicationSummary) -> Value {
    json!({
        "id": application.id,
        "display_name": application.display_name,
        "artifact_type": application.artifact_type,
        "environments": application.environments,
    })
}

fn deployment_json(deployment: &Deployment) -> Value {
    json!({
        "id": deployment.id().as_str(),
        "application": deployment.application().as_str(),
        "environment": deployment.environment().as_str(),
        "artifact": {
            "version": deployment.artifact().version(),
            "size_bytes": deployment.artifact().size_bytes(),
            "sha256": deployment.artifact().sha256(),
        },
        "state": deployment.state(),
    })
}

fn execution_result_json(result: &DeploymentExecutionResult) -> Value {
    let mut value = outcome_json(&result.outcome);
    if let Some(object) = value.as_object_mut() {
        object.insert("rollback_reference_available".to_owned(), Value::Bool(result.rollback_reference_available));
        object.insert(
            "rollback_reference_error".to_owned(),
            result.rollback_reference_error.as_ref().map_or(Value::Null, |error| Value::String(error.clone())),
        );
    }
    value
}

fn outcome_json(outcome: &DeploymentOutcome) -> Value {
    json!({
        "deployment": deployment_json(&outcome.deployment),
        "failure": outcome.failure.as_ref().map(failure_json),
        "rollback_failure": outcome.rollback_failure.as_ref().map(failure_json),
    })
}

fn failure_json(failure: &DeploymentFailure) -> Value {
    json!({
        "step": failure.step,
        "code": failure.code.as_str(),
        "message": failure.message,
        "remote_code": failure.remote_code,
    })
}

fn details_json(details: &DeploymentDetails) -> Value {
    json!({
        "deployment": deployment_json(&details.deployment),
        "transitions": details.transitions.iter().map(transition_json).collect::<Vec<_>>(),
        "step_attempts": details.step_attempts.iter().map(step_attempt_json).collect::<Vec<_>>(),
    })
}

fn transition_json(transition: &DeploymentTransition) -> Value {
    json!({
        "from": transition.from,
        "to": transition.to,
        "occurred_at_unix_ms": transition.occurred_at_unix_ms,
    })
}

fn step_attempt_json(attempt: &StepAttemptRecord) -> Value {
    json!({
        "id": attempt.id.get(),
        "step": attempt.step,
        "status": match attempt.status {
            StepAttemptStatus::Started => "started",
            StepAttemptStatus::Succeeded => "succeeded",
            StepAttemptStatus::Failed => "failed",
        },
        "error": attempt.error,
        "started_at_unix_ms": attempt.started_at_unix_ms,
        "finished_at_unix_ms": attempt.finished_at_unix_ms,
    })
}

fn rollback_outcome_json(outcome: &RollbackOutcome) -> Value {
    json!({
        "operation": rollback_operation_json(&outcome.operation),
        "failure": outcome.failure.as_ref().map(rollback_failure_json),
    })
}

fn rollback_operation_json(operation: &RollbackOperation) -> Value {
    json!({
        "id": operation.id().as_str(),
        "source_deployment_id": operation.source_deployment_id().as_str(),
        "application": operation.application().as_str(),
        "environment": operation.environment().as_str(),
        "state": operation.state(),
    })
}

fn rollback_failure_json(failure: &RollbackFailure) -> Value {
    json!({
        "code": failure.code.as_str(),
        "message": failure.message,
        "remote_code": failure.remote_code,
    })
}

fn tool_error(error: AppError) -> CallToolResult {
    CallToolResult::structured_error(json!({ "code": error.code.as_str(), "message": error.message }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{AppResult, ErrorCode};
    use async_trait::async_trait;

    struct FakeApi;

    #[async_trait]
    impl DeploymentApi for FakeApi {
        fn list_applications(&self) -> Vec<ApplicationSummary> {
            vec![ApplicationSummary {
                id: "demo".to_owned(),
                display_name: Some("Demo Service".to_owned()),
                artifact_type: "jar".to_owned(),
                environments: vec!["test".to_owned()],
            }]
        }
        async fn deploy_application(&self, _request: DeployRequest) -> AppResult<DeploymentExecutionResult> {
            Err(AppError::new(ErrorCode::UnknownApplication, "unknown application: missing"))
        }
        fn get_deployment(&self, deployment_id: &str) -> AppResult<DeploymentDetails> {
            Err(AppError::new(ErrorCode::UnknownDeployment, format!("unknown deployment: {deployment_id}")))
        }
        fn list_deployments(
            &self, _application: Option<&str>, _environment: Option<&str>, _limit: usize,
        ) -> AppResult<Vec<DeploymentDetails>> { Ok(Vec::new()) }
        async fn rollback_deployment(&self, deployment_id: &str) -> AppResult<RollbackOutcome> {
            Err(AppError::new(ErrorCode::RollbackUnavailable, format!("no rollback reference for {deployment_id}")))
        }
    }

    #[tokio::test]
    async fn list_applications_exposes_deployment_catalog_only() {
        let mcp = DeployMcp::new(Arc::new(FakeApi));
        let value = mcp.list_applications().await.structured_content.unwrap();
        assert_eq!(value["applications"][0]["id"], "demo");
    }

    #[tokio::test]
    async fn rollback_tool_exposes_only_deployment_id() {
        let mcp = DeployMcp::new(Arc::new(FakeApi));
        let result = mcp.rollback_deployment(Parameters(RollbackDeploymentArgs { deployment_id: "d1".to_owned() })).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.structured_content.unwrap()["code"], "rollback_unavailable");
    }

    #[test]
    fn deploy_tool_rejects_undeclared_raw_execution_fields() {
        let error = serde_json::from_value::<DeployApplicationArgs>(json!({
            "application": "demo", "environment": "test", "version": "1.0.0",
            "artifact_path": "/tmp/demo.jar", "shell": "systemctl restart demo", "ssh_password": "secret"
        })).unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rollback_tool_rejects_remote_execution_parameters() {
        let error = serde_json::from_value::<RollbackDeploymentArgs>(json!({
            "deployment_id": "d1", "backup_path": "/tmp/attacker.jar", "task": "arbitrary-task"
        })).unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }
}
