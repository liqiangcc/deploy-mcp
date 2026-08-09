# Structured deployment audit/history

## Purpose

`deploy-mcp` needs one machine-readable history view that can answer what happened to a deployment across normal deployment execution, explicit rollback, startup recovery, and operator reconciliation.

The history model is deliberately an **observation model**, not another workflow engine or state machine.

```text
DeploymentRepository facts
RollbackRepository facts
RecoveryRepository facts
        |
        v
   AuditRepository
   read-only projection
        |
        v
DeploymentApi
        |
        v
MCP disclosure filter
        |
        v
get_deployment_history
```

`AuditRepository` never starts remote work, changes a deployment state, validates rollback eligibility, acknowledges a recovery incident, or owns mutation policy. `DeployMcp` owns only the final AI-facing serialization/disclosure rule.

## No duplicate event store

The v0.1 implementation does not create a second `audit_events` write model.

The existing SQLite tables already contain the authoritative durable lifecycle facts:

- `deployments` records deployment identity and creation time;
- `deployment_transitions` is append-only state-transition history;
- `deployment_step_attempts` records started and terminal step attempts;
- `rollback_references` records deployment-bound rollback-reference lifecycle;
- `rollback_operations` records explicit rollback lifecycle;
- `recovery_incidents` records crash-recovery decisions;
- `recovery_acknowledgements` records controlled operator reconciliation.

`SqliteAuditRepository` performs a read-only SQL projection over those records. This avoids double-writing every lifecycle mutation into a second table and avoids creating two competing sources of truth.

Durability therefore comes from the existing transactional source records. Reopening the SQLite database reconstructs the same audit view without replaying an application-side log.

The trusted SQLite records may contain operator-useful free-form error text from failed remote work. That durable diagnostic text is **not** itself the AI disclosure contract. The final MCP adapter filters diagnostic/control fields before serialization.

## Audit subjects

Each event identifies the durable subject that produced the fact:

```text
deployment
rollback_reference
rollback_operation
recovery_incident
```

Every projected event is still correlated to the source `deployment_id`, so recovery of an interrupted explicit rollback can be traced back through its rollback operation to the deployment that created the rollback point.

## Stable event kinds

The structured history currently projects:

```text
deployment_created
deployment_transition
deployment_step_started
deployment_step_succeeded
deployment_step_failed
rollback_reference_recorded
rollback_reference_superseded
rollback_reference_consumed
rollback_operation_started
rollback_operation_succeeded
rollback_operation_failed
recovery_incident_recorded
recovery_acknowledged
```

Event-specific data is represented in a structured `attributes` object rather than requiring consumers to parse a lifecycle message string.

Examples:

```json
{
  "event": "deployment_transition",
  "attributes": {
    "from_state": "restarting",
    "to_state": "verifying"
  }
}
```

```json
{
  "event": "recovery_incident_recorded",
  "attributes": {
    "recovery_subject_kind": "rollback_operation",
    "recovery_subject_id": "rollback-operation-id",
    "previous_state": "started",
    "disposition": "manual_reconciliation_required",
    "reason": "process interrupted during explicit rollback"
  }
}
```

## MCP contract

The normal AI-facing server exposes a read-only tool:

```text
get_deployment_history
```

Input:

```json
{
  "deployment_id": "deployment-uuid",
  "limit": 200
}
```

`limit` defaults to 200 and is bounded to `1..=500` by the application layer.

Output shape:

```json
{
  "deployment_id": "deployment-uuid",
  "events": [
    {
      "deployment_id": "deployment-uuid",
      "application": "demo",
      "environment": "test",
      "subject": {
        "kind": "deployment",
        "id": "deployment-uuid"
      },
      "event": "deployment_created",
      "attributes": {
        "version": "1.2.3",
        "size_bytes": 12345,
        "sha256": "..."
      },
      "occurred_at_unix_ms": 0
    }
  ]
}
```

The MCP handler remains a protocol adapter. It does not query SQLite directly; it calls the application-owned `DeploymentApi`, which in turn uses the read-only `AuditRepository` port.

Before returning the event, the MCP disclosure filter recursively removes free-form diagnostic/control keys including:

```text
error
detail
stdout
stderr
evidence
shell
command
argv
credentials
```

This allows the persistence/audit layer to remain a faithful operator-facing observation source without making its free-form diagnostics part of the AI-facing protocol contract.

## Information boundary

The normal deployment-history tool intentionally does **not** expose remote execution capability details or remote task output merely because durable records persist them internally.

The AI-facing projection excludes:

- remote target identifiers from rollback capability snapshots;
- backup/install paths;
- rollback/restart/health task names;
- SSH credentials or any other credentials;
- shell fragments or raw commands;
- raw remote task stdout/stderr and free-form persisted error/detail bodies;
- operator reconciliation `evidence` text.

The operator identity is included on `recovery_acknowledged` so the history records who asserted reconciliation, but the full evidence remains in the administrative recovery store/CLI boundary.

This separation keeps the AI-facing history useful for diagnosis without turning an observation API into a capability-discovery, raw-log, or secret-disclosure surface.

## Ordering semantics

Events are rendered in ascending `occurred_at_unix_ms` order. SQLite row ids and a stable event-category rank provide deterministic rendering when multiple durable facts share the same millisecond timestamp.

That tie-break order is **not additional causal truth**. Millisecond timestamps can collide. Causal lifecycle rules remain defined by the source state machines, deployment transition ids, step-attempt ids, rollback-operation state, and recovery repository semantics.

Consumers should use the audit timeline to observe recorded facts, not to infer new legal state transitions from same-millisecond ordering.

## Failure and consistency model

Because v0.1 does not maintain a second audit write store, there is no best-effort "state changed but audit insert failed" side channel.

The projection sees only committed authoritative records. For example:

- deployment state and transition history are committed transactionally by `DeploymentRepository`;
- explicit rollback terminal state and rollback-reference consumption are committed transactionally by `RollbackRepository`;
- recovery termination plus incident creation are committed transactionally by `RecoveryRepository`;
- acknowledgement insertion plus incident resolution are committed transactionally by `RecoveryRepository`.

If one of those source transactions fails, the source lifecycle operation fails according to its existing fail-closed policy. `AuditRepository` does not weaken or bypass those boundaries.

## Safety evidence

The dedicated structured-history integration test uses separate repository instances against one temporary SQLite file and proves the following chain:

```text
deployment created/succeeded
  -> rollback reference recorded
  -> explicit rollback STARTED
  -> simulated process interruption
  -> rollback operation recovered as FAILED
  -> manual recovery incident recorded
  -> operator acknowledgement recorded
  -> SQLite connections closed/reopened
  -> same structured deployment history recovered
```

That integration test proves rollback target/path/task values and operator evidence are absent from the normal structured audit attributes. MCP adapter unit coverage additionally injects a sentinel into deployment/rollback failure text, a persisted step error, and nested audit diagnostic fields and proves the serialized AI-facing output does not contain the sentinel.
