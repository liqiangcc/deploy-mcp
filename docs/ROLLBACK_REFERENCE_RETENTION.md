# Rollback Reference Retention

## Goal

Bound how long deploy-mcp keeps executable rollback capability snapshots without deleting the lifecycle facts needed for audit/history.

A rollback reference contains two different kinds of data:

- lifecycle metadata: deployment id, application/environment identity, state, and timestamps;
- executable capability snapshot: target, backup/install paths, and rollback/restart/health task names.

The retention policy cleans only the second category.

## Policy

`runtime.rollback_reference_retention_days` defaults to 30 days and is bounded to `1..=3650` days.

`runtime.rollback_reference_cleanup_batch_size` defaults to 500 and is bounded to `1..=5000` references per startup.

On startup, after crash recovery and before `remote-exec-mcp` is spawned, deploy-mcp prunes capability snapshots for references that:

- are `superseded` or `consumed`;
- have been inactive since at least the configured cutoff;
- have not already been pruned.

`active` references are never pruned. A failed explicit rollback keeps its reference active, so it remains retryable and is outside retention cleanup.

## Durable representation

Pruning is atomic in SQLite. deploy-mcp records a durable marker in `rollback_reference_retention` and clears the executable snapshot fields in the same transaction.

The original `rollback_references` row remains. This preserves the existing `recorded`, `superseded`, and `consumed` audit/history events without creating a duplicate event store.

After pruning, normal rollback lookup treats the reference as unavailable. The empty capability fields are retention storage only and are never reconstructed into an executable `RollbackReference`.

## Safety boundary

Retention has no `RemoteExecutionPort` dependency and performs no target inspection, SSH/SFTP operation, named task execution, rollback, or recovery inference.

Cleanup never releases a deployment/rollback mutation guard: only inactive references are eligible, while active rollback references, non-terminal deployments, started rollback operations, and unresolved recovery incidents keep their existing safety semantics.

The batch limit intentionally bounds startup maintenance work. Additional eligible references are handled on later starts rather than turning startup into an unbounded cleanup job.
