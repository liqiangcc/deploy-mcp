use serde::{Deserialize, Serialize};

use super::{Artifact, DeploymentError, DeploymentId};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackBoundary {
    first_live_mutation: DeploymentStep,
}

impl RollbackBoundary {
    pub const fn jar_systemd() -> Self {
        Self {
            first_live_mutation: DeploymentStep::Install,
        }
    }

    pub const fn first_live_mutation(self) -> DeploymentStep {
        self.first_live_mutation
    }

    pub fn requires_rollback_after_failure(
        self,
        failed_step: DeploymentStep,
        rollback_available: bool,
    ) -> bool {
        rollback_available && step_order(failed_step) >= step_order(self.first_live_mutation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentPlan {
    pub deployment_id: DeploymentId,
    pub target: String,
    pub artifact: Artifact,
    pub steps: Vec<DeploymentStep>,
    pub rollback_boundary: RollbackBoundary,
    pub rollback_available: bool,
}

impl DeploymentPlan {
    pub fn jar_systemd(
        deployment_id: DeploymentId,
        target: impl Into<String>,
        artifact: Artifact,
        rollback_available: bool,
    ) -> Result<Self, DeploymentError> {
        let target = target.into();
        if target.trim().is_empty() {
            return Err(DeploymentError::InvalidPlanTarget);
        }

        Ok(Self {
            deployment_id,
            target,
            artifact,
            steps: vec![
                DeploymentStep::Precheck,
                DeploymentStep::StageArtifact,
                DeploymentStep::BackupCurrent,
                DeploymentStep::Install,
                DeploymentStep::Restart,
                DeploymentStep::Verify,
            ],
            rollback_boundary: RollbackBoundary::jar_systemd(),
            rollback_available,
        })
    }

    pub fn requires_rollback_after_failure(&self, failed_step: DeploymentStep) -> bool {
        self.rollback_boundary
            .requires_rollback_after_failure(failed_step, self.rollback_available)
    }
}

const fn step_order(step: DeploymentStep) -> u8 {
    match step {
        DeploymentStep::Precheck => 0,
        DeploymentStep::StageArtifact => 1,
        DeploymentStep::BackupCurrent => 2,
        DeploymentStep::Install => 3,
        DeploymentStep::Restart => 4,
        DeploymentStep::Verify => 5,
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
    fn jar_systemd_plan_has_fixed_order_and_install_mutation_boundary() {
        let plan = plan(true);
        assert_eq!(
            plan.steps,
            vec![
                DeploymentStep::Precheck,
                DeploymentStep::StageArtifact,
                DeploymentStep::BackupCurrent,
                DeploymentStep::Install,
                DeploymentStep::Restart,
                DeploymentStep::Verify,
            ]
        );
        assert_eq!(
            plan.rollback_boundary.first_live_mutation(),
            DeploymentStep::Install
        );
    }

    #[test]
    fn stage_and_backup_failures_do_not_require_rollback() {
        let plan = plan(true);
        assert!(!plan.requires_rollback_after_failure(DeploymentStep::Precheck));
        assert!(!plan.requires_rollback_after_failure(DeploymentStep::StageArtifact));
        assert!(!plan.requires_rollback_after_failure(DeploymentStep::BackupCurrent));
    }

    #[test]
    fn install_and_later_failures_require_rollback_when_available() {
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
