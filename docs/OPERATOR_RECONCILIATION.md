# Operator recovery reconciliation

## Purpose

`deploy-mcp` deliberately fails closed when a process crash occurs after remote mutation may have started. In that case startup recovery creates an unresolved `manual_reconciliation_required` incident, and SQLite blocks further deployment and explicit rollback mutations for the affected `(application, environment)`.

An unresolved incident is not cleared automatically. A human operator must inspect the actual remote state, then record an explicit acknowledgement through the separate `deploy-mcp-recovery` administrative CLI.

This is an administrative safety path, not a normal MCP deployment capability.

## Boundary

The normal MCP server does **not** expose a tool that clears recovery incidents.

```text
AI / normal MCP tools
  -> cannot acknowledge recovery incidents

operator
  -> inspect remote state out of band
  -> deploy-mcp-recovery CLI
  -> RecoveryAdminService
  -> RecoveryRepository
  -> SQLite acknowledgement + incident resolution
```

`RecoveryAdminService` has no `RemoteExecutionPort` dependency. The acknowledgement path therefore cannot infer remote state, run remote commands, or silently convert an uncertain environment into a safe one.

## Inspect unresolved incidents

Use the same SQLite database as the deploy-mcp instance:

```bash
cargo run --bin deploy-mcp-recovery -- \
  --database deployments.sqlite \
  list
```

The command returns unresolved incidents as JSON. Record the exact values for:

- `id`;
- `application`;
- `environment`;
- `subject_kind`;
- `subject_id`;
- the original recovery reason and previous state.

Do not acknowledge an incident by guessing these values.

## Reconcile the remote environment

Before acknowledgement, inspect the actual target using approved operator procedures or separately authorized tools. The exact procedure depends on the application, but the operator should establish enough evidence to decide that another deployment mutation is safe.

For a JAR/systemd environment, useful checks normally include:

- which artifact is currently installed;
- artifact checksum/version when available;
- whether the systemd service is running;
- whether the configured health check passes;
- whether a partially completed install or rollback left staging/backup state that changes the next action.

This inspection is intentionally outside `deploy-mcp-recovery`. The CLI records the conclusion; it does not manufacture the conclusion.

## Acknowledge after inspection

Supply the complete incident identity plus operator and evidence:

```bash
cargo run --bin deploy-mcp-recovery -- \
  --database deployments.sqlite \
  acknowledge \
  --incident-id 12 \
  --application demo \
  --environment test \
  --subject-kind deployment \
  --subject-id 89d4c0e1-... \
  --operator alice@example \
  --evidence "verified installed artifact checksum and systemd health on target"
```

For an interrupted explicit rollback, use:

```text
--subject-kind rollback_operation
```

The repository rejects the acknowledgement unless all supplied identity fields exactly match the still-unresolved manual recovery incident.

## Atomic release of the mutation guard

A successful acknowledgement is one SQLite transaction:

```text
load incident
  -> require manual_reconciliation_required
  -> require unresolved
  -> require exact incident/application/environment/subject identity
  -> insert recovery_acknowledgements audit record
  -> set recovery_incidents.resolved_at_unix_ms
  -> COMMIT
```

Until the transaction commits, the existing recovery trigger continues to block deployment and explicit rollback inserts.

If validation, acknowledgement insertion, or incident resolution fails, the transaction does not release the guard.

## Audit semantics

The durable acknowledgement records:

- incident identity;
- application/environment;
- subject identity;
- operator string;
- evidence text;
- acknowledgement timestamp.

`operator` and `evidence` are audit fields. They are **not authentication or authorization mechanisms**. The operating environment must control who is allowed to execute the administrative binary and access the SQLite database.

A successful acknowledgement is single-use. Replaying acknowledgement for the already-resolved incident is rejected.

## Verify completion

Run the read-only list command again:

```bash
cargo run --bin deploy-mcp-recovery -- \
  --database deployments.sqlite \
  list
```

The acknowledged incident should no longer appear in the unresolved set. A later deployment or explicit rollback may then proceed through its ordinary safety checks.

## Safety invariants

- AI-facing deployment tools cannot clear an unresolved recovery incident.
- A bare incident id is insufficient to clear the guard.
- Empty operator/evidence fields are rejected.
- Wrong application/environment/subject identity does not clear the guard.
- Auto-resolved incidents cannot be manually acknowledged through this path.
- Already-resolved incidents cannot be acknowledged again.
- Acknowledgement audit insertion and incident resolution are atomic.
- No remote action occurs as part of acknowledgement.
- The durable SQLite guard remains the final correctness boundary.

## Evidence

`tests/recovery_acknowledgement.rs` uses real SQLite connections to prove that:

- an unresolved incident blocks a new deployment before acknowledgement;
- an exact acknowledgement durably records operator evidence and releases the guard;
- the acknowledgement survives reopening the database;
- an identity mismatch leaves the environment blocked;
- acknowledgement replay is rejected.
