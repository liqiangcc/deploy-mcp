# Threat model and regression boundary

## Purpose

`deploy-mcp` is intentionally a deployment orchestrator, not a general remote administration service. Its security model assumes an AI/MCP caller may be mistaken, adversarial, replaying old requests, or attempting to smuggle lower-level execution controls into a deployment request.

The central rule is:

> AI controls deployment intent; deterministic code controls filesystem, persistence, rollback, and remote-execution capability boundaries.

This document defines the v0.1 threat boundary and the regression tests that must remain green as the implementation evolves.

## Trust boundaries

```text
AI / MCP caller
  | untrusted deployment intent
  v
MCP adapter
  | typed, deny-unknown-fields request DTOs
  v
Application / domain
  | configured app/env/task/path semantics
  | local artifact allowlist
  | idempotency + mutation guards
  | durable state / rollback references / recovery incidents
  v
RemoteExecutionPort
  | narrow typed operations only
  v
remote-exec-mcp
  | independent target/task/transfer/SSH/SFTP policy
  v
Remote Linux host
```

Separate OS boundaries protect the deploy-mcp configuration, SQLite database, executable, and configured local artifact roots.

## Threat actors and assumptions

The v0.1 model treats these as untrusted:

- MCP callers and AI-generated tool arguments;
- caller-supplied `artifact_path` values;
- replayed or duplicated deployment requests;
- stale rollback requests;
- remote failures, transport errors, and timeouts whose final remote state may be unknown.

The v0.1 model assumes:

- the OS account and administrators controlling deploy-mcp configuration and SQLite storage are trusted;
- processes with write access to configured local artifact roots are trusted artifact producers;
- `remote-exec-mcp` independently enforces its target, task, local-transfer, remote-path, authentication, and SSH/SFTP policies;
- compromise of deploy-mcp's host OS, SQLite file, or remote-exec-mcp itself is outside this application-layer threat model.

## Threats and controls

### 1. Raw execution injection

Threat: a caller attempts to turn deployment into arbitrary remote administration by supplying shell commands, credentials, target overrides, task names, or remote paths.

Controls:

- MCP request DTOs use `deny_unknown_fields`;
- `deploy_application` exposes application, environment, version, local artifact path, and optional idempotency key only;
- `rollback_deployment` exposes only `deployment_id`;
- `RemoteExecutionPort` exposes typed target checks, task discovery, file upload, and named task execution rather than raw shell;
- task names and remote deployment paths come from configuration or a durable rollback reference, not caller input.

Regression coverage:

- `tests/threat_model_regressions.rs::deploy_mcp_rejects_caller_controlled_execution_and_remote_destination_fields`
- `tests/threat_model_regressions.rs::rollback_and_history_mcp_inputs_reject_remote_execution_controls`
- existing MCP adapter tests in `src/mcp.rs`.

### 2. Local filesystem read escape

Threat: a caller uses an absolute path, `..`, or a symlink to make deploy-mcp read a file outside the intended artifact directory.

Controls:

- `local_artifacts.allowed_roots` is an explicit capability list;
- an omitted/empty list disables new local-artifact deployments;
- roots must be bounded, absolute, non-filesystem-root paths without parent components;
- roots and requested artifacts are canonicalized;
- canonical artifact containment is checked before hashing, reservation, or remote work;
- the same canonical path is used for hashing and upload delegation.

Regression coverage:

- `tests/threat_model_regressions.rs::missing_local_artifact_capability_denies_before_remote_work_or_persistence`
- `tests/threat_model_regressions.rs::symlink_escape_denies_before_remote_work_or_persistence`
- `tests/threat_model_regressions.rs::unsafe_artifact_capability_roots_are_rejected_at_configuration_boundary`
- focused unit tests in `src/application/artifact_access.rs` and `src/config.rs`.

### 3. Duplicate or replayed mutation

Threat: retries, races, or multiple processes cause the same environment to be mutated more than once unintentionally.

Controls:

- process-local deployment leases;
- SQLite mutation constraints across repository/process instances;
- bounded idempotency keys durably bind one complete deployment intent;
- same application/environment/version cannot silently change artifact identity;
- stale expected-state writes fail atomically.

Regression coverage:

