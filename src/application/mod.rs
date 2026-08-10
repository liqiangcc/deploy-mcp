//! Deployment use-case orchestration.
//!
//! Application services depend on domain types, configuration, and ports. They
//! never depend on SSH/SFTP implementations or MCP protocol types.

mod api;
mod artifact_access;
mod deploy;
mod lock;
mod mechanism;
mod preflight;
mod recovery;
mod retention;
mod rollback;

pub use api::{
    ApplicationSummary, DeploymentApi, DeploymentApplication, DeploymentDetails,
    DeploymentExecutionResult,
};
pub use deploy::{DeployRequest, DeployService, DeploymentFailure, DeploymentOutcome};
pub use lock::{DeploymentLease, DeploymentLockManager};
pub use mechanism::{
    DeploymentMechanismAction, DeploymentMechanismPort, JarSystemdMechanism,
    MechanismTaskExecution, RollbackMechanismAction, RollbackPreflightError,
};
pub use preflight::{preflight_remote_capabilities, RemotePreflightError, RemotePreflightReport};
pub use recovery::{
    RecoveryAcknowledgementRequest, RecoveryAdminService, StartupRecoveryReport,
    StartupRecoveryService,
};
pub use retention::{RollbackRetentionReport, RollbackRetentionService};
pub use rollback::{RollbackFailure, RollbackOutcome, RollbackRequest, RollbackService};
