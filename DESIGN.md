# deploy-mcp Design

## 1. Purpose

`deploy-mcp` is a deployment-orchestration MCP. Its responsibility is to make a deployment a deterministic, auditable state transition rather than a sequence of ad-hoc remote commands chosen by an AI agent.

The core design rule is:

> `deploy-mcp` owns deployment semantics; `remote-exec-mcp` owns safe remote execution.

A deployment may require file transfer and remote commands, but those are implementation capabilities, not the deployment domain itself.

## 2. Separation of concerns

### deploy-mcp owns

- application/environment deployment configuration;
- artifact identity and integrity metadata;
- deployment plan generation;
- deployment state transitions;
- prechecks;
- backup policy;
- install/restart/verification orchestration;
- rollback decisions and rollback orchestration;
- deployment history and status;
- idempotency/concurrency rules at the deployment level;
- MCP tools that expose deployment intent.

### deploy-mcp does not own

- SSH connection/session management;
- SSH keys/passwords;
- known_hosts policy;
- SFTP protocol implementation;
- generic shell access;
- arbitrary remote command execution;
- remote command quoting;
- low-level execution timeout/output bounding;
- generic remote audit infrastructure;
- log-query semantics;
- source build/CI semantics.

Those remain outside the deployment domain. The first remote capability adapter is `remote-exec-mcp`.

## 3. Architecture

```text
AI Agent
   |
   v
MCP Adapter
   |
   v
Deployment Application Service
   |
   +--> Application Catalog
   +--> Deployment Policy
   +--> Deployment Repository
   +--> Deployment Planner
   +--> Deployment State Machine
   +--> RemoteExecutionPort
   |
   v
RemoteExecMcpAdapter
   |
   v
remote-exec-mcp
   |
   +--> check_target
   +--> list_tasks
   +--> upload_file
   +--> run_task
   v
Remote Linux host
```

Dependency direction:

```text
Domain <- Application <- Adapters
```

The domain/application layers know only application-owned ports. They do not know SSH, SFTP, `rmcp`, or remote-exec transport internals.

## 4. v0.1 scope

The first release supports one deliberately narrow workflow:

```text
Java/Spring Boot JAR
        +
Remote Linux host
        +
systemd-managed service
```

The v0.1 deployment contract includes:

1. validate application/environment/version request;
2. read the local artifact and compute stable size + SHA-256 before remote mutation;
3. verify target connectivity and required configured capabilities;
4. stage the artifact to a bounded remote staging path;
5. create a configured backup rollback point before live mutation;
6. install the staged artifact;
7. restart the configured service;
8. verify health deterministically;
9. mark success only after verification;
10. rollback automatically after a post-mutation failure when rollback is available;
11. persist deployment state transitions and step attempts.

Building source, discovering arbitrary service commands, or interpreting logs to decide deployment success are not part of v0.1.

## 5. Domain model

### Application

Represents a deployable logical service.

```text
Application
- id
- display_name
- artifact_type = jar
- environments
```

### Environment

Binds an application to deployment-specific configuration.

```text
Environment
- name                 # test / staging / prod
- target               # remote-exec target id
- staging_path
- install_path
- backup_path
- backup_task
- install_task
- restart_task
- health_check_task
- optional precheck_task
- optional rollback_task
```

`target` and task names are references to externally configured capabilities. The deployment domain never translates them into raw SSH commands.

### Artifact

The durable domain artifact contains identity/integrity metadata:

```text
Artifact
- version
- sha256
- size_bytes
```

`artifact_path` is request/application input used to read and upload the local file; it is intentionally not part of the durable Artifact value object. `version` identifies the logical release and `sha256` identifies the bytes being deployed.

### Deployment

The current domain aggregate is intentionally small:

```text
Deployment
- id
- application
- environment
- artifact
- state
```

Operational history is not duplicated into mutable aggregate fields. Timestamps, state-transition history, step-attempt status, and step failures are durable repository records. The application result (`DeploymentOutcome`) separately carries the primary deployment failure and rollback failure for the current request.

### DeploymentPlan

A plan is generated before mutating the target.

```text
DeploymentPlan
- deployment_id
- target
- artifact
- ordered steps
- rollback boundary
- rollback availability
```

The plan contains deployment operations, not shell strings.

## 6. State machine

Primary path:

```text
CREATED
   |
   v
PRECHECKING
   |
   v
STAGING_ARTIFACT
   |
   v
BACKING_UP
   |
   v
INSTALLING
   |
   v
RESTARTING
   |
   v
VERIFYING
   |
   v
SUCCEEDED
```

Failure behavior:

