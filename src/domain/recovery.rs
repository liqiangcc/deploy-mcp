use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoverySubjectKind {
    Deployment,
    RollbackOperation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryDisposition {
    AutoResolved,
    ManualReconciliationRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryIncident {
    id: u64,
    subject_kind: RecoverySubjectKind,
    subject_id: String,
    application: String,
    environment: String,
    previous_state: String,
    disposition: RecoveryDisposition,
    reason: String,
    created_at_unix_ms: i64,
    resolved_at_unix_ms: Option<i64>,
}

#[allow(clippy::too_many_arguments)]
impl RecoveryIncident {
    pub(crate) fn rehydrate(
        id: u64,
        subject_kind: RecoverySubjectKind,
        subject_id: String,
        application: String,
        environment: String,
        previous_state: String,
        disposition: RecoveryDisposition,
        reason: String,
        created_at_unix_ms: i64,
        resolved_at_unix_ms: Option<i64>,
    ) -> Self {
        Self {
            id,
            subject_kind,
            subject_id,
            application,
            environment,
            previous_state,
            disposition,
            reason,
            created_at_unix_ms,
            resolved_at_unix_ms,
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn subject_kind(&self) -> RecoverySubjectKind {
        self.subject_kind
    }

    pub fn subject_id(&self) -> &str {
        &self.subject_id
    }

    pub fn application(&self) -> &str {
        &self.application
    }

    pub fn environment(&self) -> &str {
        &self.environment
    }

    pub fn previous_state(&self) -> &str {
        &self.previous_state
    }

    pub fn disposition(&self) -> RecoveryDisposition {
        self.disposition
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn created_at_unix_ms(&self) -> i64 {
        self.created_at_unix_ms
    }

    pub fn resolved_at_unix_ms(&self) -> Option<i64> {
        self.resolved_at_unix_ms
    }

    pub fn requires_manual_reconciliation(&self) -> bool {
        self.disposition == RecoveryDisposition::ManualReconciliationRequired
            && self.resolved_at_unix_ms.is_none()
    }
}
