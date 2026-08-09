# deploy-mcp Roadmap

## Phase 0 — Architecture baseline

- [x] Define deploy-mcp vs remote-exec-mcp boundary
- [x] Define JAR/systemd v0.1 scope
- [x] Define deployment domain model
- [x] Define deployment state machine
- [x] Define `RemoteExecutionPort`
- [x] Define initial MCP tool surface
- [x] Define rollback and verification semantics
- [x] Define persistence/idempotency/concurrency requirements

## Phase 1 — Rust project bootstrap

- [x] Initialize Rust crate/binary
- [x] Add module boundaries: domain / application / ports / adapters / mcp / config / persistence
- [x] Add CI gates:
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test --all-features`
- [x] Add stable application error model
- [x] Add configuration loader and validation

Acceptance criteria:

- architecture modules compile without SSH/SFTP dependencies in domain/application;
- malformed application/environment/task references fail at startup or precheck with stable errors.

## Phase 2 — Deployment domain

- [x] `ApplicationId` / `EnvironmentId` / `DeploymentId`
- [x] `Artifact` with version, size, SHA-256
- [x] `Deployment`
- [x] `DeploymentState`
- [x] explicit valid state-transition rules
- [x] `DeploymentPlan`
- [x] rollback boundary/model
- [x] unit tests for every valid and invalid transition

Acceptance criteria:

- impossible transitions are rejected deterministically;
- `SUCCEEDED` can only follow successful verification;
- pre-mutation failure and post-mutation failure have different rollback behavior;
- `INSTALL` is the first live-artifact mutation boundary;
- `PRECHECK`, staging, and backup failures do not trigger automatic rollback;
- install/restart/verify failures enter rollback only when a rollback point is available.

## Phase 3 — Durable deployment repository

- [x] `DeploymentRepository` port
- [x] SQLite implementation
- [x] schema/migrations
- [x] deployment record persistence
- [x] step-attempt persistence
- [x] state-transition history
- [x] transaction boundaries for state changes
- [x] recovery tests for interrupted/non-terminal deployments

Acceptance criteria:

- process restart does not erase deployment state;
- non-terminal deployments can be rehydrated after reopening the SQLite database;
- durable state and transition history are committed atomically;
- optimistic expected-state checks reject stale writers without leaving partial history;
- step attempts are durable and a completed attempt cannot be completed twice;
- persistence failure/conflict stops state advancement before later deployment work can continue.

## Phase 4 — Remote execution adapter

- [x] `RemoteExecutionPort`
- [x] remote-exec MCP client adapter
- [x] `check_target`
- [x] capability/task preflight
- [x] bounded artifact upload
- [x] named task execution
- [x] structured remote error mapping
- [x] fake/mock remote port for deterministic workflow tests

Acceptance criteria:

- deploy-mcp contains no SSH/SFTP implementation;
- `RemoteExecMcpAdapter` communicates with a configured remote-exec-mcp process over MCP stdio only;
- the adapter invokes only `check_target`, `list_tasks`, `upload_file`, and `run_task`, with no raw-shell capability;
- application preflight rejects unreachable targets and configured tasks that are not currently exposed/authorized;
- artifact upload delegates remote/local path allowlists, transfer-size bounds, timeout, and overwrite authorization to remote-exec-mcp rather than duplicating its security policy;
- structured remote `{code, message}` errors remain distinguishable from transport/protocol and malformed-response failures;
- the fake remote port records typed calls and allows deterministic Phase 5 workflow tests without SSH or a real remote-exec process;
- contract tests lock the remote-exec-mcp v0.1 structured response shapes consumed by the adapter.

## Phase 5 — JAR/systemd deployment workflow

- [x] request validation
- [x] artifact checksum calculation
- [x] deployment-level lock per `(application, environment)`
- [x] PRECHECKING
- [x] STAGING_ARTIFACT
- [x] BACKING_UP
- [x] INSTALLING
- [x] RESTARTING
- [x] VERIFYING
- [x] SUCCEEDED
- [x] automatic rollback after post-mutation failure
- [x] ROLLED_BACK / ROLLBACK_FAILED
- [x] preserve original failure separately from rollback failure

Acceptance criteria:

```text
deploy request
  -> validate configured application/environment/version
  -> stream artifact SHA-256 + size
  -> acquire deployment lock
  -> persist CREATED
  -> precheck target/capabilities
  -> stage artifact
  -> backup current artifact
  -> install staged artifact
  -> restart
  -> verify
  -> SUCCEEDED