- `tests/active_deployment_lock.rs`
- `tests/deployment_idempotency.rs`
- persistence concurrency/state-conflict tests.

### 4. Rollback confused-deputy attack

Threat: a caller supplies a historical deployment id but replaces the target, backup path, install path, or rollback task so deploy-mcp performs an unrelated mutation.

Controls:

- the rollback MCP tool accepts only `deployment_id`;
- a successful deployment may create a durable deployment-bound `RollbackReference`;
- the reference snapshots target/path/task capability identity;
- current environment configuration must still match the snapshot;
- newer deployment history conservatively invalidates older rollback eligibility;
- rollback is represented by an independent `RollbackOperation`; the terminal source deployment is never reopened.

Regression coverage:

- `tests/explicit_rollback.rs`
- raw rollback input rejection in `tests/threat_model_regressions.rs` and `src/mcp.rs`.

### 5. Ambiguous timeout or crash state

Threat: a remote action may have completed even though deploy-mcp timed out or crashed, and immediately retrying or rolling back could duplicate or compound mutation.

Controls:

- timeouts after the live mutation boundary preserve a non-terminal/unknown durable guard rather than claiming a terminal result;
- started explicit rollback remains durably guarded when completion is unknown;
- startup recovery never guesses remote state;
- unresolved recovery incidents block later deployment/rollback mutation until explicit operator reconciliation.

Regression coverage:

- `tests/timeouts.rs`
- `tests/startup_recovery.rs`
- `tests/recovery_acknowledgement.rs`.

### 6. Information disclosure through history tools

Threat: read-only history becomes an indirect way to obtain credentials, shell controls, or executable rollback snapshot details.

Controls:

- audit history is projected from durable facts through `AuditRepository`;
- normal AI-facing history exposes bounded structured lifecycle information;
- rollback target/path/task snapshot details and operator reconciliation evidence are excluded from the normal history projection;
- history arguments reject remote execution parameters.

Regression coverage:

- `tests/structured_audit_history.rs`
- MCP history parameter rejection in `tests/threat_model_regressions.rs` and `src/mcp.rs`.

### 7. Security-boundary collapse with remote-exec-mcp

Threat: deploy-mcp starts implementing SSH/SFTP or bypasses remote-exec-mcp policy by exposing generic protocol calls.

Controls:

- domain/application depend on `RemoteExecutionPort`, not SSH/SFTP libraries;
- the real adapter invokes only the remote-exec MCP capabilities needed by deployment;
- local artifact authorization is independent of remote-exec transfer authorization;
- no raw `call_tool(name, arbitrary_json)` surface is exposed to deployment orchestration.

Regression coverage:

- adapter contract/unit tests in `src/adapters.rs`;
- Phase 4 acceptance tests and dependency review in CI.

## Fail-closed ordering requirements

Security-sensitive rejection should happen at the earliest owning boundary:

```text
invalid MCP field
  -> reject during DTO decoding

invalid/disabled local artifact capability
  -> reject before artifact hashing
  -> no deployment reservation
  -> no RemoteExecutionPort call

active mutation/recovery guard
  -> reject before new remote work

missing remote target/task capability
  -> fail during PRECHECKING
  -> no live artifact mutation

unknown post-mutation timeout/crash result
  -> retain durable guard
  -> require recovery/reconciliation rather than guessing
```

Tests should assert not only the error code but also the absence of remote calls and durable mutation where that is part of the boundary contract.

## Residual risks and explicit non-goals

The local artifact allowlist is authorization, not immutable snapshotting. A process that is already trusted to write inside an allowed artifact root can change artifact bytes. Operators should therefore use dedicated artifact directories with restrictive ownership and treat their writers as trusted artifact producers. A future stronger artifact-source abstraction could use immutable content-addressed storage or descriptor/handle-based transfer if the trust model expands.

The application threat model also does not defend against a hostile kernel/host administrator, direct tampering with the SQLite database, or compromise of `remote-exec-mcp`. Those remain OS/infrastructure security responsibilities and should not be “solved” by adding generic shell, credential, or transport logic to deploy-mcp.

## Change rule

Any new v0.1 or post-v0.1 capability that expands caller-controlled inputs, filesystem access, durable mutation, rollback authority, or remote execution must update this threat model and add a regression proving the new boundary fails closed.
