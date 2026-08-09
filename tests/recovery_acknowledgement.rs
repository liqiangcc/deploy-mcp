use deploy_mcp::application::{
    RecoveryAcknowledgementRequest, RecoveryAdminService, StartupRecoveryService,
};
use deploy_mcp::domain::{
    ApplicationId, Artifact, Deployment, DeploymentId, DeploymentState, EnvironmentId,
    RecoverySubjectKind,
};
use deploy_mcp::error::ErrorCode;
use deploy_mcp::persistence::SqliteDeploymentRepository;
use deploy_mcp::ports::DeploymentRepository;
use deploy_mcp::recovery_persistence::SqliteRecoveryRepository;

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

fn create_manual_incident(database: &std::path::Path) -> u64 {
    let mut deployments = SqliteDeploymentRepository::open(database).unwrap();
    let mut interrupted = deployment("interrupted");
    deployments.create(&interrupted).unwrap();
    for state in [
        DeploymentState::Prechecking,
        DeploymentState::StagingArtifact,
        DeploymentState::BackingUp,
        DeploymentState::Installing,
    ] {
        advance(&mut deployments, &mut interrupted, state);
    }

    let mut startup =
        StartupRecoveryService::new(SqliteRecoveryRepository::open(database).unwrap());
    let report = startup.recover().unwrap();
    assert_eq!(report.manual_reconciliation_count(), 1);
    report.incidents()[0].id()
}

#[test]
fn exact_operator_acknowledgement_resolves_guard_and_is_durable() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("deployments.sqlite");
    let incident_id = create_manual_incident(&database);

    let mut deployments = SqliteDeploymentRepository::open(&database).unwrap();
    assert!(deployments
        .create(&deployment("blocked-before-ack"))
        .is_err());

    let mut admin = RecoveryAdminService::new(SqliteRecoveryRepository::open(&database).unwrap());
    let unresolved = admin.unresolved_incidents().unwrap();
    assert_eq!(unresolved.len(), 1);
    let incident = &unresolved[0];

    let record = admin
        .acknowledge(RecoveryAcknowledgementRequest {
            incident_id,
            application: incident.application().to_owned(),
            environment: incident.environment().to_owned(),
            subject_kind: incident.subject_kind(),
            subject_id: incident.subject_id().to_owned(),
            operator: "alice@example".to_owned(),
            evidence: "verified current artifact checksum and systemd service health on target"
                .to_owned(),
        })
        .unwrap();
    assert_eq!(record.acknowledgement().incident_id(), incident_id);
    assert_eq!(record.acknowledgement().operator(), "alice@example");
    assert!(admin.unresolved_incidents().unwrap().is_empty());

    deployments
        .create(&deployment("allowed-after-ack"))
        .unwrap();

    let reopened = RecoveryAdminService::new(SqliteRecoveryRepository::open(&database).unwrap());
    let durable = reopened.acknowledgement(incident_id).unwrap().unwrap();
    assert_eq!(
        durable.acknowledgement().evidence(),
        "verified current artifact checksum and systemd service health on target"
    );
    assert!(durable.acknowledged_at_unix_ms() > 0);
}

#[test]
fn identity_mismatch_cannot_release_recovery_guard() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("deployments.sqlite");
    let incident_id = create_manual_incident(&database);

    let mut admin = RecoveryAdminService::new(SqliteRecoveryRepository::open(&database).unwrap());
    let error = admin
        .acknowledge(RecoveryAcknowledgementRequest {
            incident_id,
            application: "demo".to_owned(),
            environment: "prod".to_owned(),
            subject_kind: RecoverySubjectKind::Deployment,
            subject_id: "interrupted".to_owned(),
            operator: "alice@example".to_owned(),
            evidence: "verified target manually".to_owned(),
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryIncidentConflict);
    assert_eq!(admin.unresolved_incidents().unwrap().len(), 1);

    let mut deployments = SqliteDeploymentRepository::open(&database).unwrap();
    assert!(deployments.create(&deployment("still-blocked")).is_err());
}

#[test]
fn acknowledgement_is_single_use_and_cannot_be_replayed() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("deployments.sqlite");
    let incident_id = create_manual_incident(&database);

    let mut admin = RecoveryAdminService::new(SqliteRecoveryRepository::open(&database).unwrap());
    let incident = admin.unresolved_incidents().unwrap().remove(0);
    let request = RecoveryAcknowledgementRequest {
        incident_id,
        application: incident.application().to_owned(),
        environment: incident.environment().to_owned(),
        subject_kind: incident.subject_kind(),
        subject_id: incident.subject_id().to_owned(),
        operator: "alice@example".to_owned(),
        evidence: "verified remote state".to_owned(),
    };

    admin.acknowledge(request.clone()).unwrap();
    let error = admin.acknowledge(request).unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryIncidentConflict);
}
