//! Deployment use-case orchestration.
//!
//! Application services depend on domain types, configuration, and ports. They
//! never depend on SSH/SFTP implementations or MCP protocol types.

mod api;
mod deploy;
mod lock;
mod preflight;

pub use api::{ApplicationSummary, DeploymentApi, DeploymentApplication, DeploymentDetails};
pub use deploy::{DeployRequest, DeployService, DeploymentFailure, DeploymentOutcome};
pub use lock::{DeploymentLease, DeploymentLockManager};
pub use preflight::{preflight_remote_capabilities, RemotePreflightError, RemotePreflightReport};
