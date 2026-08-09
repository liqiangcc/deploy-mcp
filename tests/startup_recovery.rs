use deploy_mcp::application::StartupRecoveryService;
use deploy_mcp::domain::{
    ApplicationId, Artifact, Deployment, DeploymentId, DeploymentState, DeploymentStep,
    EnvironmentId, RecoveryDisposition, RecoverySubjectKind, RollbackOperation,
    RollbackOperationId, RollbackOperationState, RollbackReference, RollbackReferenceState,
};
use deploy_mcp::persistence::SqliteDeploymentRepository;
use deploy_mcp::ports::{
    DeploymentRepository, RecoveryRepository, RepositoryError, RollbackRepository,
    StepAttemptStatus,
};
use deploy_mcp::recovery_persistence::SqliteRecoveryRepository;
use deploy_mcp::rollback_persistence::SqliteRollbackRepository;

const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn deployment(id: &str) -> Deployment {
    Deployment::new(
        DeploymentId::new(id).unwrap(),
        ApplicationId::new("demo").unwrap(),
        EnvironmentId::new("test").unwrap(),
        Artifact::new("1.0.0", 42, SHA256).unwrap(),
    )
}

fn advance(
    repository: &mut SqliteDeploymentRepository,
    deployment: &mut Deployment,
    next: DeploymentState,
) {
    let from = deployment.state();
    repository
        .persist_transition(deployment.id(), from, next)
        .unwrap();
    deployment.transition(next).unwrap();
}

fn succeed(repository: &mut SqliteDeploymentRepository, deployment: &mut Deployment) {
    for state in [
        DeploymentState::Prechecking,
        DeploymentState::StagingArtifact,
        DeploymentState::BackingUp,
        DeploymentState::Installing,
        DeploymentState::Restarting,
        DeploymentState::Verifying,
        DeploymentState::Succeeded,
    ] {
        advance(repository, deployment, state);
    }
}

fn rollback_reference(deployment: &Deployment) -> RollbackReference {
    RollbackReference::new(
        deployment.id().clone(),
        deployment.application().clone(),
        deployment.environment().clone(),
        "test-server",
        "/opt/apps/demo/backup/demo.jar",
        "/opt/apps/demo/demo.jar",
        "demo-rollback",
        "demo-restart",
        "demo-health",
    )
    .unwrap()
}

#[test]
fn pre_mutation_interruption_is_failed_and_auto_resolved() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("deployments.sqlite");
    let mut deployments = SqliteDeploymentRepository::open(&database).unwrap();
    let mut interrupted = deployment("pre-mutation");
    deployments.create(&interrupted).unwrap();
    advance(
        &mut deployments,
        &mut interrupted,
        DeploymentState::Prechecking,
    );
    advance(
        &mut deployments,
        &mut interrupted,
        DeploymentState::StagingArtifact,
    );
    advance(
        &mut deployments,
        &mut interrupted,
        DeploymentState::BackingUp,
    );
    deployments
        .start_step(interrupted.id(), DeploymentStep::BackupCurrent)
        .unwrap();

    let mut recovery =
        StartupRecoveryService::new(SqliteRecoveryRepository::open(&database).unwrap());
    let report = recovery.recover().unwrap();
    assert_eq!(report.incidents().len(), 1);
    assert_eq!(
        report.incidents()[0].disposition(),
        RecoveryDisposition::AutoResolved
    );
    assert!(report.incidents()[0].resolved_at_unix_ms().is_some());
    assert!(recovery.unresolved_incidents().unwrap().is_empty());

    let recovered = deployments.get(interrupted.id()).unwrap().unwrap();
    assert_eq!(recovered.state(), DeploymentState::Failed);
    let attempts = deployments.step_attempts(interrupted.id()).unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, StepAttemptStatus::Failed);
    assert!(attempts[0]
        .error
        .as_deref()
        .unwrap()
        .contains("live-artifact mutation had not started"));
    let transitions = deployments.transitions(interrupted.id()).unwrap();
    assert_eq!(transitions.last().unwrap().from, DeploymentState::BackingUp);
    assert_eq!(transitions.last().unwrap().to, DeploymentState::Failed);

    deployments.create(&deployment("next-safe-deploy")).unwrap();
}

