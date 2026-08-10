use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{ApplicationId, DeploymentId, DeploymentMechanismKind, EnvironmentId};

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
pub struct MechanismContractFingerprint(String);

impl MechanismContractFingerprint {
    pub fn new(value: impl Into<String>) -> Result<Self, RollbackError> {
        let value = value.into();
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(RollbackError::InvalidContractFingerprint);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn jar_systemd(target: &str, snapshot: &JarSystemdRollbackSnapshot) -> Self {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, DeploymentMechanismKind::JarSystemd.as_str());
        hash_field(&mut hasher, target);
        hash_field(&mut hasher, snapshot.backup_path());
        hash_field(&mut hasher, snapshot.install_path());
        hash_field(&mut hasher, snapshot.rollback_task());
        hash_field(&mut hasher, snapshot.restart_task());
        hash_field(&mut hasher, snapshot.health_check_task());
        Self(format!("{:x}", hasher.finalize()))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn docker_compose(
        target: &str,
        image_repository: &str,
        compose_project: &str,
        service: &str,
        precheck_task: Option<&str>,
        prepare_task: &str,
        capture_rollback_task: &str,
        apply_task: &str,
        activate_task: &str,
        health_check_task: &str,
        rollback_task: &str,
    ) -> Self {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, DeploymentMechanismKind::DockerCompose.as_str());
        hash_field(&mut hasher, target);
        hash_field(&mut hasher, image_repository);
        hash_field(&mut hasher, compose_project);
        hash_field(&mut hasher, service);
        hash_field(&mut hasher, precheck_task.unwrap_or(""));
        hash_field(&mut hasher, prepare_task);
        hash_field(&mut hasher, capture_rollback_task);
        hash_field(&mut hasher, apply_task);
        hash_field(&mut hasher, activate_task);
        hash_field(&mut hasher, health_check_task);
        hash_field(&mut hasher, rollback_task);
        Self(format!("{:x}", hasher.finalize()))
    }
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    hasher.update(length.to_be_bytes());
    hasher.update(value.as_bytes());
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JarSystemdRollbackSnapshot {
    backup_path: String,
    install_path: String,
    rollback_task: String,
    restart_task: String,
    health_check_task: String,
}

impl JarSystemdRollbackSnapshot {
    pub fn new(
        backup_path: impl Into<String>,
        install_path: impl Into<String>,
        rollback_task: impl Into<String>,
        restart_task: impl Into<String>,
        health_check_task: impl Into<String>,
    ) -> Result<Self, RollbackError> {
        Ok(Self {
            backup_path: non_empty("backup_path", backup_path.into())?,
            install_path: non_empty("install_path", install_path.into())?,
            rollback_task: non_empty("rollback_task", rollback_task.into())?,
            restart_task: non_empty("restart_task", restart_task.into())?,
            health_check_task: non_empty("health_check_task", health_check_task.into())?,
        })
    }

    pub fn backup_path(&self) -> &str {
        &self.backup_path
    }
    pub fn install_path(&self) -> &str {
        &self.install_path
    }
    pub fn rollback_task(&self) -> &str {
        &self.rollback_task
    }
    pub fn restart_task(&self) -> &str {
        &self.restart_task
    }
    pub fn health_check_task(&self) -> &str {
        &self.health_check_task
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerComposeRollbackSnapshot {
    previous_digest: String,
    image_repository: String,
    compose_project: String,
    service: String,
    rollback_task: String,
    activate_task: String,
    health_check_task: String,
}

impl DockerComposeRollbackSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        previous_digest: impl Into<String>,
        image_repository: impl Into<String>,
        compose_project: impl Into<String>,
        service: impl Into<String>,
        rollback_task: impl Into<String>,
        activate_task: impl Into<String>,
        health_check_task: impl Into<String>,
    ) -> Result<Self, RollbackError> {
        let previous_digest = previous_digest.into();
        validate_digest(&previous_digest)?;
        Ok(Self {
            previous_digest: previous_digest.to_ascii_lowercase(),
            image_repository: non_empty("image_repository", image_repository.into())?,
            compose_project: non_empty("compose_project", compose_project.into())?,
            service: non_empty("service", service.into())?,
            rollback_task: non_empty("rollback_task", rollback_task.into())?,
            activate_task: non_empty("activate_task", activate_task.into())?,
            health_check_task: non_empty("health_check_task", health_check_task.into())?,
        })
    }

