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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentMechanismKind {
    JarSystemd,
    DockerCompose,
}

impl DeploymentMechanismKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::JarSystemd => "jar_systemd",
            Self::DockerCompose => "docker_compose",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "jar_systemd" => Some(Self::JarSystemd),
            "docker_compose" => Some(Self::DockerCompose),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    version: String,
    size_bytes: u64,
    sha256: String,
}

impl Artifact {
    pub fn new(
        version: impl Into<String>,
        size_bytes: u64,
        sha256: impl Into<String>,
    ) -> Result<Self, DeploymentError> {
        let version = version.into();
        if version.trim().is_empty() {
            return Err(DeploymentError::InvalidArtifactVersion);
        }
        if size_bytes == 0 {
            return Err(DeploymentError::InvalidArtifactSize);
        }

        let sha256 = sha256.into();
        if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(DeploymentError::InvalidArtifactChecksum);
        }

        Ok(Self {
            version,
            size_bytes,
            sha256: sha256.to_ascii_lowercase(),
        })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerImageReleaseIdentity {
    version: String,
    repository: String,
    digest: String,
}

impl ContainerImageReleaseIdentity {
    pub fn new(
        version: impl Into<String>,
        repository: impl Into<String>,
        digest: impl Into<String>,
    ) -> Result<Self, DeploymentError> {
        let version = version.into();
        if version.trim().is_empty() {
            return Err(DeploymentError::InvalidArtifactVersion);
        }
        let repository = repository.into();
        if repository.trim().is_empty() {
            return Err(DeploymentError::InvalidContainerRepository);
        }
        let digest = digest.into();
        let Some(hex) = digest.strip_prefix("sha256:") else {
            return Err(DeploymentError::InvalidContainerDigest);
        };
        if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(DeploymentError::InvalidContainerDigest);
        }
        Ok(Self {
            version,
            repository,
            digest: format!("sha256:{}", hex.to_ascii_lowercase()),
        })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn repository(&self) -> &str {
        &self.repository
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "identity", rename_all = "snake_case")]
pub enum ReleaseIdentity {
    LocalFile(Artifact),
    ContainerImage(ContainerImageReleaseIdentity),
}

impl ReleaseIdentity {
    pub fn version(&self) -> &str {
        match self {
            Self::LocalFile(artifact) => artifact.version(),
            Self::ContainerImage(image) => image.version(),
        }
    }

    pub fn local_file(&self) -> Option<&Artifact> {
        match self {
            Self::LocalFile(artifact) => Some(artifact),
            Self::ContainerImage(_) => None,
        }
    }

