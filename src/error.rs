use std::fmt;

use serde::Serialize;
use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidConfiguration,
    UnknownApplication,
    UnknownEnvironment,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "invalid_configuration",
            Self::UnknownApplication => "unknown_application",
            Self::UnknownEnvironment => "unknown_environment",
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
        assert_eq!(
            ErrorCode::InvalidConfiguration.as_str(),
            "invalid_configuration"
        );
        assert_eq!(
            ErrorCode::UnknownApplication.as_str(),
            "unknown_application"
        );
        assert_eq!(
            ErrorCode::UnknownEnvironment.as_str(),
            "unknown_environment"
        );
    }

    #[test]
    fn display_includes_machine_code() {
        let error = AppError::new(ErrorCode::UnknownApplication, "missing demo");
        assert_eq!(error.to_string(), "unknown_application: missing demo");
    }
}