```

and on post-mutation failure:

```text
install/restart/verify failure
  -> ROLLING_BACK
  -> restore configured backup
  -> restart
  -> verify previous release
  -> ROLLED_BACK | ROLLBACK_FAILED
```

Additional invariants:

- backup failure is pre-mutation and ends as `FAILED` without rollback;
- remote task failures are recorded as durable step attempts before the workflow advances;
- persistence failure prevents later remote work from continuing;
- in-process leases reject concurrent work for the same application/environment;
- SQLite also has a partial unique index for non-terminal deployments, so separate repository/process instances sharing the database cannot create two active deployments for the same application/environment;
- deployment paths are passed only as structured task parameters; deploy-mcp never turns them into shell strings;
- original deployment failure and rollback failure remain separate in the application result.

## Phase 6 — MCP adapter

- [x] stdio MCP server
- [x] `list_applications`
- [x] `deploy_application`
- [x] `get_deployment`
- [x] `list_deployments`
- [x] `rollback_deployment`
- [x] durable deployment-bound `RollbackReference`
- [x] independent durable `RollbackOperation`
- [x] structured machine-readable errors
- [x] no raw shell / remote deployment path / credential parameters

Acceptance criteria:

- MCP remains a thin protocol adapter over the application-owned `DeploymentApi` inbound port;
- deployment semantics remain in application/domain services rather than tool handlers;
- tools return deployment/rollback records and structured outcomes rather than raw SSH command output;
- `deploy_application` accepts only application/environment/version/local artifact path and rejects undeclared fields such as shell commands or SSH credentials;
- `rollback_deployment` accepts only `deployment_id`; callers cannot supply backup/install paths, task names, credentials, or shell fragments;
- a successful deployment records a rollback reference bound to that deployment when the configured rollback capability is available; reference-persistence failure is reported separately and does not rewrite an already successful deployment as failed;
- explicit rollback is a separate `RollbackOperation`; it never reopens the terminal source `Deployment` aggregate;
- the rollback reference snapshots target, backup/install paths, and rollback/restart/health task names and is used only when the current environment contract still matches that snapshot;
- a newer deployment record for the same application/environment conservatively invalidates the older reference before remote mutation;
- successful explicit rollback atomically marks the rollback operation `SUCCEEDED` and consumes the reference; failed explicit rollback records `FAILED` while preserving the active reference for a retry;
- process-local leases serialize deploy/rollback calls in one process, while SQLite constraints/triggers prevent a started explicit rollback and a non-terminal deployment from mutating the same application/environment across separate process/repository instances;
- `get_deployment` and `list_deployments` query durable state through `DeploymentRepository`, not SQLite from the MCP adapter;
- `list_deployments` is bounded to 1..=200 records and requires an application when filtering by environment;
- stdout is reserved for MCP stdio frames; startup diagnostics/logging go to stderr;
- the server composes the existing remote-exec MCP client rather than adding SSH/SFTP code;
- dedicated explicit-rollback tests prove success/consume, failure/retry, newer-deployment invalidation, cross-instance mutation exclusion, and configuration-drift rejection before remote work.

## Phase 7 — Production hardening for v0.1

- [x] idempotency key support
- [x] reject same version with changed checksum
- [x] startup recovery policy for non-terminal deployments and `STARTED` rollback operations
- [x] operator reconciliation/acknowledgement for unresolved recovery incidents
- [x] deployment-level structured audit/history
- [x] timeouts at deployment-step and explicit-rollback-operation level
- [x] deterministic verification retry policy
- [ ] rollback-reference retention/cleanup policy
- [ ] local artifact-path allowlist
- [ ] threat-model regression tests
- [ ] disposable integration test using a real remote-exec-mcp process or equivalent protocol fixture
- [ ] README/config/client setup documentation

Deployment identity acceptance criteria:

- `deploy_application` accepts an optional idempotency key of 1..=128 bytes with no leading/trailing whitespace or control characters;
- one SQLite-global idempotency key is durably bound to one deployment intent identified by application, environment, version, artifact SHA-256, and artifact size;
- an exact retry after the original active mutation has finished returns the original durable Deployment with `idempotent_replay = true` and performs no additional remote work;
- a concurrent retry while an application/environment mutation is still active remains governed by the existing mutation lease and may return `conflicting_deployment` rather than joining the running orchestration;
- reusing one idempotency key with a changed durable request identity fails with stable `idempotency_conflict` before remote work;
- for one application/environment, a version string is immutable with respect to artifact SHA-256 and size; changed bytes fail with stable `artifact_version_conflict` even when a new idempotency key is supplied;
- a new deployment of the same version and exactly the same artifact remains an explicit redeployment when a new key (or no key) is used and no mutation guard is active;
- SQLite `BEGIN IMMEDIATE` reservation transactions serialize idempotency lookup, version-identity validation, deployment insertion, and key binding across repository/process instances;
- idempotency/version identity does not weaken active-deployment, explicit-rollback, or unresolved-recovery mutation guards;
- MCP continues to deny undeclared raw execution fields; `idempotency_key` cannot control remote paths, commands, tasks, services, or credentials.

Structured audit/history acceptance criteria:

- `AuditRepository` is a read-only observation port; it cannot change deployment, rollback, or recovery state and has no `RemoteExecutionPort` dependency;
- v0.1 creates no duplicate audit write/event store: history is projected from authoritative durable deployment, step-attempt, rollback, recovery-incident, and reconciliation records already owned by their existing repositories;
- `get_deployment_history` accepts only `deployment_id` plus a limit bounded to `1..=500`; the MCP adapter does not query SQLite directly;
- the timeline exposes stable structured events for deployment creation/transitions/steps, rollback-reference and explicit-rollback lifecycle, recovery incidents, and operator acknowledgement;
- recovery incidents whose subject is an explicit rollback operation are correlated back to that operation's `source_deployment_id`;
- normal AI-facing history does not expose rollback target/path/task snapshot details, credentials, shell controls, or operator reconciliation evidence;
- the projection survives SQLite connection/process reopen because it reconstructs from committed durable source facts rather than process-local logs;
- same-millisecond events are deterministically ordered for rendering, but the tie-break order is not treated as additional causal/state-machine semantics;
- a dedicated cross-repository test proves deployment -> rollback STARTED -> crash recovery -> manual incident -> operator acknowledgement -> database reopen as one recoverable structured timeline.

Verification retry acceptance criteria:

- only the configured `health_check` verification task is retried; precheck, upload, backup, install, restart, and restore tasks remain single-attempt to avoid duplicating side effects;
- `verification_max_attempts` includes the first attempt and is bounded to `1..=10`; `verification_retry_delay_ms` is a fixed `0..=60000` millisecond delay with no jitter, randomness, or exponential backoff;
- every deployment or automatic-rollback verification attempt is recorded as its own durable `DeploymentStep::Verify` step attempt, preserving failed attempts before a later success;
- a successful verification attempt stops immediately; a completed failed health check may retry until exhaustion, but `operation_timed_out` stops immediately because remote completion is unknown;
- automatic rollback verification uses the same deterministic policy, while explicit rollback retries its final verification inside the existing whole-operation timeout;
- if the explicit rollback deadline expires during a retry or retry delay, the durable rollback operation remains `STARTED` and the existing fail-closed recovery/reconciliation guard continues to apply;
- retry configuration remains deploy-mcp orchestration policy and does not alter `RemoteExecutionPort`, SSH/SFTP behavior, raw command exposure, or remote-exec-mcp task semantics.

Timeout acceptance criteria:

- deployment-step deadlines are configured in deploy-mcp and wrap capability preflight, staging upload, named deployment tasks, and automatic rollback tasks without adding SSH/SFTP behavior to the deployment domain;
- a timed-out deployment step closes its durable step attempt as `FAILED` with stable `operation_timed_out`;
- pre-mutation timeout before the live `INSTALL` boundary can terminate as `FAILED`, while `INSTALL`/`RESTART`/`VERIFY` timeout retains the current non-terminal deployment state and does not immediately start automatic rollback because remote completion is unknown;
- automatic rollback timeout retains `ROLLING_BACK` and stops subsequent rollback tasks instead of claiming `ROLLBACK_FAILED` while the timed-out remote action may still finish;
- explicit rollback has a whole-operation deadline after the durable operation enters `STARTED`; a timeout leaves that operation `STARTED` because remote mutation state is unknown;
- the existing cross-process mutation guards therefore block further deploy/rollback mutation until startup recovery/manual reconciliation resolves uncertain timed-out mutation state;
- timeout configuration is bounded and has safe defaults; timeout handling never exposes raw commands, credentials, or remote paths through new MCP inputs.

Startup recovery acceptance criteria:

- recovery is a separate `StartupRecoveryService` / `RecoveryRepository` concern rather than a transport-specific branch in `Deployment` or `RollbackOperation`;
- startup recovery runs before `remote-exec-mcp` is spawned and never performs remote work or infers remote state;
- an interrupted deployment before `INSTALLING` is durably terminated as `FAILED`, any `STARTED` step attempt is closed as failed, and an `auto_resolved` recovery incident permits later mutation;
- an interrupted deployment at or after `INSTALLING` is durably terminated as `FAILED` but creates an unresolved `manual_reconciliation_required` incident because live remote state may be unknown;
- an interrupted explicit rollback is durably terminated as `FAILED`, preserves its active rollback reference, and creates an unresolved recovery incident;
- unresolved incidents survive restart and SQLite connection boundaries and block both new deployments and explicit rollbacks for the same application/environment;
- startup recovery is idempotent and does not create duplicate incidents/transitions on a later restart.

Operator reconciliation acceptance criteria:

- reconciliation is a separate `RecoveryAdminService` / `RecoveryRepository` administrative concern with no `RemoteExecutionPort` dependency;
- the normal AI-facing MCP server exposes no tool that clears recovery incidents;
- the separate `deploy-mcp-recovery` CLI can list unresolved incidents and record acknowledgements against the same SQLite database;
- acknowledgement requires incident id, application, environment, subject kind, subject id, a non-empty operator value, and non-empty evidence;
- the repository revalidates that the referenced incident is still unresolved, is `manual_reconciliation_required`, and exactly matches every supplied identity field;
- acknowledgement audit insertion and setting `resolved_at_unix_ms` are committed atomically, so a partial administrative operation cannot release the mutation guard;
- an identity mismatch, auto-resolved incident, already-resolved incident, or acknowledgement replay cannot release the guard;
- a successful acknowledgement survives database reopen and permits later deployment/rollback mutation through the ordinary safety checks;
- `operator` and `evidence` are durable audit metadata, not authentication/authorization; OS/database access controls remain responsible for limiting use of the administrative binary;
- operator acknowledgement records a human conclusion but never performs remote inspection, remote commands, automated state inference, or automatic post-crash rollback.

## v0.1 completion boundary

v0.1 is complete when a Java JAR can be deployed to one configured Linux/systemd environment through `remote-exec-mcp`, with durable state, deterministic verification, automatic rollback, safe deployment-bound explicit rollback, fail-closed crash recovery, controlled operator reconciliation, deployment history, and no unrestricted remote execution surface in deploy-mcp.

## Post-v0.1 candidates

Do not start these until the generic deployment lifecycle has proven stable:

- Docker/Compose deployment adapter;
- Kubernetes native API adapter;
- Helm workflow;
- multi-host rolling deployment;
- blue/green and canary strategies;
- database migration gates;
- approval/change-management gates;
- artifact registry integration;
- log-query-mcp assisted post-deploy diagnosis;
- notification/event integrations.

The rule for future expansion is:

> New deployment mechanisms should implement ports/adapters around the same deployment lifecycle, not add transport-specific branches throughout the deployment domain model.
