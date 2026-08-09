# Threat model and regression boundary

## Purpose

`deploy-mcp` is intentionally a deployment orchestrator, not a general remote administration service. Its security model assumes an AI/MCP caller may be mistaken, adversarial, replaying old requests, or attempting to smuggle lower-level execution controls into a deployment request.

The central rule is:

> AI controls deployment intent; deterministic code controls filesystem, persistence, rollback, remote-execution capability boundaries, and what diagnostic detail is disclosed back to AI callers.

This document defines the v0.1 threat boundary and the regression tests that must remain green as the implementation evolves.

## Trust boundaries

```text
AI / MCP caller
  | untrusted deployment intent
  v
MCP adapter
  | typed, deny-unknown-fields request DTOs
  | structured/redacted diagnostic disclosure
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
- remote failures, transport errors, remote stdout/stderr, and timeouts whose final remote state may be unknown.

The v0.1 model assumes:

- the OS account and administrators controlling deploy-mcp configuration and SQLite storage are trusted;
- processes with write access to configured local artifact roots are trusted artifact producers;
- `remote-exec-mcp` independently enforces its target, task, local-transfer, remote-path, authentication, and SSH/SFTP policies;
- trusted SQLite storage may retain operator-useful free-form diagnostics, but that does not authorize the MCP layer to disclose them to AI callers;
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

- `tests/threat_model.rs::ai_facing_mutation_schemas_reject_remote_capability_injection`
- `tests/threat_model.rs::ai_facing_read_schemas_do_not_accept_remote_execution_controls`
- existing MCP adapter tests in `src/mcp.rs`.

### 2. Local filesystem read escape

Threat: a caller uses an absolute path, `..`, an artifact symlink, or a symlinked allowed root to make deploy-mcp read a file outside the intended artifact directory.

Controls:

- `local_artifacts.allowed_roots` is an explicit capability list;
- an omitted/empty list disables new local-artifact deployments;
- configured root strings must be bounded, absolute, non-filesystem-root paths without parent components;
- roots and requested artifacts are canonicalized;
- every canonical root is revalidated as a directory and must still be a non-filesystem-root capability;
- canonical artifact containment is checked before hashing, reservation, or remote work;
- the same canonical path is used for hashing and upload delegation.

The post-canonicalization root check prevents a lexical path such as `/srv/deploy/artifacts` from silently expanding into the entire filesystem when that path is a symbolic link to `/`.

Regression coverage:

- `tests/threat_model.rs::missing_local_artifact_capability_denies_before_remote_or_durable_work`
- `tests/threat_model.rs::artifact_outside_allowlist_is_rejected_before_remote_or_durable_work`
- `tests/threat_model.rs::symlink_inside_allowlist_cannot_escape_before_remote_or_durable_work`
- `tests/threat_model.rs::symlinked_allowed_root_cannot_expand_capability_to_filesystem_root`
- `tests/threat_model.rs::unsafe_artifact_capability_roots_are_rejected_at_configuration_boundary`
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
- rollback input rejection in `tests/threat_model.rs::ai_facing_mutation_schemas_reject_remote_capability_injection` and `src/mcp.rs`.

### 5. Unconfigured target or capability substitution

Threat: a caller attempts to reinterpret an application/environment value as a transport address, or to promote caller-controlled values into remote target, task, path, or task-parameter authority.

Controls:

- application and environment are configuration references rather than transport endpoints;
- unknown application/environment values are rejected before remote work;
- target, task names, and remote deployment paths are selected from the configured environment;
- the local artifact path is authorized and canonicalized, but is not promoted into named-task parameters;
- version, target, task, shell, command, and credential-like caller values never become remote task parameters.

Regression coverage:

- `tests/threat_model.rs::unknown_environment_cannot_be_used_to_select_an_unconfigured_target`
- `tests/threat_model.rs::deployment_remote_calls_are_derived_only_from_configured_capabilities`.

### 6. Ambiguous timeout or crash state

Threat: a remote action may have completed or may still be completing even though deploy-mcp timed out or crashed, and immediately retrying or rolling back could duplicate or compound mutation.

Controls:

- the remote-side-effect ambiguity boundary starts at `STAGING_ARTIFACT`, not at `INSTALLING`;
- staging upload and backup timeout preserve a non-terminal durable guard because fixed staging/backup resources may still be changing remotely;
- install/restart/verify/rollback timeout likewise preserves a non-terminal/unknown guard rather than claiming a terminal result;
- automatic rollback remains a separate policy boundary for **completed** failures after live artifact replacement starts;
- started explicit rollback remains durably guarded when completion is unknown;
- startup recovery never guesses remote state;
- interrupted staging/backup/install/restart/verify/rollback states create manual-reconciliation incidents;
- unresolved recovery incidents block later deployment/rollback mutation until explicit operator reconciliation.

Regression coverage:

- `tests/timeouts.rs::staging_timeout_keeps_durable_guard_until_recovery`
- `tests/timeouts.rs::backup_timeout_keeps_durable_guard_until_recovery`
- remaining timeout cases in `tests/timeouts.rs`
- `tests/startup_recovery.rs`
- `tests/recovery_acknowledgement.rs`.

### 7. Information disclosure through deployment/history tools

Threat: deployment results or read-only history become an indirect way to obtain credentials, shell controls, remote task stdout/stderr, secret-bearing error text, or executable rollback snapshot details.

Controls:

- audit history is projected from durable facts through `AuditRepository`;
- trusted persistence may retain free-form diagnostics for operator forensics, but those strings are not the AI protocol contract;
- deployment and rollback outcome serialization exposes stable structured fields (`step`, `code`, optional `remote_code`) plus generic public messages rather than raw remote output;
- `get_deployment`/`list_deployments` retain step lifecycle but replace persisted free-form error bodies with `details_redacted`;
- `get_deployment_history` recursively removes diagnostic/control keys including `error`, `detail`, `stdout`, `stderr`, `evidence`, `shell`, `command`, `argv`, and `credentials` before MCP serialization;
- rollback target/path/task snapshot details and operator reconciliation evidence are excluded from the normal history projection;
- history arguments reject remote execution parameters.

Regression coverage:

- `src/mcp.rs::ai_facing_serialization_redacts_remote_diagnostic_output`
- `tests/structured_audit_history.rs`
- history/list/get parameter rejection in `tests/threat_model.rs::ai_facing_read_schemas_do_not_accept_remote_execution_controls` and `src/mcp.rs`.

### 8. Security-boundary collapse with remote-exec-mcp

Threat: deploy-mcp starts implementing SSH/SFTP or bypasses remote-exec-mcp policy by exposing generic protocol calls.

Controls:

- domain/application depend on `RemoteExecutionPort`, not SSH/SFTP libraries;
- the real adapter invokes only the remote-exec MCP capabilities needed by deployment;
- local artifact authorization is independent of remote-exec transfer authorization;
- no raw `call_tool(name, arbitrary_json)` surface is exposed to deployment orchestration.

Regression coverage:

- adapter contract/unit tests in `src/adapters.rs`;
- `tests/threat_model.rs::deployment_remote_calls_are_derived_only_from_configured_capabilities`;
- Phase 4 acceptance tests and dependency review in CI.

## Fail-closed ordering requirements

Security-sensitive rejection should happen at the earliest owning boundary:

```text
invalid MCP field
  -> reject during DTO decoding

