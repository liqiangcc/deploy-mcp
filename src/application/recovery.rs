use crate::domain::{RecoveryDisposition, RecoveryIncident};
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
/// It never calls the remote execution port. Before the live-artifact mutation
/// boundary, an interrupted deployment can be failed safely. After that
/// boundary, the deployment/rollback execution is failed but a durable,
/// unresolved recovery incident keeps the environment fail-closed until an
/// operator-level reconciliation workflow is implemented and completed.
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

fn deployment_recovery_reason(
    state: crate::domain::DeploymentState,
    disposition: RecoveryDisposition,
) -> String {
    match disposition {
        RecoveryDisposition::AutoResolved => format!(
            "deploy-mcp restarted while deployment was {state:?}; live-artifact mutation had not started, so the interrupted orchestration was failed without remote recovery work"
        ),
        RecoveryDisposition::ManualReconciliationRequired => format!(
            "deploy-mcp restarted while deployment was {state:?}; live-artifact mutation may already have occurred, remote state was not guessed, and manual reconciliation is required"
        ),
    }
}

fn repository_error(error: RepositoryError) -> AppError {
    AppError::new(ErrorCode::PersistenceFailed, error.to_string())
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
    fn recovery_policy_distinguishes_pre_and_post_mutation_interruptions() {
        let repository = FakeRecoveryRepository {
            deployments: vec![
                deployment(DeploymentState::BackingUp),
                Deployment::rehydrate(
                    DeploymentId::new("d2").unwrap(),
                    ApplicationId::new("demo2").unwrap(),
                    EnvironmentId::new("test").unwrap(),
                    Artifact::new("1.0.0", 1, SHA256).unwrap(),
                    DeploymentState::Installing,
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
}
