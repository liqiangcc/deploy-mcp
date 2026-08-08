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

- [ ] Initialize Rust crate/binary
- [ ] Add module boundaries: domain / application / ports / adapters / mcp / config / persistence
- [ ] Add CI gates:
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test --all-features`
- [ ] Add stable application error model
- [ ] Add configuration loader and validation

Acceptance criteria:

- architecture modules compile without SSH/SFTP dependencies in domain/application;
- malformed application/environment/task references fail at startup or precheck with stable errors.

## Phase 2 — Deployment domain

- [ ] `ApplicationId` / `EnvironmentId` / `DeploymentId`
- [ ] `Artifact` with version, size, SHA-256
- [ ] `Deployment`
- [ ] `DeploymentState`
- [ ] explicit valid state-transition rules
- [ ] `DeploymentPlan`
- [ ] rollback boundary/model
- [ ] unit tests for every valid and invalid transition

Acceptance criteria:

- impossible transitions are rejected deterministically;
- `SUCCEEDED` can only follow successful verification;
- pre-mutation failure and post-mutation failure have different rollback behavior.

## Phase 3 — Durable deployment repository

- [ ] `DeploymentRepository` port
- [ ] SQLite implementation
- [ ] schema/migrations
- [ ] deployment record persistence
- [ ] step-attempt persistence
- [ ] state-transition history
- [ ] transaction boundaries for state changes
- [ ] recovery tests for interrupted/non-terminal deployments

Acceptance criteria:

- process restart does not erase deployment state;
- persistence failure prevents unrecorded mutation from continuing.

## Phase 4 — Remote execution adapter

- [ ] `RemoteExecutionPort`
- [ ] remote-exec MCP client adapter
- [ ] `check_target`
- [ ] capability/task preflight
- [ ] bounded artifact upload
- [ ] named task execution
- [ ] structured remote error mapping
- [ ] fake/mock remote port for deterministic workflow tests

Acceptance criteria:

- deploy-mcp contains no SSH/SFTP implementation;
- tests can run the full deployment workflow without a real server;
- real adapter only invokes capabilities exposed by remote-exec-mcp.

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
