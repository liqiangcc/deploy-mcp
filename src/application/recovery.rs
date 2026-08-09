use crate::domain::{
    RecoveryAcknowledgement, RecoveryAcknowledgementRecord, RecoveryDisposition, RecoveryIncident,
    RecoverySubjectKind,
};
use crate::error::{AppError, AppResult, ErrorCode};
use crate::ports::{RecoveryRepository, RepositoryError};

#[derive(Debug, Clone, Default)]
pub struct StartupRecoveryReport {
    incidents: Vec<RecoveryIncident>,
}

impl StartupRecoveryReport {
    pub fn incidents(&self) -> &[RecoveryIncident] {
        &self.incidents
    }

    pub fn manual_reconciliation_count(&self) -> usize {
        self.incidents
            .iter()
            .filter(|incident| incident.requires_manual_reconciliation())
            .count()
    }
}

/// Reconciles orchestration records left active by a previous process.
///
/// It never calls the remote execution port. Before any remote side-effecting
/// step has started, an interrupted deployment can be failed safely. Once
/// staging, backup, install, restart, verification, or rollback may have started,
/// the orchestration record is failed but a durable unresolved recovery incident
/// keeps the environment fail-closed until an operator inspects the remote state.
pub struct StartupRecoveryService<R>
where
    R: RecoveryRepository,
{
    repository: R,
}