    pub fn previous_digest(&self) -> &str {
        &self.previous_digest
    }
    pub fn image_repository(&self) -> &str {
        &self.image_repository
    }
    pub fn compose_project(&self) -> &str {
        &self.compose_project
    }
    pub fn service(&self) -> &str {
        &self.service
    }
    pub fn rollback_task(&self) -> &str {
        &self.rollback_task
    }
    pub fn activate_task(&self) -> &str {
        &self.activate_task
    }
    pub fn health_check_task(&self) -> &str {
        &self.health_check_task
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "snapshot", rename_all = "snake_case")]
pub enum RollbackMechanismSnapshot {
    JarSystemd(JarSystemdRollbackSnapshot),
    DockerCompose(DockerComposeRollbackSnapshot),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackReference {
    deployment_id: DeploymentId,
    application: ApplicationId,
    environment: EnvironmentId,
    mechanism_kind: DeploymentMechanismKind,
    target: String,
    contract_fingerprint: MechanismContractFingerprint,
    mechanism_snapshot: RollbackMechanismSnapshot,
    state: RollbackReferenceState,
}

impl RollbackReference {
    /// Backward-compatible v0.1 constructor. New mechanism code should prefer
    /// `jar_systemd` or `new_with_snapshot`.
    #[allow(clippy::too_many_arguments)]
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
        Self::jar_systemd(
            deployment_id,
            application,
            environment,
            target,
            backup_path,
            install_path,
            rollback_task,
            restart_task,
            health_check_task,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn jar_systemd(
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
        let snapshot = JarSystemdRollbackSnapshot::new(
            backup_path,
            install_path,
            rollback_task,
            restart_task,
            health_check_task,
        )?;
        let fingerprint = MechanismContractFingerprint::jar_systemd(&target, &snapshot);
        Self::new_with_snapshot(
            deployment_id,
            application,
            environment,
            DeploymentMechanismKind::JarSystemd,
            target,
            fingerprint,
            RollbackMechanismSnapshot::JarSystemd(snapshot),
        )
    }

    pub fn new_with_snapshot(
        deployment_id: DeploymentId,
        application: ApplicationId,
        environment: EnvironmentId,
        mechanism_kind: DeploymentMechanismKind,
        target: impl Into<String>,
        contract_fingerprint: MechanismContractFingerprint,
        mechanism_snapshot: RollbackMechanismSnapshot,
    ) -> Result<Self, RollbackError> {
        let target = non_empty("target", target.into())?;
        if !snapshot_matches_kind(mechanism_kind, &mechanism_snapshot) {
            return Err(RollbackError::MechanismSnapshotMismatch);
        }
        Ok(Self {
            deployment_id,
            application,
            environment,
            mechanism_kind,
            target,
            contract_fingerprint,
            mechanism_snapshot,
            state: RollbackReferenceState::Active,
        })
    }

    #[allow(clippy::too_many_arguments)]
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
        let snapshot = JarSystemdRollbackSnapshot {
            backup_path,
            install_path,
            rollback_task,
            restart_task,
            health_check_task,
        };
        let contract_fingerprint = MechanismContractFingerprint::jar_systemd(&target, &snapshot);
        Self {
            deployment_id,
            application,
            environment,
            mechanism_kind: DeploymentMechanismKind::JarSystemd,
            target,
            contract_fingerprint,
            mechanism_snapshot: RollbackMechanismSnapshot::JarSystemd(snapshot),
            state,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn rehydrate_with_snapshot(
        deployment_id: DeploymentId,
        application: ApplicationId,
        environment: EnvironmentId,
        mechanism_kind: DeploymentMechanismKind,
        target: String,
        contract_fingerprint: MechanismContractFingerprint,
        mechanism_snapshot: RollbackMechanismSnapshot,
        state: RollbackReferenceState,
    ) -> Result<Self, RollbackError> {
        if target.trim().is_empty() {
            return Err(RollbackError::EmptyCapability("target"));
        }
        if !snapshot_matches_kind(mechanism_kind, &mechanism_snapshot) {
            return Err(RollbackError::MechanismSnapshotMismatch);
        }
        Ok(Self {
            deployment_id,
            application,
            environment,
            mechanism_kind,
            target,
            contract_fingerprint,
            mechanism_snapshot,
            state,
        })
    }

    pub fn deployment_id(&self) -> &DeploymentId {
        &self.deployment_id
    }
    pub fn application(&self) -> &ApplicationId {
        &self.application
    }
    pub fn environment(&self) -> &EnvironmentId {
        &self.environment
    }
    pub fn mechanism_kind(&self) -> DeploymentMechanismKind {
        self.mechanism_kind
    }
    pub fn target(&self) -> &str {
        &self.target
    }
    pub fn contract_fingerprint(&self) -> &MechanismContractFingerprint {
        &self.contract_fingerprint
    }
    pub fn mechanism_snapshot(&self) -> &RollbackMechanismSnapshot {
        &self.mechanism_snapshot
    }
    pub fn jar_systemd_snapshot(&self) -> Option<&JarSystemdRollbackSnapshot> {
        match &self.mechanism_snapshot {
            RollbackMechanismSnapshot::JarSystemd(snapshot) => Some(snapshot),
            RollbackMechanismSnapshot::DockerCompose(_) => None,
        }
    }
    pub fn docker_compose_snapshot(&self) -> Option<&DockerComposeRollbackSnapshot> {
        match &self.mechanism_snapshot {
            RollbackMechanismSnapshot::DockerCompose(snapshot) => Some(snapshot),
            RollbackMechanismSnapshot::JarSystemd(_) => None,
        }
    }
    pub fn backup_path(&self) -> &str {
        self.jar_systemd_snapshot()
            .expect("legacy accessor requires jar_systemd rollback snapshot")
            .backup_path()
    }
    pub fn install_path(&self) -> &str {
        self.jar_systemd_snapshot()
            .expect("legacy accessor requires jar_systemd rollback snapshot")
            .install_path()
    }
    pub fn rollback_task(&self) -> &str {
        self.jar_systemd_snapshot()
            .expect("legacy accessor requires jar_systemd rollback snapshot")
            .rollback_task()
    }
    pub fn restart_task(&self) -> &str {
        self.jar_systemd_snapshot()
            .expect("legacy accessor requires jar_systemd rollback snapshot")
            .restart_task()
    }
    pub fn health_check_task(&self) -> &str {
        self.jar_systemd_snapshot()
            .expect("legacy accessor requires jar_systemd rollback snapshot")
            .health_check_task()
    }
    pub fn state(&self) -> RollbackReferenceState {
        self.state
    }
}

fn snapshot_matches_kind(
    mechanism_kind: DeploymentMechanismKind,
    snapshot: &RollbackMechanismSnapshot,
) -> bool {
    matches!(
        (mechanism_kind, snapshot),
        (
            DeploymentMechanismKind::JarSystemd,
            RollbackMechanismSnapshot::JarSystemd(_)
        ) | (
            DeploymentMechanismKind::DockerCompose,
            RollbackMechanismSnapshot::DockerCompose(_)
        )
    )
}

fn validate_digest(value: &str) -> Result<(), RollbackError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(RollbackError::InvalidContainerDigest);
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RollbackError::InvalidContainerDigest);
    }
    Ok(())
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
        Self {
            id,
            source_deployment_id,
            application,
            environment,
            state: RollbackOperationState::Started,
        }
    }

