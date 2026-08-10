use deploy_mcp::audit_persistence::SqliteAuditRepository;
use deploy_mcp::domain::{
    ApplicationId, Artifact, Deployment, DeploymentId, DeploymentState, EnvironmentId,
    RollbackOperation, RollbackOperationId, RollbackOperationState, RollbackReference,
    RollbackReferenceState,
};
use deploy_mcp::persistence::SqliteDeploymentRepository;
use deploy_mcp::ports::{
    AuditEventKind, AuditRepository, DeploymentRepository, RollbackRepository,
    RollbackRetentionRepository,
};
use deploy_mcp::rollback_persistence::SqliteRollbackRepository;
use rusqlite::{params, Connection};

const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn create_succeeded_deployment(
    repository: &mut SqliteDeploymentRepository,
    id: &str,
    version: &str,
) -> Deployment {
    let mut deployment = Deployment::new(
        DeploymentId::new(id).unwrap(),
        ApplicationId::new("demo").unwrap(),
        EnvironmentId::new("test").unwrap(),
        Artifact::new(version, 1, SHA256).unwrap(),
    );
    repository.create(&deployment).unwrap();
    for next in [
        DeploymentState::Prechecking,
        DeploymentState::StagingArtifact,
        DeploymentState::BackingUp,
        DeploymentState::Installing,
        DeploymentState::Restarting,
        DeploymentState::Verifying,
        DeploymentState::Succeeded,
    ] {
        let from = deployment.state();
        repository
            .persist_transition(deployment.id(), from, next)
            .unwrap();
        deployment.transition(next).unwrap();
    }
    deployment
}

fn reference(deployment: &Deployment) -> RollbackReference {
    RollbackReference::new(
        deployment.id().clone(),
        deployment.application().clone(),
        deployment.environment().clone(),
        "test-server",
        format!("/backup/{}.jar", deployment.id().as_str()),
        "/opt/demo/demo.jar",
        "demo-rollback",
        "demo-restart",
        "demo-health",
    )
    .unwrap()
}

#[test]
fn retention_prunes_only_inactive_snapshots_in_deterministic_batches() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("deployments.sqlite");
    let mut deployments = SqliteDeploymentRepository::open(&database_path).unwrap();
    let mut rollbacks = SqliteRollbackRepository::open(&database_path).unwrap();

    let d1 = create_succeeded_deployment(&mut deployments, "d1", "1.0.0");
    rollbacks.record_reference(&reference(&d1)).unwrap();
    let d2 = create_succeeded_deployment(&mut deployments, "d2", "2.0.0");
    rollbacks.record_reference(&reference(&d2)).unwrap();
    let d3 = create_succeeded_deployment(&mut deployments, "d3", "3.0.0");
    rollbacks.record_reference(&reference(&d3)).unwrap();

    assert_eq!(
        rollbacks.get_reference(d1.id()).unwrap().unwrap().state(),
        RollbackReferenceState::Superseded
    );
    assert_eq!(
        rollbacks.get_reference(d2.id()).unwrap().unwrap().state(),
        RollbackReferenceState::Superseded
    );
    assert_eq!(
        rollbacks.get_reference(d3.id()).unwrap().unwrap().state(),
        RollbackReferenceState::Active
    );

    assert_eq!(
        rollbacks
            .prune_inactive_reference_snapshots(i64::MAX, 1)
            .unwrap(),
        1
    );
    assert!(rollbacks.get_reference(d1.id()).unwrap().is_none());
    assert!(rollbacks.get_reference(d2.id()).unwrap().is_some());
    assert!(rollbacks.get_reference(d3.id()).unwrap().is_some());

    assert_eq!(
        rollbacks
            .prune_inactive_reference_snapshots(i64::MAX, 1)
            .unwrap(),
        1
    );
    assert!(rollbacks.get_reference(d2.id()).unwrap().is_none());
    assert_eq!(
        rollbacks.get_reference(d3.id()).unwrap().unwrap().state(),
        RollbackReferenceState::Active
    );
    assert_eq!(
        rollbacks
            .prune_inactive_reference_snapshots(i64::MAX, 10)
            .unwrap(),
        0
    );

    let connection = Connection::open(&database_path).unwrap();
    let (state, target, backup_path): (String, String, String) = connection
        .query_row(
            "SELECT state, target, backup_path FROM rollback_references WHERE deployment_id = ?1",
            params![d1.id().as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, "superseded");
    assert!(target.is_empty());
    assert!(backup_path.is_empty());
    let retained: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM rollback_reference_retention WHERE deployment_id = ?1",
            params![d1.id().as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained, 1);
    let generic_snapshot_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM rollback_reference_model WHERE deployment_id = ?1",
            params![d1.id().as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(generic_snapshot_rows, 0);

    let audit = SqliteAuditRepository::open(&database_path).unwrap();
    let events = audit.events_for_deployment(d1.id(), 100).unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == AuditEventKind::RollbackReferenceRecorded));
    assert!(events
        .iter()
        .any(|event| event.kind == AuditEventKind::RollbackReferenceSuperseded));
}

#[test]
fn consumed_reference_snapshot_can_be_pruned_without_losing_operation_history() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("deployments.sqlite");
    let mut deployments = SqliteDeploymentRepository::open(&database_path).unwrap();
    let mut rollbacks = SqliteRollbackRepository::open(&database_path).unwrap();

    let deployment = create_succeeded_deployment(&mut deployments, "d1", "1.0.0");
    rollbacks.record_reference(&reference(&deployment)).unwrap();
    let operation = RollbackOperation::new(
        RollbackOperationId::new("r1").unwrap(),
        deployment.id().clone(),
        deployment.application().clone(),
        deployment.environment().clone(),
    );
    rollbacks.begin_operation(&operation).unwrap();
    rollbacks
        .finish_operation(
            operation.id(),
            deployment.id(),
            RollbackOperationState::Succeeded,
            None,
        )
        .unwrap();
    assert_eq!(
        rollbacks
            .get_reference(deployment.id())
            .unwrap()
            .unwrap()
            .state(),
        RollbackReferenceState::Consumed
    );

    assert_eq!(
        rollbacks
            .prune_inactive_reference_snapshots(i64::MAX, 10)
            .unwrap(),
        1
    );
    assert!(rollbacks.get_reference(deployment.id()).unwrap().is_none());
    assert_eq!(
        rollbacks
            .get_operation(operation.id())
            .unwrap()
            .unwrap()
            .state(),
        RollbackOperationState::Succeeded
    );

    let audit = SqliteAuditRepository::open(&database_path).unwrap();
    let events = audit.events_for_deployment(deployment.id(), 100).unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == AuditEventKind::RollbackReferenceConsumed));
    assert!(events
        .iter()
        .any(|event| event.kind == AuditEventKind::RollbackOperationStarted));
    assert!(events
        .iter()
        .any(|event| event.kind == AuditEventKind::RollbackOperationSucceeded));
}
