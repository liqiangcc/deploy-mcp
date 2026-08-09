# Startup recovery contract

## Responsibility

Startup recovery handles orchestration records left active when a previous `deploy-mcp` process stopped unexpectedly.

It is intentionally separate from both the `Deployment` and `RollbackOperation` state machines:

```text
Deployment / RollbackOperation
        |
        | process interruption
        v
StartupRecoveryService
        |
        v
RecoveryRepository
        |
        v
RecoveryIncident
```

The recovery layer answers a different question from deployment state:

- deployment/rollback state: **did the orchestration finish normally?**
- recovery state: **is it safe to permit another mutation of this environment?**

A crash after live mutation may leave the remote host in a state that cannot be inferred from local SQLite records. Startup recovery therefore never guesses remote state and never invokes `RemoteExecutionPort`.

## Startup ordering

The binary runs recovery before starting `remote-exec-mcp`:

```text
load config
  -> open recovery repository / apply recovery migration
  -> reconcile interrupted local orchestration records
  -> persist recovery incidents
  -> only then open normal repositories
  -> only then start remote-exec-mcp
  -> serve MCP
```

If recovery persistence fails, startup fails closed before any remote child process is started.

## Interrupted deployment policy

The existing deployment mutation boundary remains authoritative: `INSTALLING` is the first state in which the live artifact may have changed.

### Before live mutation

For an interrupted deployment in:

```text
CREATED
PRECHECKING
STAGING_ARTIFACT
BACKING_UP
```

startup recovery can prove that deploy-mcp had not intentionally crossed the live-artifact mutation boundary.

It atomically:

1. changes the stale deployment record to `FAILED`;
2. marks any `STARTED` step attempt as `FAILED` with a recovery reason;
3. appends the durable transition from the interrupted state to `FAILED`;
4. writes an `auto_resolved` recovery incident with a resolved timestamp.

No unresolved environment guard remains, so a later deployment may proceed.

### After live mutation may have started

For an interrupted deployment in:

```text
INSTALLING
RESTARTING
VERIFYING
ROLLING_BACK
```

startup recovery does **not** retry install, restart, verification, or rollback. The remote state is treated as unknown.

It atomically:

1. terminates the stale orchestration record as `FAILED`;
2. marks any still-started step attempt as failed with the recovery reason;
3. records the terminal transition;
4. creates an unresolved `manual_reconciliation_required` recovery incident.

The unresolved incident is a durable mutation guard. SQLite rejects new deployments and explicit rollbacks for the same `(application, environment)` until an operator inspects the actual environment and records a valid acknowledgement through the separate administrative recovery path.

`FAILED` therefore means the old orchestration is no longer running; the unresolved `RecoveryIncident` separately means the environment is not yet proven safe for another mutation.

## Interrupted explicit rollback policy

A persisted `RollbackOperation` left in `STARTED` is always treated as remotely ambiguous because the restore/restart/health sequence may have partially executed.

Startup recovery:

1. marks the stale rollback operation `FAILED` with a recovery error;
2. leaves its `RollbackReference` unchanged and `ACTIVE`;
3. creates an unresolved `manual_reconciliation_required` incident;
4. blocks both a new deployment and another explicit rollback for that environment.

The active rollback reference is preserved as evidence/capability context. It must not be consumed merely because the deploy-mcp process crashed.

## Durable guard

Migration `002_startup_recovery.sql` owns the recovery safety constraint:

```text
recovery_incidents
  unique unresolved (application_id, environment_id)

unresolved incident
  -> BEFORE INSERT deployments: reject
  -> BEFORE INSERT rollback_operations: reject
```

The database is the correctness boundary. In-memory locks cannot satisfy restart or multi-process recovery safety.

Migration `003_operator_reconciliation.sql` adds the durable acknowledgement audit record. A valid operator acknowledgement inserts that audit record and resolves the incident in one SQLite transaction. Only the committed `resolved_at_unix_ms` removes the trigger condition.

## Operator reconciliation

Manual reconciliation is deliberately separate from startup recovery and from the normal MCP surface:

```text
operator inspection
  -> deploy-mcp-recovery CLI
  -> RecoveryAdminService
  -> RecoveryRepository
  -> exact incident validation
  -> acknowledgement audit + incident resolution
```

The normal MCP server does not expose a tool that clears recovery incidents. `RecoveryAdminService` also has no `RemoteExecutionPort` dependency: acknowledgement never runs remote commands or infers remote state.

An acknowledgement must provide the exact incident id, application, environment, subject kind, subject id, a non-empty operator value, and non-empty evidence. Any identity mismatch, replay, already-resolved incident, or non-manual incident remains fail-closed.

See `docs/OPERATOR_RECONCILIATION.md` for the operator procedure and CLI contract.

## Idempotence

Startup recovery updates use expected persisted states. Re-running startup recovery after a completed reconciliation does not create a duplicate incident or repeat a state transition.

An unresolved manual incident survives subsequent restarts and continues to block mutation. Operator acknowledgement is single-use: once an incident is resolved, the same acknowledgement cannot be replayed.

## Current boundary

Implemented now:

- detect interrupted deployments and started rollback operations;
- classify pre/post mutation interruption deterministically;
- terminate stale orchestration records durably;
- preserve step/transition history;
- persist resolved or unresolved recovery incidents;
- fail closed across process restarts and SQLite connections;
- perform no remote recovery work at startup;
- list unresolved incidents through a separate administrative CLI;
- require exact incident identity plus operator/evidence before acknowledgement;
- atomically persist acknowledgement evidence and resolve the incident;
- release the durable mutation guard only after that transaction commits.

Not implemented by this recovery subsystem:

- automated remote-state inference;
- automatic post-crash rollback;
- authentication/authorization for the operator identity string.

Automated remote-state inference or automatic post-crash rollback is intentionally not a v0.1 goal unless a future design can prove the remote state rather than infer it. The `operator` acknowledgement field is audit metadata, not authentication; operating-system/database access controls must restrict use of the administrative binary.

## Safety evidence

`tests/startup_recovery.rs` proves with real SQLite connections that:

- a pre-mutation interruption becomes `FAILED`, its active step is closed, the incident auto-resolves, and a later deployment is allowed;
- a post-mutation interruption becomes `FAILED` plus an unresolved incident, and a later deployment is blocked;
- a second startup is idempotent and preserves the unresolved guard;
- a `STARTED` explicit rollback becomes `FAILED`, its rollback reference remains active, and both rollback retry and new deployment remain blocked.

`tests/recovery_acknowledgement.rs` additionally proves that:

- the unresolved guard remains active before acknowledgement;
- an exact acknowledgement persists its audit record and releases the guard;
- the acknowledgement survives database reopen;
- identity mismatch leaves the incident unresolved and mutation blocked;
- acknowledgement replay is rejected.
