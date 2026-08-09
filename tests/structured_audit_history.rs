use deploy_mcp::audit_persistence::SqliteAuditRepository;
use deploy_mcp::domain::{
    ApplicationId, Artifact, Deployment, DeploymentId, DeploymentState, DeploymentStep,
    EnvironmentId, RecoveryAcknowledgement, RecoverySubjectKind, RollbackOperation,
    RollbackOperationId, RollbackReference,
};
use deploy_mcp::persistence::SqliteDeploymentRepository;
use deploy_mcp::ports::{
    AuditEventKind, AuditRepository, DeploymentRepository, RecoveryRepository, RollbackRepository,
    StepAttemptStatus,
};
use deploy_mcp::recovery_persistence::SqliteRecoveryRepository;
use deploy_mcp::rollback_persistence::SqliteRollbackRepository;

const SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const BACKUP_PATH: &str = "/opt/apps/demo/backup/demo.jar";
const INSTALL_PATH: &str = "/opt/apps/demo/demo.jar";
const EVIDENCE: &str = "verified service state and installed artifact checksum";

#[test]
fn structured_history_correlates_deploy_rollback_recovery_and_acknowledgement() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("deployments.sqlite");
    let deployment_id = DeploymentId::new("d-audit").unwrap();
    let application_id = ApplicationId::new("demo").unwrap();
    let environment_id = EnvironmentId::new("test").unwrap();
    let deployment = Deployment::new(
        deployment_id.clone(),
        application_id.clone(),
        environment_id.clone(),
        Artifact::new("1.2.3", 42, SHA256).unwrap(),
    );

    {
        let mut deployments = SqliteDeploymentRepository::open(&database_path).unwrap();
        deployments.create(&deployment).unwrap();
        let attempt = deployments
            .start_step(&deployment_id, DeploymentStep::Precheck)
            .unwrap();
        deployments
            .finish_step(attempt, StepAttemptStatus::Succeeded, None)
            .unwrap();

        let transitions = [
            (DeploymentState::Created, DeploymentState::Prechecking),
            (
                DeploymentState::Prechecking,
                DeploymentState::StagingArtifact,
            ),
            (DeploymentState::StagingArtifact, DeploymentState::BackingUp),
            (DeploymentState::BackingUp, DeploymentState::Installing),
            (DeploymentState::Installing, DeploymentState::Restarting),
            (DeploymentState::Restarting, DeploymentState::Verifying),
            (DeploymentState::Verifying, DeploymentState::Succeeded),
        ];
        for (from, to) in transitions {
            deployments
                .persist_transition(&deployment_id, from, to)
                .unwrap();
        }
    }

    let operation = {
        let mut rollbacks = SqliteRollbackRepository::open(&database_path).unwrap();
        let reference = RollbackReference::new(
            deployment_id.clone(),
            application_id.clone(),
            environment_id.clone(),
            "test-server",
            BACKUP_PATH,
            INSTALL_PATH,
            "demo-rollback",
            "demo-restart",
            "demo-health",
        )
        .unwrap();
        rollbacks.record_reference(&reference).unwrap();

        let operation = RollbackOperation::new(
            RollbackOperationId::new("rb-audit").unwrap(),
            deployment_id.clone(),
            application_id,
            environment_id,
        );
        rollbacks.begin_operation(&operation).unwrap();
        operation
    };

    {
        let mut recovery = SqliteRecoveryRepository::open(&database_path).unwrap();
        let incident = recovery
            .recover_rollback_operation(&operation, "process interrupted during explicit rollback")
            .unwrap()
            .unwrap();
        let acknowledgement = RecoveryAcknowledgement::new(
            incident.id(),
            incident.application(),
            incident.environment(),
            RecoverySubjectKind::RollbackOperation,
            incident.subject_id(),
            "operator@example",
            EVIDENCE,
        )
        .unwrap();
        recovery.acknowledge_incident(&acknowledgement).unwrap();
    }

    let expected_kinds = {
        let audit = SqliteAuditRepository::open(&database_path).unwrap();
        let events = audit.events_for_deployment(&deployment_id, 500).unwrap();
        let kinds = events.iter().map(|event| event.kind).collect::<Vec<_>>();

        assert_eq!(kinds.first(), Some(&AuditEventKind::DeploymentCreated));
        for required in [
            AuditEventKind::DeploymentTransition,
            AuditEventKind::DeploymentStepStarted,
            AuditEventKind::DeploymentStepSucceeded,
            AuditEventKind::RollbackReferenceRecorded,
            AuditEventKind::RollbackOperationStarted,
            AuditEventKind::RollbackOperationFailed,
            AuditEventKind::RecoveryIncidentRecorded,
            AuditEventKind::RecoveryAcknowledged,
        ] {
            assert!(
                kinds.contains(&required),
                "missing audit event: {required:?}"
            );
        }

        let incident = events
            .iter()
            .find(|event| event.kind == AuditEventKind::RecoveryIncidentRecorded)
            .unwrap();
        assert_eq!(
            incident.attributes["recovery_subject_kind"],
            "rollback_operation"
        );
        assert_eq!(incident.attributes["recovery_subject_id"], "rb-audit");
        assert_eq!(
            incident.attributes["disposition"],
            "manual_reconciliation_required"
        );

        let acknowledgement = events
            .iter()
            .find(|event| event.kind == AuditEventKind::RecoveryAcknowledged)
            .unwrap();
        assert_eq!(acknowledgement.attributes["operator"], "operator@example");

        let exposed = serde_json::to_string(
            &events
                .iter()
                .map(|event| &event.attributes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for secret_or_control in [
            BACKUP_PATH,
            INSTALL_PATH,
            "test-server",
            "demo-rollback",
            "demo-restart",
            "demo-health",
            EVIDENCE,
        ] {
            assert!(
                !exposed.contains(secret_or_control),
                "audit projection exposed restricted detail: {secret_or_control}"
            );
        }
        kinds
    };

    let reopened = SqliteAuditRepository::open(&database_path).unwrap();
    let reopened_kinds = reopened
        .events_for_deployment(&deployment_id, 500)
        .unwrap()
        .iter()
        .map(|event| event.kind)
        .collect::<Vec<_>>();
    assert_eq!(reopened_kinds, expected_kinds);
}