impl<R> StartupRecoveryService<R>
where
    R: RecoveryRepository,
{
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    pub fn recover(&mut self) -> AppResult<StartupRecoveryReport> {
        let deployments = self
            .repository
            .interrupted_deployments()
            .map_err(repository_error)?;
        let mut incidents = Vec::new();

        for deployment in deployments {
            let disposition = if deployment.state().has_live_mutation_started() {
                RecoveryDisposition::ManualReconciliationRequired
            } else {
                RecoveryDisposition::AutoResolved
            };
            let reason = deployment_recovery_reason(deployment.state(), disposition);
            if let Some(incident) = self
                .repository
                .recover_deployment(&deployment, disposition, &reason)
                .map_err(repository_error)?
            {
                incidents.push(incident);
            }
        }

        let rollbacks = self
            .repository
            .started_rollback_operations()
            .map_err(repository_error)?;
        for operation in rollbacks {
            let reason = format!(
                "deploy-mcp restarted while explicit rollback {} was STARTED; remote rollback state is unknown, no remote action was guessed, and manual reconciliation is required",
                operation.id().as_str()
            );
            if let Some(incident) = self
                .repository
                .recover_rollback_operation(&operation, &reason)
                .map_err(repository_error)?
            {
                incidents.push(incident);
            }
        }

        Ok(StartupRecoveryReport { incidents })
    }

    pub fn unresolved_incidents(&self) -> AppResult<Vec<RecoveryIncident>> {
        self.repository
            .unresolved_incidents()
            .map_err(repository_error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryAcknowledgementRequest {
    pub incident_id: u64,
    pub application: String,
    pub environment: String,
    pub subject_kind: RecoverySubjectKind,
    pub subject_id: String,
    pub operator: String,
    pub evidence: String,
}

/// Administrative recovery use case. This service deliberately has no
/// RemoteExecutionPort dependency: the operator must inspect the remote system
/// outside this acknowledgement path, then submit exact incident identity plus
/// durable evidence of that inspection.
pub struct RecoveryAdminService<R>
where
    R: RecoveryRepository,
{
    repository: R,
}

impl<R> RecoveryAdminService<R>
where
    R: RecoveryRepository,
{
    pub fn new(repository: R) -> Self {
        Self { repository }
    }

    pub fn unresolved_incidents(&self) -> AppResult<Vec<RecoveryIncident>> {
        self.repository
            .unresolved_incidents()
            .map_err(repository_error)
    }

    pub fn acknowledge(
        &mut self,
        request: RecoveryAcknowledgementRequest,
    ) -> AppResult<RecoveryAcknowledgementRecord> {
        let acknowledgement = RecoveryAcknowledgement::new(
            request.incident_id,
            request.application,
            request.environment,
            request.subject_kind,
            request.subject_id,
            request.operator,
            request.evidence,
        )
        .map_err(|error| AppError::new(ErrorCode::InvalidRequest, error.to_string()))?;

        self.repository
            .acknowledge_incident(&acknowledgement)
            .map_err(repository_error)
    }

    pub fn acknowledgement(
        &self,
        incident_id: u64,
    ) -> AppResult<Option<RecoveryAcknowledgementRecord>> {
        if incident_id == 0 {
            return Err(AppError::new(
                ErrorCode::InvalidRequest,
                "recovery incident id must be greater than zero",
            ));
        }
        self.repository
            .acknowledgement(incident_id)
            .map_err(repository_error)
    }
}

fn deployment_recovery_reason(
    state: crate::domain::DeploymentState,
    disposition: RecoveryDisposition,
) -> String {
    match disposition {
        RecoveryDisposition::AutoResolved => format!(
            "deploy-mcp restarted while deployment was {state:?}; no remote side-effecting deployment step had started, so the interrupted orchestration was failed without remote recovery work"
        ),
        RecoveryDisposition::ManualReconciliationRequired => format!(
            "deploy-mcp restarted while deployment was {state:?}; a remote side effect may have completed or may still be completing, remote state was not guessed, and manual reconciliation is required"
        ),
    }
}

fn repository_error(error: RepositoryError) -> AppError {
    match error {
        RepositoryError::RecoveryIncidentNotFound(incident_id) => AppError::new(
            ErrorCode::UnknownRecoveryIncident,
            format!("unknown recovery incident: {incident_id}"),
        ),
        RepositoryError::RecoveryIncidentConflict(message) => {
            AppError::new(ErrorCode::RecoveryIncidentConflict, message)
        }
        other => AppError::new(ErrorCode::PersistenceFailed, other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ApplicationId, Artifact, Deployment, DeploymentId, DeploymentState, EnvironmentId,
        RecoverySubjectKind, RollbackOperation, RollbackOperationId,
    };
    use crate::ports::{RecoveryRepository, RepositoryResult};

    const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[derive(Default)]
    struct FakeRecoveryRepository {
        deployments: Vec<Deployment>,
        rollbacks: Vec<RollbackOperation>,
        incidents: Vec<RecoveryIncident>,
        acknowledgements: Vec<RecoveryAcknowledgementRecord>,
    }

    impl RecoveryRepository for FakeRecoveryRepository {
        fn interrupted_deployments(&self) -> RepositoryResult<Vec<Deployment>> {
            Ok(self.deployments.clone())
        }

        fn started_rollback_operations(&self) -> RepositoryResult<Vec<RollbackOperation>> {
            Ok(self.rollbacks.clone())
        }

        fn recover_deployment(
            &mut self,
            deployment: &Deployment,
            disposition: RecoveryDisposition,
            reason: &str,
        ) -> RepositoryResult<Option<RecoveryIncident>> {
            let incident = RecoveryIncident::rehydrate(
                self.incidents.len() as u64 + 1,
                RecoverySubjectKind::Deployment,
                deployment.id().as_str().to_owned(),
                deployment.application().as_str().to_owned(),
                deployment.environment().as_str().to_owned(),
                format!("{:?}", deployment.state()),
                disposition,
                reason.to_owned(),
                1,
                (disposition == RecoveryDisposition::AutoResolved).then_some(1),
            );
            self.incidents.push(incident.clone());
            Ok(Some(incident))
        }

        fn recover_rollback_operation(
            &mut self,
            operation: &RollbackOperation,
            reason: &str,
        ) -> RepositoryResult<Option<RecoveryIncident>> {
            let incident = RecoveryIncident::rehydrate(
                self.incidents.len() as u64 + 1,
                RecoverySubjectKind::RollbackOperation,
                operation.id().as_str().to_owned(),
                operation.application().as_str().to_owned(),
                operation.environment().as_str().to_owned(),
                "Started".to_owned(),
                RecoveryDisposition::ManualReconciliationRequired,
                reason.to_owned(),
                1,
                None,
            );
            self.incidents.push(incident.clone());
            Ok(Some(incident))
        }

        fn unresolved_incidents(&self) -> RepositoryResult<Vec<RecoveryIncident>> {
            Ok(self
                .incidents
                .iter()
                .filter(|incident| incident.resolved_at_unix_ms().is_none())
                .cloned()
                .collect())
        }

        fn get_incident(&self, incident_id: u64) -> RepositoryResult<Option<RecoveryIncident>> {
            Ok(self
                .incidents
                .iter()
                .find(|incident| incident.id() == incident_id)
                .cloned())
        }

        fn acknowledge_incident(
            &mut self,
            acknowledgement: &RecoveryAcknowledgement,
        ) -> RepositoryResult<RecoveryAcknowledgementRecord> {
            let record = RecoveryAcknowledgementRecord::rehydrate(acknowledgement.clone(), 2);
            self.acknowledgements.push(record.clone());
            Ok(record)
        }

        fn acknowledgement(
            &self,
            incident_id: u64,
        ) -> RepositoryResult<Option<RecoveryAcknowledgementRecord>> {
            Ok(self
                .acknowledgements
                .iter()
                .find(|record| record.acknowledgement().incident_id() == incident_id)
                .cloned())
        }
    }

    fn deployment(state: DeploymentState) -> Deployment {
        Deployment::rehydrate(
            DeploymentId::new("d1").unwrap(),
            ApplicationId::new("demo").unwrap(),
            EnvironmentId::new("test").unwrap(),
            Artifact::new("1.0.0", 1, SHA256).unwrap(),
            state,
        )
    }

    #[test]
    fn recovery_policy_distinguishes_preflight_from_remote_side_effects() {
        let repository = FakeRecoveryRepository {
            deployments: vec![
                deployment(DeploymentState::Prechecking),
                Deployment::rehydrate(
                    DeploymentId::new("d2").unwrap(),
                    ApplicationId::new("demo2").unwrap(),
                    EnvironmentId::new("test").unwrap(),
                    Artifact::new("1.0.0", 1, SHA256).unwrap(),
                    DeploymentState::BackingUp,
                ),
            ],
            ..Default::default()
        };
        let mut service = StartupRecoveryService::new(repository);
        let report = service.recover().unwrap();
        assert_eq!(report.incidents().len(), 2);
        assert_eq!(
            report.incidents()[0].disposition(),
            RecoveryDisposition::AutoResolved
        );
        assert_eq!(
            report.incidents()[1].disposition(),
            RecoveryDisposition::ManualReconciliationRequired
        );
        assert_eq!(report.manual_reconciliation_count(), 1);
    }

    #[test]
    fn started_explicit_rollback_always_requires_manual_reconciliation() {
        let operation = RollbackOperation::new(
            RollbackOperationId::new("r1").unwrap(),
            DeploymentId::new("source").unwrap(),
            ApplicationId::new("demo").unwrap(),
            EnvironmentId::new("test").unwrap(),
        );
        let repository = FakeRecoveryRepository {
            rollbacks: vec![operation],
            ..Default::default()
        };
        let mut service = StartupRecoveryService::new(repository);
        let report = service.recover().unwrap();
        assert_eq!(report.manual_reconciliation_count(), 1);
        assert_eq!(
            report.incidents()[0].subject_kind(),
            RecoverySubjectKind::RollbackOperation
        );
    }

    #[test]
    fn admin_acknowledgement_requires_non_empty_evidence() {
        let mut service = RecoveryAdminService::new(FakeRecoveryRepository::default());
        let error = service
            .acknowledge(RecoveryAcknowledgementRequest {
                incident_id: 1,
                application: "demo".to_owned(),
                environment: "test".to_owned(),
                subject_kind: RecoverySubjectKind::Deployment,
                subject_id: "d1".to_owned(),
                operator: "operator@example".to_owned(),
                evidence: "".to_owned(),
            })
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidRequest);
    }
}
