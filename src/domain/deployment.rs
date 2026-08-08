use serde::{Deserialize, Serialize};
use thiserror::Error;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, DeploymentError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(DeploymentError::InvalidIdentifier(stringify!($name)));
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

id_type!(ApplicationId);
id_type!(EnvironmentId);
id_type!(DeploymentId);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub version: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentState {
    Created,
    Prepared,
    BackedUp,
    Installed,
    Restarted,
    Verified,
    Failed,
    RolledBack,
}

impl DeploymentState {
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Created, Self::Prepared)
                | (Self::Prepared, Self::BackedUp)
                | (Self::BackedUp, Self::Installed)
                | (Self::Installed, Self::Restarted)
                | (Self::Restarted, Self::Verified)
                | (_, Self::Failed)
                | (Self::Failed, Self::RolledBack)
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deployment {
    pub id: DeploymentId,
    pub application: ApplicationId,
    pub environment: EnvironmentId,
    pub artifact: Artifact,
    state: DeploymentState,
}

impl Deployment {
    pub fn new(
        id: DeploymentId,
        application: ApplicationId,
        environment: EnvironmentId,
        artifact: Artifact,
    ) -> Self {
        Self {
            id,
            application,
            environment,
            artifact,
            state: DeploymentState::Created,
        }
    }

    pub fn state(&self) -> DeploymentState {
        self.state
    }

    pub fn transition(&mut self, next: DeploymentState) -> Result<(), DeploymentError> {
        if !self.state.can_transition_to(next) {
            return Err(DeploymentError::InvalidTransition {
                from: self.state,
                to: next,
            });
        }
        self.state = next;
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DeploymentError {
    #[error("invalid identifier: {0}")]
    InvalidIdentifier(&'static str),
    #[error("invalid deployment transition: {from:?} -> {to:?}")]
    InvalidTransition { from: DeploymentState, to: DeploymentState },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deployment() -> Deployment {
        Deployment::new(
            DeploymentId::new("d1").unwrap(),
            ApplicationId::new("app").unwrap(),
            EnvironmentId::new("test").unwrap(),
            Artifact { version: "1".into(), size: 1, sha256: "abc".into() },
        )
    }

    #[test]
    fn accepts_valid_state_flow() {
        let mut deployment = deployment();
        deployment.transition(DeploymentState::Prepared).unwrap();
        deployment.transition(DeploymentState::BackedUp).unwrap();
        assert_eq!(deployment.state(), DeploymentState::BackedUp);
    }

    #[test]
    fn rejects_invalid_transition() {
        let mut deployment = deployment();
        assert!(deployment.transition(DeploymentState::Verified).is_err());
    }
}
