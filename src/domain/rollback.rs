use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{ApplicationId, DeploymentId, EnvironmentId};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RollbackOperationId(String);

impl RollbackOperationId {
    pub fn new(value: impl Into<String>) -> Result<Self, RollbackError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(RollbackError::InvalidOperationId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackReferenceState {
    Active,
    Superseded,
    Consumed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackReference {
    deployment_id: DeploymentId,
    application: ApplicationId,
    environment: EnvironmentId,
    target: String,
    backup_path: String,
    install_path: String,
    rollback_task: String,
    restart_task: String,
    health_check_task: String,
    state: RollbackReferenceState,
}

#[allow(clippy::too_many_arguments)]
impl RollbackReference {
    pub fn new(
        deployment_id: DeploymentId,
        application: ApplicationId,
        environment: EnvironmentId,
        target: impl Into<String>,
        backup_path: impl Into<String>,
        install_path: impl Into<String>,
        rollback_task: impl Into<String>,
        restart_task: impl Into<String>,
        health_check_task: impl Into<String>,
    ) -> Result<Self, RollbackError> {
        let target = non_empty("target", target.into())?;
        let backup_path = non_empty("backup_path", backup_path.into())?;
        let install_path = non_empty("install_path", install_path.into())?;
        let rollback_task = non_empty("rollback_task", rollback_task.into())?;
        let restart_task = non_empty("restart_task", restart_task.into())?;
        let health_check_task = non_empty("health_check_task", health_check_task.into())?;
        Ok(Self {
            deployment_id,
            application,
            environment,
            target,
            backup_path,
            install_path,
            rollback_task,
            restart_task,
            health_check_task,
            state: RollbackReferenceState::Active,
        })
    }

    pub(crate) fn rehydrate(
        deployment_id: DeploymentId,
        application: ApplicationId,
        environment: EnvironmentId,
        target: String,
        backup_path: String,
        install_path: String,
        rollback_task: String,
        restart_task: String,
        health_check_task: String,
        state: RollbackReferenceState,
    ) -> Self {
        Self {
            deployment_id,
            application,
            environment,
            target,
            backup_path,
            install_path,
            rollback_task,
            restart_task,
            health_check_task,
            state,
        }
    }

    pub fn deployment_id(&self) -> &DeploymentId { &self.deployment_id }
    pub fn application(&self) -> &ApplicationId { &self.application }
    pub fn environment(&self) -> &EnvironmentId { &self.environment }
    pub fn target(&self) -> &str { &self.target }
    pub fn backup_path(&self) -> &str { &self.backup_path }
    pub fn install_path(&self) -> &str { &self.install_path }
    pub fn rollback_task(&self) -> &str { &self.rollback_task }
    pub fn restart_task(&self) -> &str { &self.restart_task }
    pub fn health_check_task(&self) -> &str { &self.health_check_task }
    pub fn state(&self) -> RollbackReferenceState { self.state }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackOperationState {
    Started,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackOperation {
    id: RollbackOperationId,
    source_deployment_id: DeploymentId,
    application: ApplicationId,
    environment: EnvironmentId,
    state: RollbackOperationState,
}

impl RollbackOperation {
    pub fn new(
        id: RollbackOperationId,
        source_deployment_id: DeploymentId,
        application: ApplicationId,
        environment: EnvironmentId,
    ) -> Self {
        Self { id, source_deployment_id, application, environment, state: RollbackOperationState::Started }
    }

    pub(crate) fn rehydrate(
        id: RollbackOperationId,
        source_deployment_id: DeploymentId,
        application: ApplicationId,
        environment: EnvironmentId,
        state: RollbackOperationState,
    ) -> Self {
        Self { id, source_deployment_id, application, environment, state }
    }

    pub fn id(&self) -> &RollbackOperationId { &self.id }
    pub fn source_deployment_id(&self) -> &DeploymentId { &self.source_deployment_id }
    pub fn application(&self) -> &ApplicationId { &self.application }
    pub fn environment(&self) -> &EnvironmentId { &self.environment }
    pub fn state(&self) -> RollbackOperationState { self.state }
}

fn non_empty(kind: &'static str, value: String) -> Result<String, RollbackError> {
    if value.trim().is_empty() { Err(RollbackError::EmptyCapability(kind)) } else { Ok(value) }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RollbackError {
    #[error("rollback operation id must not be empty")]
    InvalidOperationId,
    #[error("rollback capability field must not be empty: {0}")]
    EmptyCapability(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_requires_complete_capability_snapshot() {
        let result = RollbackReference::new(
            DeploymentId::new("d1").unwrap(),
            ApplicationId::new("demo").unwrap(),
            EnvironmentId::new("test").unwrap(),
            "server", "/backup/demo.jar", "/opt/demo.jar",
            "demo-rollback", "demo-restart", "demo-health",
        ).unwrap();
        assert_eq!(result.state(), RollbackReferenceState::Active);
        assert_eq!(result.target(), "server");
    }

    #[test]
    fn empty_operation_id_is_rejected() {
        assert_eq!(RollbackOperationId::new("").unwrap_err(), RollbackError::InvalidOperationId);
    }
}