```text
CREATED / PRECHECKING / STAGING_ARTIFACT / BACKING_UP
        |
        +---- failure ----> FAILED

INSTALLING / RESTARTING / VERIFYING
        |
        +---- failure ----> ROLLING_BACK   (only when rollback is available)
                              |
                         +----+----+
                         |         |
                         v         v
                   ROLLED_BACK  ROLLBACK_FAILED
```

`INSTALLING` is the first live-artifact mutation boundary. Staging and backup prepare deployment/rollback material but must not replace the live artifact.

Important invariants:

> A deployment becomes `SUCCEEDED` only after the configured verification task passes.

> Failures before the first live-artifact mutation do not trigger rollback. Failures after the mutation boundary enter an explicit rollback outcome only when a rollback point is available; otherwise they end as `FAILED`.

`SUCCEEDED` remains terminal for the original deployment. A future explicit user-requested rollback must be modeled as a separate application use case/operation rather than reopening the completed deployment state machine.

## 7. Deployment step semantics

### PRECHECK

Checks deterministic prerequisites before mutation:

- target is reachable;
- required remote-exec task names exist/are allowed;
- artifact is a readable, non-empty regular file and its checksum/size are computed before remote work;
- no conflicting deployment is active for the same application/environment.

Capability preflight currently validates task presence by name. The actual typed task-parameter schema remains owned and enforced by `remote-exec-mcp`; a schema mismatch therefore fails deterministically when `run_task` is invoked rather than being guessed by deploy-mcp.

### STAGE_ARTIFACT

Uses `RemoteExecutionPort.upload_file` to copy the artifact to the configured staging location. Staging must not replace the live artifact. `deploy-mcp` supplies the configured destination; `remote-exec-mcp` independently enforces local/remote allowlists, transfer-size bounds, timeout, and overwrite behavior.

The returned byte count must equal the size hashed before deployment. A mismatch is treated as `artifact_changed` and the workflow does not proceed to backup/install.

### BACKUP_CURRENT

Creates a rollback point through the configured approved backup task. Backup itself remains before the live-artifact mutation boundary.

### INSTALL

Moves or installs the staged artifact into the configured live location through the approved install task. This is the first live-artifact mutation. `deploy-mcp` decides *when* install occurs; `remote-exec-mcp` decides whether the underlying task is authorized and safely executable.

### RESTART

Invokes the configured approved restart task.

### VERIFY

Invokes the configured deterministic health-check task. v0.1 does not accept AI interpretation of arbitrary logs as the success criterion.

### ROLLBACK

Automatic rollback after an install/restart/verify failure performs:

```text
restore backup task
  -> restart task
  -> health-check task
  -> ROLLED_BACK | ROLLBACK_FAILED
```

Rollback has its own terminal result and never overwrites the primary deployment failure in `DeploymentOutcome`. The durable step-attempt history also preserves which operation failed.

## 8. RemoteExecutionPort

The application layer depends on a narrow port:

```text
RemoteExecutionPort
- check_target(target)
- list_tasks(target)
- upload_file(target, local_path, remote_path, overwrite)
- run_task(target, task, parameters)
```

The first adapter calls `remote-exec-mcp` over MCP stdio using a configured child process. No SSH library belongs in `deploy-mcp` v0.1.

The adapter owns only MCP request/response conversion and structured remote error decoding. Deployment capability rules remain in the application layer. Preflight calls `check_target` and `list_tasks`, then verifies that every task referenced by the selected environment is currently exposed/authorized.

Remote-exec failures preserve their structured `{code, message}` cause and remain distinguishable from MCP transport/protocol failures or malformed responses.

A deterministic fake `RemoteExecutionPort` is used by application/workflow tests, so deployment logic does not require a live SSH server or remote-exec process.

### JAR/systemd task parameter contract

Deployment paths cross the port only as typed JSON task parameters, never as shell fragments:

```text
backup task
  install_path
  backup_path

install task
  staging_path
  install_path

rollback task
  backup_path
  install_path

precheck / restart / health-check
  no deploy-mcp-defined path parameters
```

The matching `remote-exec-mcp` task definitions must declare and validate these parameters. The names above are a deterministic adapter contract between the two independently deployable MCPs; they do not grant arbitrary filesystem access.

## 9. MCP surface

The initial public tools should be small and deployment-oriented:

### list_applications

Returns configured application/environment summaries without remote access.

### deploy_application

Input:

```json
{
  "application": "demo-service",
  "environment": "test",
  "version": "1.2.3",
  "artifact_path": "/staging/demo-service-1.2.3.jar"
}
```

Returns a deployment record/result, not raw command output.

### get_deployment

Returns current/terminal deployment state and step outcomes.

### list_deployments

Returns recent deployment records for an application/environment.

### rollback_deployment

Planned explicit rollback entry point. It must be implemented as its own application use case and must not reopen a `SUCCEEDED` Deployment aggregate.