invalid/disabled local artifact capability
  -> canonicalize configured root
  -> revalidate canonical root is bounded
  -> reject before artifact hashing
  -> no deployment reservation
  -> no RemoteExecutionPort call

active mutation/recovery guard
  -> reject before new remote work

unknown application/environment
  -> reject before target selection or remote work

missing remote target/task capability
  -> fail during PRECHECKING
  -> no side-effecting deployment step

unknown side-effecting timeout/crash result
  -> retain durable guard from STAGING_ARTIFACT onward
  -> require recovery/reconciliation rather than guessing

free-form remote diagnostics
  -> may remain in trusted operator storage
  -> never cross the MCP disclosure boundary
```

Tests should assert not only the error code but also the absence of remote calls and durable mutation where that is part of the boundary contract.

## Residual risks and explicit non-goals

The local artifact allowlist is authorization, not immutable snapshotting. A process that is already trusted to write inside an allowed artifact root can change artifact bytes. Operators should therefore use dedicated artifact directories with restrictive ownership and treat their writers as trusted artifact producers. A future stronger artifact-source abstraction could use immutable content-addressed storage or descriptor/handle-based transfer if the trust model expands.

The application threat model also does not defend against a hostile kernel/host administrator, direct tampering with the SQLite database, or compromise of `remote-exec-mcp`. Those remain OS/infrastructure security responsibilities and should not be “solved” by adding generic shell, credential, or transport logic to deploy-mcp.

## Regression suite rule

`tests/threat_model.rs` is the focused cross-layer suite for the caller-to-capability boundary. It intentionally complements rather than duplicates transition, persistence, idempotency, rollback, timeout, recovery, reconciliation, retention, and remote-exec contract suites.

Any new v0.1 or post-v0.1 capability that expands caller-controlled inputs, filesystem access, durable mutation, rollback authority, diagnostic disclosure, or remote execution must update this threat model and add a regression proving the new boundary fails closed.
