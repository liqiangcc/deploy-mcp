use std::fmt;

use serde::Serialize;
use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidConfiguration,
    InvalidRequest,
    UnknownApplication,
    UnknownEnvironment,
    UnknownDeployment,
    UnknownRecoveryIncident,
    InvalidVersion,
    InvalidArtifact,
    ArtifactNotFound,
    ArtifactChanged,
    ConflictingDeployment,
    PrecheckFailed,
    RemoteCapabilityMissing,
    RemoteExecutionFailed,
    VerificationFailed,
    RollbackUnavailable,
    RollbackFailed,
    RecoveryIncidentConflict,
    InvalidStateTransition,
    PersistenceFailed,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "invalid_configuration",
            Self::InvalidRequest => "invalid_request",
            Self::UnknownApplication => "unknown_application",
            Self::UnknownEnvironment => "unknown_environment",
            Self::UnknownDeployment => "unknown_deployment",
            Self::UnknownRecoveryIncident => "unknown_recovery_incident",
            Self::InvalidVersion => "invalid_version",
            Self::InvalidArtifact => "invalid_artifact",
            Self::ArtifactNotFound => "artifact_not_found",
            Self::ArtifactChanged => "artifact_changed",
            Self::ConflictingDeployment => "conflicting_deployment",
            Self::PrecheckFailed => "precheck_failed",
            Self::RemoteCapabilityMissing => "remote_capability_missing",
            Self::RemoteExecutionFailed => "remote_execution_failed",
            Self::VerificationFailed => "verification_failed",
            Self::RollbackUnavailable => "rollback_unavailable",
            Self::RollbackFailed => "rollback_failed",
            Self::RecoveryIncidentConflict => "recovery_incident_conflict",
            Self::InvalidStateTransition => "invalid_state_transition",
            Self::PersistenceFailed => "persistence_failed",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{code}: {message}")]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_configuration(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidConfiguration, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_stable() {
        let expected = [
            (ErrorCode::InvalidConfiguration, "invalid_configuration"),
            (ErrorCode::InvalidRequest, "invalid_request"),
            (ErrorCode::UnknownApplication, "unknown_application"),
            (ErrorCode::UnknownEnvironment, "unknown_environment"),
            (ErrorCode::UnknownDeployment, "unknown_deployment"),
            (
                ErrorCode::UnknownRecoveryIncident,
                "unknown_recovery_incident",
            ),
            (ErrorCode::InvalidVersion, "invalid_version"),
            (ErrorCode::InvalidArtifact, "invalid_artifact"),
            (ErrorCode::ArtifactNotFound, "artifact_not_found"),
            (ErrorCode::ArtifactChanged, "artifact_changed"),
            (ErrorCode::ConflictingDeployment, "conflicting_deployment"),
            (ErrorCode::PrecheckFailed, "precheck_failed"),
            (
                ErrorCode::RemoteCapabilityMissing,
                "remote_capability_missing",
            ),
            (ErrorCode::RemoteExecutionFailed, "remote_execution_failed"),
            (ErrorCode::VerificationFailed, "verification_failed"),
            (ErrorCode::RollbackUnavailable, "rollback_unavailable"),
            (ErrorCode::RollbackFailed, "rollback_failed"),
            (
                ErrorCode::RecoveryIncidentConflict,
                "recovery_incident_conflict",
            ),
            (
                ErrorCode::InvalidStateTransition,
                "invalid_state_transition",
            ),
            (ErrorCode::PersistenceFailed, "persistence_failed"),
        ];

        for (code, text) in expected {
            assert_eq!(code.as_str(), text);
        }
    }

    #[test]
    fn display_includes_machine_code() {
        let error = AppError::new(ErrorCode::UnknownApplication, "missing demo");
        assert_eq!(error.to_string(), "unknown_application: missing demo");
    }
}