    pub fn container_image(&self) -> Option<&ContainerImageReleaseIdentity> {
        match self {
            Self::ContainerImage(image) => Some(image),
            Self::LocalFile(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentState {
    Created,
    Prechecking,
    StagingArtifact,
    BackingUp,
    Installing,
    Restarting,
    Verifying,
    Succeeded,
    Failed,
    RollingBack,
    RolledBack,
    RollbackFailed,
}

impl DeploymentState {
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Created, Self::Prechecking)
                | (Self::Prechecking, Self::StagingArtifact | Self::Failed)
                | (Self::StagingArtifact, Self::BackingUp | Self::Failed)
                | (Self::BackingUp, Self::Installing | Self::Failed)
                | (
                    Self::Installing,
                    Self::Restarting | Self::RollingBack | Self::Failed
                )
                | (
                    Self::Restarting,
                    Self::Verifying | Self::RollingBack | Self::Failed
                )
                | (
                    Self::Verifying,
                    Self::Succeeded | Self::RollingBack | Self::Failed
                )
                | (Self::RollingBack, Self::RolledBack | Self::RollbackFailed)
        )
    }

    /// Returns true once a remote operation with side effects may have started.
    ///
    /// This is the durable ambiguity boundary used by timeout/crash handling.
    /// It intentionally begins at artifact staging, before the live install
    /// boundary, because a timed-out upload or backup task may continue remotely
    /// after deploy-mcp stops waiting.
    pub fn has_live_mutation_started(self) -> bool {
        matches!(
            self,
            Self::StagingArtifact
                | Self::BackingUp
                | Self::Installing
                | Self::Restarting
                | Self::Verifying
                | Self::RollingBack
                | Self::RolledBack
                | Self::RollbackFailed
        )
    }

    /// Automatic rollback is a separate concern from remote-side-effect
    /// ambiguity. A completed failure needs rollback only after the live
    /// artifact replacement boundary has been crossed.
    fn has_live_artifact_replacement_started(self) -> bool {
        matches!(
            self,
            Self::Installing
                | Self::Restarting
                | Self::Verifying
                | Self::RollingBack
                | Self::RolledBack
                | Self::RollbackFailed
        )
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::RolledBack | Self::RollbackFailed
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deployment {
    id: DeploymentId,
    application: ApplicationId,
    environment: EnvironmentId,
    mechanism_kind: DeploymentMechanismKind,
    release_identity: ReleaseIdentity,
    state: DeploymentState,
}

impl Deployment {
    /// Backward-compatible constructor for the v0.1 local JAR request model.
    pub fn new(
        id: DeploymentId,
        application: ApplicationId,
        environment: EnvironmentId,
        artifact: Artifact,
    ) -> Self {
        Self::new_with_release(
            id,
            application,
            environment,
            DeploymentMechanismKind::JarSystemd,
            ReleaseIdentity::LocalFile(artifact),
        )
    }

    pub fn new_with_release(
        id: DeploymentId,
        application: ApplicationId,
        environment: EnvironmentId,
        mechanism_kind: DeploymentMechanismKind,
        release_identity: ReleaseIdentity,
    ) -> Self {
        Self {
            id,
            application,
            environment,
            mechanism_kind,
            release_identity,
            state: DeploymentState::Created,
        }
    }

    /// Backward-compatible rehydration for pre-v0.2 rows that only contain the
    /// legacy local-file identity columns.
    pub(crate) fn rehydrate(
        id: DeploymentId,
        application: ApplicationId,
        environment: EnvironmentId,
        artifact: Artifact,
        state: DeploymentState,
    ) -> Self {
        Self::rehydrate_with_release(
            id,
            application,
            environment,
            DeploymentMechanismKind::JarSystemd,
            ReleaseIdentity::LocalFile(artifact),
            state,
        )
    }

    pub(crate) fn rehydrate_with_release(
        id: DeploymentId,
        application: ApplicationId,
        environment: EnvironmentId,
        mechanism_kind: DeploymentMechanismKind,
        release_identity: ReleaseIdentity,
        state: DeploymentState,
    ) -> Self {
        Self {
            id,
            application,
            environment,
            mechanism_kind,
            release_identity,
            state,
        }
    }

    pub fn id(&self) -> &DeploymentId {
        &self.id
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

    pub fn release_identity(&self) -> &ReleaseIdentity {
        &self.release_identity
    }

    /// Compatibility projection for the v0.1 JAR-only call sites. New generic
    /// orchestration and persistence code should use `release_identity()`.
    pub fn artifact(&self) -> &Artifact {
        self.release_identity
            .local_file()
            .expect("artifact() is only valid for local-file deployments")
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

    pub fn fail(&mut self, rollback_available: bool) -> Result<(), DeploymentError> {
        let next = if self.state.has_live_artifact_replacement_started() && rollback_available {
            DeploymentState::RollingBack
        } else {
            DeploymentState::Failed
        };
        self.transition(next)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DeploymentError {
    #[error("invalid identifier: {0}")]
    InvalidIdentifier(&'static str),
    #[error("artifact version must not be empty")]
    InvalidArtifactVersion,
    #[error("artifact size must be greater than zero")]
    InvalidArtifactSize,
    #[error("artifact sha256 must contain exactly 64 hexadecimal characters")]
    InvalidArtifactChecksum,
    #[error("container image repository must not be empty")]
    InvalidContainerRepository,
    #[error("container image digest must be immutable sha256:<64-hex>")]
    InvalidContainerDigest,
    #[error("invalid deployment transition: {from:?} -> {to:?}")]
    InvalidTransition {
        from: DeploymentState,
        to: DeploymentState,
    },
    #[error("deployment plan target must not be empty")]
    InvalidPlanTarget,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn deployment() -> Deployment {
        Deployment::new(
            DeploymentId::new("d1").unwrap(),
            ApplicationId::new("app").unwrap(),
            EnvironmentId::new("test").unwrap(),
            Artifact::new("1.0.0", 1, SHA256).unwrap(),
        )
    }

    #[test]
    fn artifact_rejects_invalid_identity_fields() {
        assert_eq!(
            Artifact::new("", 1, SHA256).unwrap_err(),
            DeploymentError::InvalidArtifactVersion
        );
        assert_eq!(
            Artifact::new("1.0.0", 0, SHA256).unwrap_err(),
            DeploymentError::InvalidArtifactSize
        );
        assert_eq!(
            Artifact::new("1.0.0", 1, "abc").unwrap_err(),
            DeploymentError::InvalidArtifactChecksum
        );
    }

    #[test]
    fn artifact_normalizes_checksum_case() {
        let artifact = Artifact::new("1.0.0", 42, SHA256.to_ascii_uppercase()).unwrap();
        assert_eq!(artifact.version(), "1.0.0");
        assert_eq!(artifact.size_bytes(), 42);
        assert_eq!(artifact.sha256(), SHA256);
    }

    #[test]
    fn release_identity_supports_local_file_and_immutable_container_identity() {
        let local = ReleaseIdentity::LocalFile(Artifact::new("1.0.0", 42, SHA256).unwrap());
        assert_eq!(local.version(), "1.0.0");
        assert!(local.local_file().is_some());

        let image = ContainerImageReleaseIdentity::new(
            "2.0.0",
            "registry.example.com/demo",
            format!("sha256:{}", SHA256.to_ascii_uppercase()),
        )
        .unwrap();
        assert_eq!(image.digest(), format!("sha256:{SHA256}"));
        let container = ReleaseIdentity::ContainerImage(image);
        assert_eq!(container.version(), "2.0.0");
        assert!(container.container_image().is_some());
    }

    #[test]
    fn mutable_or_invalid_container_identity_is_rejected() {
        assert_eq!(
            ContainerImageReleaseIdentity::new("1.0.0", "", format!("sha256:{SHA256}"))
                .unwrap_err(),
            DeploymentError::InvalidContainerRepository
        );
        assert_eq!(
            ContainerImageReleaseIdentity::new("1.0.0", "repo/demo", "latest").unwrap_err(),
            DeploymentError::InvalidContainerDigest
        );
    }

    #[test]
    fn legacy_constructor_maps_to_jar_systemd_local_file_identity() {
        let deployment = deployment();
        assert_eq!(
            deployment.mechanism_kind(),
            DeploymentMechanismKind::JarSystemd
        );
        assert_eq!(deployment.release_identity().version(), "1.0.0");
        assert_eq!(deployment.artifact().sha256(), SHA256);
    }

    #[test]
    fn transition_matrix_is_exhaustive() {
        use DeploymentState::*;

        let states = [
            Created,
            Prechecking,
            StagingArtifact,
            BackingUp,
            Installing,
            Restarting,
            Verifying,
            Succeeded,
            Failed,
            RollingBack,
            RolledBack,
            RollbackFailed,
        ];
        let allowed = [
            (Created, Prechecking),
            (Prechecking, StagingArtifact),
            (Prechecking, Failed),
            (StagingArtifact, BackingUp),
            (StagingArtifact, Failed),
            (BackingUp, Installing),
            (BackingUp, Failed),
            (Installing, Restarting),
            (Installing, RollingBack),
            (Installing, Failed),
            (Restarting, Verifying),
            (Restarting, RollingBack),
            (Restarting, Failed),
            (Verifying, Succeeded),
            (Verifying, RollingBack),
            (Verifying, Failed),
            (RollingBack, RolledBack),
            (RollingBack, RollbackFailed),
        ];

        for from in states {
            for to in states {
                assert_eq!(
                    from.can_transition_to(to),
                    allowed.contains(&(from, to)),
                    "unexpected transition verdict for {from:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn success_is_only_reachable_after_verification() {
        use DeploymentState::*;
        let states = [
            Created,
            Prechecking,
            StagingArtifact,
            BackingUp,
            Installing,
            Restarting,
            Verifying,
            Succeeded,
            Failed,
            RollingBack,
            RolledBack,
            RollbackFailed,
        ];

        for state in states {
            assert_eq!(
                state.can_transition_to(Succeeded),
                state == Verifying,
                "only verifying may transition to succeeded"
            );
        }
    }

    #[test]
    fn remote_side_effect_ambiguity_begins_before_live_artifact_replacement() {
        assert!(!DeploymentState::Prechecking.has_live_mutation_started());
        assert!(DeploymentState::StagingArtifact.has_live_mutation_started());
        assert!(DeploymentState::BackingUp.has_live_mutation_started());
        assert!(DeploymentState::Installing.has_live_mutation_started());
    }

    #[test]
    fn failure_before_live_artifact_replacement_does_not_rollback() {
        let mut deployment = deployment();
        deployment.transition(DeploymentState::Prechecking).unwrap();
        deployment
            .transition(DeploymentState::StagingArtifact)
            .unwrap();
        deployment.transition(DeploymentState::BackingUp).unwrap();
        deployment.fail(true).unwrap();
        assert_eq!(deployment.state(), DeploymentState::Failed);
    }

    #[test]
    fn failure_after_live_mutation_enters_rollback_when_available() {
        let mut deployment = deployment();
        deployment.transition(DeploymentState::Prechecking).unwrap();
        deployment
            .transition(DeploymentState::StagingArtifact)
            .unwrap();
        deployment.transition(DeploymentState::BackingUp).unwrap();
        deployment.transition(DeploymentState::Installing).unwrap();
        deployment.fail(true).unwrap();
        assert_eq!(deployment.state(), DeploymentState::RollingBack);
    }

    #[test]
    fn post_mutation_failure_without_rollback_finishes_failed() {
        let mut deployment = deployment();
        deployment.transition(DeploymentState::Prechecking).unwrap();
        deployment
            .transition(DeploymentState::StagingArtifact)
            .unwrap();
        deployment.transition(DeploymentState::BackingUp).unwrap();
        deployment.transition(DeploymentState::Installing).unwrap();
        deployment.fail(false).unwrap();
        assert_eq!(deployment.state(), DeploymentState::Failed);
    }

    #[test]
    fn terminal_failure_states_cannot_resume() {
        use DeploymentState::*;
        assert!(Failed.is_terminal());
        assert!(RolledBack.is_terminal());
        assert!(RollbackFailed.is_terminal());
        assert!(!RollingBack.is_terminal());
        assert!(!Created.can_transition_to(Succeeded));
    }
}
