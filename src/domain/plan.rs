use serde::{Deserialize, Serialize};

use super::{Artifact, DeploymentError, DeploymentId, DeploymentMechanismKind, ReleaseIdentity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentLifecycleOperation {
    Validate,
    Precheck,
    Prepare,
    CaptureRollback,
    Apply,
    Activate,
    Verify,
}

impl DeploymentLifecycleOperation {
    pub const fn durable_step(self) -> Option<DeploymentStep> {
        match self {
            Self::Validate => None,
            Self::Precheck => Some(DeploymentStep::Precheck),
            Self::Prepare => Some(DeploymentStep::StageArtifact),
            Self::CaptureRollback => Some(DeploymentStep::BackupCurrent),
            Self::Apply => Some(DeploymentStep::Install),
            Self::Activate => Some(DeploymentStep::Restart),
            Self::Verify => Some(DeploymentStep::Verify),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentStep {
    Precheck,
    StageArtifact,
    BackupCurrent,
    Install,
    Restart,
    Verify,
}

impl DeploymentStep {
    pub const fn lifecycle_operation(self) -> DeploymentLifecycleOperation {
        match self {
            Self::Precheck => DeploymentLifecycleOperation::Precheck,
            Self::StageArtifact => DeploymentLifecycleOperation::Prepare,
            Self::BackupCurrent => DeploymentLifecycleOperation::CaptureRollback,
            Self::Install => DeploymentLifecycleOperation::Apply,
            Self::Restart => DeploymentLifecycleOperation::Activate,
            Self::Verify => DeploymentLifecycleOperation::Verify,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackBoundary {
    first_live_mutation: DeploymentLifecycleOperation,
}

impl RollbackBoundary {
    pub const fn generic() -> Self {
        Self {
            first_live_mutation: DeploymentLifecycleOperation::Apply,
        }
    }

    pub const fn first_live_mutation(self) -> DeploymentLifecycleOperation {
        self.first_live_mutation
    }

    pub fn requires_rollback_after_failure(
        self,
        failed_step: DeploymentStep,
        rollback_available: bool,
    ) -> bool {
        self.requires_rollback_after_operation(
            failed_step.lifecycle_operation(),
            rollback_available,
        )
    }

    pub fn requires_rollback_after_operation(
        self,
        failed_operation: DeploymentLifecycleOperation,
        rollback_available: bool,
    ) -> bool {
        rollback_available
            && lifecycle_order(failed_operation) >= lifecycle_order(self.first_live_mutation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentPlan {
    deployment_id: DeploymentId,
    target: String,
    mechanism_kind: DeploymentMechanismKind,
    release_identity: ReleaseIdentity,
    operations: Vec<DeploymentLifecycleOperation>,
    rollback_boundary: RollbackBoundary,
    rollback_available: bool,
}

impl DeploymentPlan {
    pub fn generic(
        deployment_id: DeploymentId,
        target: impl Into<String>,
        mechanism_kind: DeploymentMechanismKind,
        release_identity: ReleaseIdentity,
        rollback_available: bool,
    ) -> Result<Self, DeploymentError> {
        let target = target.into();
        if target.trim().is_empty() {
            return Err(DeploymentError::InvalidPlanTarget);
        }

        Ok(Self {
            deployment_id,
            target,
            mechanism_kind,
            release_identity,
            operations: vec![
                DeploymentLifecycleOperation::Validate,
                DeploymentLifecycleOperation::Precheck,
                DeploymentLifecycleOperation::Prepare,
                DeploymentLifecycleOperation::CaptureRollback,
                DeploymentLifecycleOperation::Apply,
                DeploymentLifecycleOperation::Activate,
                DeploymentLifecycleOperation::Verify,
            ],
            rollback_boundary: RollbackBoundary::generic(),
            rollback_available,
        })
    }

    pub fn jar_systemd(
        deployment_id: DeploymentId,
        target: impl Into<String>,
        artifact: Artifact,
        rollback_available: bool,
    ) -> Result<Self, DeploymentError> {
        Self::generic(
            deployment_id,
            target,
            DeploymentMechanismKind::JarSystemd,
            ReleaseIdentity::LocalFile(artifact),
            rollback_available,
        )
    }

    pub fn deployment_id(&self) -> &DeploymentId {
        &self.deployment_id
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn mechanism_kind(&self) -> DeploymentMechanismKind {
        self.mechanism_kind
    }

    pub fn release_identity(&self) -> &ReleaseIdentity {
        &self.release_identity
    }

    /// Compatibility projection for v0.1 JAR-only callers.
    pub fn artifact(&self) -> &Artifact {
        self.release_identity
            .local_file()
            .expect("artifact() is only valid for local-file deployment plans")
    }

    pub fn operations(&self) -> &[DeploymentLifecycleOperation] {
        &self.operations
    }

    pub fn rollback_boundary(&self) -> RollbackBoundary {
        self.rollback_boundary
    }

    pub fn rollback_available(&self) -> bool {
        self.rollback_available
    }

    pub fn requires_rollback_after_failure(&self, failed_step: DeploymentStep) -> bool {
        self.rollback_boundary
            .requires_rollback_after_failure(failed_step, self.rollback_available)
    }
}

const fn lifecycle_order(operation: DeploymentLifecycleOperation) -> u8 {
    match operation {
        DeploymentLifecycleOperation::Validate => 0,
        DeploymentLifecycleOperation::Precheck => 1,
        DeploymentLifecycleOperation::Prepare => 2,
        DeploymentLifecycleOperation::CaptureRollback => 3,
        DeploymentLifecycleOperation::Apply => 4,
        DeploymentLifecycleOperation::Activate => 5,
        DeploymentLifecycleOperation::Verify => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn plan(rollback_available: bool) -> DeploymentPlan {
        DeploymentPlan::jar_systemd(
            DeploymentId::new("d1").unwrap(),
            "test-server",
            Artifact::new("1.0.0", 1, SHA256).unwrap(),
            rollback_available,
        )
        .unwrap()
    }

    #[test]
    fn generic_lifecycle_maps_to_existing_durable_steps_without_renaming_state_history() {
        let plan = plan(true);
        assert_eq!(
            plan.operations(),
            [
                DeploymentLifecycleOperation::Validate,
                DeploymentLifecycleOperation::Precheck,
                DeploymentLifecycleOperation::Prepare,
                DeploymentLifecycleOperation::CaptureRollback,
                DeploymentLifecycleOperation::Apply,
                DeploymentLifecycleOperation::Activate,
                DeploymentLifecycleOperation::Verify,
            ]
        );
        assert_eq!(
            DeploymentLifecycleOperation::Prepare.durable_step(),
            Some(DeploymentStep::StageArtifact)
        );
        assert_eq!(
            DeploymentLifecycleOperation::CaptureRollback.durable_step(),
            Some(DeploymentStep::BackupCurrent)
        );
        assert_eq!(
            plan.rollback_boundary().first_live_mutation(),
            DeploymentLifecycleOperation::Apply
        );
    }

    #[test]
    fn plan_identity_and_release_are_exposed_read_only() {
        let plan = plan(true);
        assert_eq!(plan.deployment_id().as_str(), "d1");
        assert_eq!(plan.target(), "test-server");
        assert_eq!(plan.mechanism_kind(), DeploymentMechanismKind::JarSystemd);
        assert_eq!(plan.release_identity().version(), "1.0.0");
        assert_eq!(plan.artifact().version(), "1.0.0");
        assert!(plan.rollback_available());
    }

    #[test]
    fn prepare_and_capture_failures_do_not_require_rollback() {
        let plan = plan(true);
        assert!(!plan.requires_rollback_after_failure(DeploymentStep::Precheck));
        assert!(!plan.requires_rollback_after_failure(DeploymentStep::StageArtifact));
        assert!(!plan.requires_rollback_after_failure(DeploymentStep::BackupCurrent));
    }

    #[test]
    fn apply_and_later_failures_require_rollback_when_available() {
        let plan = plan(true);
        assert!(plan.requires_rollback_after_failure(DeploymentStep::Install));
        assert!(plan.requires_rollback_after_failure(DeploymentStep::Restart));
        assert!(plan.requires_rollback_after_failure(DeploymentStep::Verify));
    }

    #[test]
    fn rollback_unavailable_never_claims_automatic_recovery() {
        let plan = plan(false);
        assert!(!plan.requires_rollback_after_failure(DeploymentStep::Install));
        assert!(!plan.requires_rollback_after_failure(DeploymentStep::Verify));
    }

    #[test]
    fn empty_target_is_rejected() {
        let error = DeploymentPlan::jar_systemd(
            DeploymentId::new("d1").unwrap(),
            "   ",
            Artifact::new("1.0.0", 1, SHA256).unwrap(),
            true,
        )
        .unwrap_err();
        assert_eq!(error, DeploymentError::InvalidPlanTarget);
    }
}