No MCP tool accepts a raw command, SSH credential, arbitrary service name, arbitrary install path, or shell script body.

## 10. Idempotency and concurrency

Deployment concurrency is different from remote command concurrency.

v0.1 invariant:

> At most one non-terminal deployment may exist for a given `(application, environment)` at a time.

Phase 5 enforces this at two independent levels:

1. `DeploymentLockManager` provides a process-local lease to reject concurrent calls before work starts;
2. SQLite has a partial unique index on `(application_id, environment_id)` for non-terminal states, so separate repository/process instances sharing the same database cannot create two active deployments.

The durable constraint is the final authority. A process-local lock alone is never considered sufficient for correctness.

Idempotency-key semantics and same-version/different-checksum retry policy are Phase 7 hardening items and must not be inferred from the Phase 5 deployment lock.

## 11. Persistence

Deployment state must survive the MCP process lifetime. In-memory state is insufficient because losing the process during `INSTALLING` or `ROLLING_BACK` must not erase what happened.

For v0.1, SQLite implements the `DeploymentRepository` port and stores:

- deployment identity, artifact version/checksum/size, and current state;
- deployment step attempts and their error text;
- state-transition history;
- timestamps for durable records.

State and transition history are updated atomically with optimistic expected-state checks. A persistence error/conflict prevents later workflow operations from running.

Primary and rollback failures are separate in the in-flight `DeploymentOutcome`. Durable diagnosis uses the terminal deployment state plus ordered step-attempt records, so a rollback failure does not erase the original failed deployment step.

Do not persist SSH secrets or arbitrary credentials. Remote-exec owns its own security/audit boundary.

## 12. Error model

Errors are stable and machine-readable. Current categories include:

```text
invalid_configuration
unknown_application
unknown_environment
invalid_version
invalid_artifact
artifact_not_found
artifact_changed
conflicting_deployment
precheck_failed
remote_capability_missing
remote_execution_failed
verification_failed
rollback_unavailable
rollback_failed
invalid_state_transition
persistence_failed
```

Remote-exec errors preserve their structured remote code where useful, while the deployment-level error describes the failed deployment operation.

Example:

```text
verification_failed
  remote_code: execution_timeout
```

## 13. Audit boundary

Two audit layers are expected and must not be confused:

```text
remote-exec audit
= what remote capability was invoked and how it ended

deploy history
= why that capability was invoked as part of deployment X and how deployment state/steps changed
```

This separation is intentional because the two histories answer different questions.

## 14. Configuration principle

Configuration is declarative. It maps deployment concepts to approved remote capabilities and points at the remote-exec MCP process to compose.

Example shape:

```yaml
remote_exec:
  command: remote-exec-mcp
  args:
    - /etc/remote-exec/config.yaml

applications:
  demo-service:
    artifact_type: jar
    environments:
      test:
        target: test-server
        staging_path: /opt/staging/demo-service.jar
        install_path: /opt/apps/demo-service/demo-service.jar
        backup_path: /opt/apps/demo-service/backup/demo-service.jar
        tasks:
          precheck: demo-precheck
          backup: demo-backup
          install: demo-install
          restart: demo-restart
          health_check: demo-health
          rollback: demo-rollback
```

The remote-exec executable/arguments are startup configuration, never MCP tool inputs. Paths are deployment configuration and are passed only through the structured task contract above. Actual task authorization, parameter validation, and filesystem/execution safety remain independently enforced by `remote-exec-mcp`.

## 15. Explicit non-goals for v0.1

- building source code;
- Git checkout/pull;
- CI/CD pipeline orchestration;
- unrestricted remote terminal;
- generic file manager;
- SSH/SFTP implementation;
- Docker/Compose deployment;
- Kubernetes/Helm deployment;
- database migration orchestration;
- log search/diagnostics;
- traffic shifting/canary/blue-green rollout;
- multi-host distributed rollout;
- configuration-management replacement.

These can be added as separate capability adapters/workflows only after the JAR/systemd deployment lifecycle is proven.

## 16. Design acceptance criteria

The architecture is considered preserved when all of the following remain true:

1. no domain/application module imports SSH/SFTP implementation types;
2. no MCP tool accepts raw shell;
3. deployment paths cross the remote port only as structured task parameters;
4. all target mutations are represented by deployment state transitions;
5. success requires deterministic verification;
6. rollback is explicit and separately observable from the primary failure;
7. deployment state and step history are durable;
8. active deployment exclusion has a durable database-level constraint, not only an in-memory lock;
9. remote execution authorization and typed task validation remain enforced by `remote-exec-mcp`;
10. adding Docker/Kubernetes later does not require transport-specific branches throughout the deployment domain.
