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

- [ ] request validation
- [ ] artifact checksum calculation
- [ ] deployment-level lock per `(application, environment)`
- [ ] PRECHECKING
- [ ] STAGING_ARTIFACT
- [ ] BACKING_UP
- [ ] INSTALLING
- [ ] RESTARTING
- [ ] VERIFYING
- [ ] SUCCEEDED
- [ ] automatic rollback after post-mutation failure
- [ ] ROLLED_BACK / ROLLBACK_FAILED
- [ ] preserve original failure separately from rollback failure

Acceptance criteria:

```text
deploy request
  -> precheck
  -> stage
  -> backup
  -> install
  -> restart
  -> verify
  -> success
```

and on post-mutation failure:

```text
failure
  -> rollback
  -> restart/verify previous release
  -> rolled_back | rollback_failed
```

## Phase 6 — MCP adapter

- [ ] stdio MCP server
- [ ] `list_applications`
- [ ] `deploy_application`
- [ ] `get_deployment`
- [ ] `list_deployments`
- [ ] `rollback_deployment`
- [ ] structured machine-readable errors
- [ ] no raw shell / arbitrary path / credential parameters

Acceptance criteria:

- MCP remains a thin protocol adapter;
- deployment semantics live in application/domain services;
- tools return deployment records/results rather than raw SSH command output.

## Phase 7 — Production hardening for v0.1

- [ ] idempotency key support
- [ ] reject same version with changed checksum
- [ ] startup recovery policy for non-terminal deployments
- [ ] deployment-level structured audit/history
- [ ] timeouts at deployment-step level
- [ ] deterministic verification retry policy
- [ ] explicit rollback-unavailable behavior
- [ ] local artifact-path allowlist
- [ ] threat-model regression tests
- [ ] disposable integration test using a real remote-exec-mcp process or equivalent protocol fixture
- [ ] README/config/client setup documentation

## v0.1 completion boundary

v0.1 is complete when a Java JAR can be deployed to one configured Linux/systemd environment through `remote-exec-mcp`, with durable state, deterministic verification, automatic rollback, deployment history, and no unrestricted remote execution surface in deploy-mcp.

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

> New deployment mechanisms should implement ports/adapters around the same deployment lifecycle, not add transport-specific branches throughout the domain model.
