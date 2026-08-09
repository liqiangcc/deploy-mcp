//! Interfaces owned by the application core.
//!
//! Persistence and remote-execution adapters implement these traits. The core
//! depends only on these stable contracts, never on SQLite, SSH, or MCP crates.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::domain::{Deployment, DeploymentId, DeploymentState, DeploymentStep};

pub type RemoteExecutionResult<T> = Result<T, RemoteExecutionError>;
pub type RepositoryResult<T> = Result<T, RepositoryError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTargetCheck {
    pub reachable: bool,
    pub remote_identity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTransferResult {
    pub bytes_transferred: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTaskResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[async_trait]
pub trait RemoteExecutionPort: Send + Sync {
    async fn check_target(&self, target: &str) -> RemoteExecutionResult<RemoteTargetCheck>;

    async fn list_tasks(&self, target: &str) -> RemoteExecutionResult<BTreeSet<String>>;

    async fn upload_file(
        &self,
        target: &str,
        local_path: &str,
        remote_path: &str,
        overwrite: bool,
    ) -> RemoteExecutionResult<RemoteTransferResult>;

    async fn run_task(
        &self,
        target: &str,
        task: &str,
        parameters: BTreeMap<String, Value>,
    ) -> RemoteExecutionResult<RemoteTaskResult>;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RemoteExecutionError {
    #[error("remote-exec transport/protocol error: {0}")]
    Transport(String),
    #[error("remote-exec error {code}: {message}")]
    Remote { code: String, message: String },
    #[error("invalid response from remote-exec tool {tool}: {message}")]
    InvalidResponse { tool: String, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StepAttemptId(u64);

impl StepAttemptId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepAttemptStatus {
    Started,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentTransition {
    pub from: DeploymentState,
    pub to: DeploymentState,
    pub occurred_at_unix_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepAttemptRecord {
    pub id: StepAttemptId,
    pub step: DeploymentStep,
    pub status: StepAttemptStatus,
    pub error: Option<String>,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: Option<i64>,
}

pub trait DeploymentRepository {
    fn create(&mut self, deployment: &Deployment) -> RepositoryResult<()>;

    fn get(&self, id: &DeploymentId) -> RepositoryResult<Option<Deployment>>;

    fn list(
        &self,
        application: Option<&str>,
        environment: Option<&str>,
        limit: usize,
    ) -> RepositoryResult<Vec<Deployment>>;

    fn list_non_terminal(&self) -> RepositoryResult<Vec<Deployment>>;

    /// Atomically move the durable state and append the matching history row.
    ///
    /// `expected_from` provides optimistic concurrency protection. If durable
    /// state differs, no state or history mutation is committed.
    fn persist_transition(
        &mut self,
        id: &DeploymentId,
        expected_from: DeploymentState,
        to: DeploymentState,
    ) -> RepositoryResult<()>;

    fn transitions(&self, id: &DeploymentId) -> RepositoryResult<Vec<DeploymentTransition>>;

    fn start_step(
        &mut self,
        id: &DeploymentId,
        step: DeploymentStep,
    ) -> RepositoryResult<StepAttemptId>;

    fn finish_step(
        &mut self,
        attempt_id: StepAttemptId,
        status: StepAttemptStatus,
        error: Option<&str>,
    ) -> RepositoryResult<()>;

    fn step_attempts(&self, id: &DeploymentId) -> RepositoryResult<Vec<StepAttemptRecord>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RepositoryError {
    #[error("repository storage error: {0}")]
    Storage(String),
    #[error("deployment already exists: {0}")]
    AlreadyExists(String),
    #[error("deployment not found: {0}")]
    NotFound(String),
    #[error("deployment state conflict for {deployment_id}: expected {expected:?}")]
    StateConflict {
        deployment_id: String,
        expected: DeploymentState,
    },
    #[error("corrupt repository data: {0}")]
    CorruptData(String),
    #[error("invalid step-attempt completion")]
    InvalidStepAttemptCompletion,
}