#[test]
fn post_mutation_interruption_requires_manual_reconciliation_and_blocks_new_deploy() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("deployments.sqlite");
    let mut deployments = SqliteDeploymentRepository::open(&database).unwrap();
    let mut interrupted = deployment("post-mutation");
    deployments.create(&interrupted).unwrap();
    for state in [
        DeploymentState::Prechecking,
        DeploymentState::StagingArtifact,
        DeploymentState::BackingUp,
        DeploymentState::Installing,
    ] {
        advance(&mut deployments, &mut interrupted, state);
    }
    deployments
        .start_step(interrupted.id(), DeploymentStep::Install)
        .unwrap();

    let mut recovery =
        StartupRecoveryService::new(SqliteRecoveryRepository::open(&database).unwrap());
    let report = recovery.recover().unwrap();
    assert_eq!(report.manual_reconciliation_count(), 1);
    assert_eq!(
        report.incidents()[0].subject_kind(),
        RecoverySubjectKind::Deployment
    );
    assert_eq!(
        report.incidents()[0].disposition(),
        RecoveryDisposition::ManualReconciliationRequired
    );
    assert!(report.incidents()[0].resolved_at_unix_ms().is_none());
    assert_eq!(recovery.unresolved_incidents().unwrap().len(), 1);
    assert_eq!(
        deployments.get(interrupted.id()).unwrap().unwrap().state(),
        DeploymentState::Failed
    );

    let error = deployments
        .create(&deployment("blocked-deploy"))
        .unwrap_err();
    assert!(matches!(error, RepositoryError::AlreadyExists(_)));

    let mut second_recovery =
        StartupRecoveryService::new(SqliteRecoveryRepository::open(&database).unwrap());
    assert!(second_recovery.recover().unwrap().incidents().is_empty());
    assert_eq!(second_recovery.unresolved_incidents().unwrap().len(), 1);
}

#[test]
fn interrupted_explicit_rollback_is_failed_but_keeps_reference_and_blocks_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("deployments.sqlite");
    let mut deployments = SqliteDeploymentRepository::open(&database).unwrap();
    let mut source = deployment("source-deployment");
    deployments.create(&source).unwrap();
    succeed(&mut deployments, &mut source);

    let mut rollbacks = SqliteRollbackRepository::open(&database).unwrap();
    let reference = rollback_reference(&source);
    rollbacks.record_reference(&reference).unwrap();
    let operation = RollbackOperation::new(
        RollbackOperationId::new("interrupted-rollback").unwrap(),
        source.id().clone(),
        source.application().clone(),
        source.environment().clone(),
    );
    rollbacks.begin_operation(&operation).unwrap();

    let mut recovery =
        StartupRecoveryService::new(SqliteRecoveryRepository::open(&database).unwrap());
    let report = recovery.recover().unwrap();
    assert_eq!(report.manual_reconciliation_count(), 1);
    assert_eq!(
        report.incidents()[0].subject_kind(),
        RecoverySubjectKind::RollbackOperation
    );

    let recovered_operation = rollbacks.get_operation(operation.id()).unwrap().unwrap();
    assert_eq!(recovered_operation.state(), RollbackOperationState::Failed);
    assert_eq!(
        rollbacks
            .get_reference(source.id())
            .unwrap()
            .unwrap()
            .state(),
        RollbackReferenceState::Active
    );

    let retry = RollbackOperation::new(
        RollbackOperationId::new("blocked-retry").unwrap(),
        source.id().clone(),
        source.application().clone(),
        source.environment().clone(),
    );
    let error = rollbacks.begin_operation(&retry).unwrap_err();
    assert!(matches!(error, RepositoryError::MutationConflict { .. }));

    let error = deployments
        .create(&deployment("blocked-after-rollback"))
        .unwrap_err();
    assert!(matches!(error, RepositoryError::AlreadyExists(_)));
}
