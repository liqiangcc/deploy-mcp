use serde::{Deserialize, Serialize};
use thiserror::Error;

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

/// Explicit operator evidence that a recovery-blocked environment was inspected
/// and is safe to mutate again. Identity fields are intentionally redundant: an
/// acknowledgement must match the incident exactly rather than merely naming an
/// integer row id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryAcknowledgement {
    incident_id: u64,
    application: String,
    environment: String,
    subject_kind: RecoverySubjectKind,
    subject_id: String,
    operator: String,
    evidence: String,
}

impl RecoveryAcknowledgement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        incident_id: u64,
        application: impl Into<String>,
        environment: impl Into<String>,
        subject_kind: RecoverySubjectKind,
        subject_id: impl Into<String>,
        operator: impl Into<String>,
        evidence: impl Into<String>,
    ) -> Result<Self, RecoveryError> {
        if incident_id == 0 {
            return Err(RecoveryError::InvalidIncidentId);
        }
        let application = non_empty("application", application.into())?;
        let environment = non_empty("environment", environment.into())?;
        let subject_id = non_empty("subject_id", subject_id.into())?;
        let operator = non_empty("operator", operator.into())?;
        let evidence = non_empty("evidence", evidence.into())?;
        Ok(Self {
            incident_id,
            application,
            environment,
            subject_kind,
            subject_id,
            operator,
            evidence,
        })
    }

    pub fn incident_id(&self) -> u64 {
        self.incident_id
    }

    pub fn application(&self) -> &str {
        &self.application
    }

    pub fn environment(&self) -> &str {
        &self.environment
    }

    pub fn subject_kind(&self) -> RecoverySubjectKind {
        self.subject_kind
    }

    pub fn subject_id(&self) -> &str {
        &self.subject_id
    }

    pub fn operator(&self) -> &str {
        &self.operator
    }

    pub fn evidence(&self) -> &str {
        &self.evidence
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryAcknowledgementRecord {
    acknowledgement: RecoveryAcknowledgement,
    acknowledged_at_unix_ms: i64,
}

impl RecoveryAcknowledgementRecord {
    pub(crate) fn rehydrate(
        acknowledgement: RecoveryAcknowledgement,
        acknowledged_at_unix_ms: i64,
    ) -> Self {
        Self {
            acknowledgement,
            acknowledged_at_unix_ms,
        }
    }

    pub fn acknowledgement(&self) -> &RecoveryAcknowledgement {
        &self.acknowledgement
    }

    pub fn acknowledged_at_unix_ms(&self) -> i64 {
        self.acknowledged_at_unix_ms
    }
}

fn non_empty(kind: &'static str, value: String) -> Result<String, RecoveryError> {
    if value.trim().is_empty() {
        Err(RecoveryError::EmptyField(kind))
    } else {
        Ok(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RecoveryError {
    #[error("recovery incident id must be greater than zero")]
    InvalidIncidentId,
    #[error("recovery acknowledgement field must not be empty: {0}")]
    EmptyField(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledgement_requires_explicit_operator_evidence() {
        assert_eq!(
            RecoveryAcknowledgement::new(
                1,
                "demo",
                "test",
                RecoverySubjectKind::Deployment,
                "d1",
                "operator@example",
                "",
            )
            .unwrap_err(),
            RecoveryError::EmptyField("evidence")
        );
    }

    #[test]
    fn acknowledgement_rejects_zero_incident_id() {
        assert_eq!(
            RecoveryAcknowledgement::new(
                0,
                "demo",
                "test",
                RecoverySubjectKind::Deployment,
                "d1",
                "operator@example",
                "verified service and artifact state",
            )
            .unwrap_err(),
            RecoveryError::InvalidIncidentId
        );
    }
}