    pub(crate) fn rehydrate(
        id: RollbackOperationId,
        source_deployment_id: DeploymentId,
        application: ApplicationId,
        environment: EnvironmentId,
        state: RollbackOperationState,
    ) -> Self {
        Self {
            id,
            source_deployment_id,
            application,
            environment,
            state,
        }
    }

    pub fn id(&self) -> &RollbackOperationId {
        &self.id
    }
    pub fn source_deployment_id(&self) -> &DeploymentId {
        &self.source_deployment_id
    }
    pub fn application(&self) -> &ApplicationId {
        &self.application
    }
    pub fn environment(&self) -> &EnvironmentId {
        &self.environment
    }
    pub fn state(&self) -> RollbackOperationState {
        self.state
    }
}

fn non_empty(kind: &'static str, value: String) -> Result<String, RollbackError> {
    if value.trim().is_empty() {
        Err(RollbackError::EmptyCapability(kind))
    } else {
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RollbackError {
    #[error("rollback operation id must not be empty")]
    InvalidOperationId,
    #[error("rollback capability field must not be empty: {0}")]
    EmptyCapability(&'static str),
    #[error(
        "rollback mechanism contract fingerprint must contain exactly 64 hexadecimal characters"
    )]
    InvalidContractFingerprint,
    #[error("rollback mechanism snapshot does not match mechanism kind")]
    MechanismSnapshotMismatch,
    #[error("container rollback digest must be immutable sha256:<64 hex>")]
    InvalidContainerDigest,
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
            "server",
            "/backup/demo.jar",
            "/opt/demo.jar",
            "demo-rollback",
            "demo-restart",
            "demo-health",
        )
        .unwrap();
        assert_eq!(result.state(), RollbackReferenceState::Active);
        assert_eq!(result.target(), "server");
        assert_eq!(result.mechanism_kind(), DeploymentMechanismKind::JarSystemd);
        assert_eq!(result.contract_fingerprint().as_str().len(), 64);
    }

    #[test]
    fn jar_systemd_fingerprint_is_deterministic_and_changes_with_authority() {
        let first = RollbackReference::new(
            DeploymentId::new("d1").unwrap(),
            ApplicationId::new("demo").unwrap(),
            EnvironmentId::new("test").unwrap(),
            "server",
            "/backup/demo.jar",
            "/opt/demo.jar",
            "demo-rollback",
            "demo-restart",
            "demo-health",
        )
        .unwrap();
        let same = RollbackReference::new(
            DeploymentId::new("d2").unwrap(),
            ApplicationId::new("demo").unwrap(),
            EnvironmentId::new("test").unwrap(),
            "server",
            "/backup/demo.jar",
            "/opt/demo.jar",
            "demo-rollback",
            "demo-restart",
            "demo-health",
        )
        .unwrap();
        let changed = RollbackReference::new(
            DeploymentId::new("d3").unwrap(),
            ApplicationId::new("demo").unwrap(),
            EnvironmentId::new("test").unwrap(),
            "server",
            "/backup/demo.jar",
            "/opt/demo.jar",
            "demo-rollback-v2",
            "demo-restart",
            "demo-health",
        )
        .unwrap();
        assert_eq!(first.contract_fingerprint(), same.contract_fingerprint());
        assert_ne!(first.contract_fingerprint(), changed.contract_fingerprint());
    }

    #[test]
    fn invalid_contract_fingerprint_is_rejected() {
        assert_eq!(
            MechanismContractFingerprint::new("abc").unwrap_err(),
            RollbackError::InvalidContractFingerprint
        );
    }

    #[test]
    fn empty_operation_id_is_rejected() {
        assert_eq!(
            RollbackOperationId::new("").unwrap_err(),
            RollbackError::InvalidOperationId
        );
    }
}
