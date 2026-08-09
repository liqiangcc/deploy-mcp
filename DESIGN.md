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
- automatic and explicit rollback decisions/orchestration;
- deployment-bound rollback-reference validity;
- deployment/rollback history and status;
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
Deployment Application
   |
   +--> DeployService
   +--> RollbackService
   +--> Deployment Repository
   +--> Rollback Repository
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
11. persist deployment state transitions and step attempts;
12. record a deployment-bound rollback reference when a terminal deployment still owns a valid rollback point;
13. expose explicit rollback only through that durable reference and an independent rollback operation.

Building source, discovering arbitrary service commands, or interpreting logs to decide deployment success are not part of v0.1.

## 5. Domain model

### Application

```text
Application
- id
- display_name
- artifact_type = jar
- environments
```

### Environment

```text
Environment
- name
- target
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

```text
Artifact
- version
- sha256
- size_bytes
```

`artifact_path` is request/application input used to read and upload the local file; it is intentionally not part of the durable Artifact value object.

### Deployment

```text
Deployment
- id
- application
- environment
- artifact
- state
```

Operational history is not duplicated into mutable aggregate fields. Timestamps, state-transition history, step-attempt status, and step failures are durable repository records. The application result (`DeploymentOutcome`) separately carries the primary deployment failure and automatic-rollback failure for the current request.

### DeploymentPlan

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

### RollbackReference

A rollback reference proves which configured capability context belongs to a specific deployment:

```text
RollbackReference
- deployment_id
- application
- environment
- target
- backup_path
- install_path
- rollback_task
- restart_task
- health_check_task
- state = ACTIVE | SUPERSEDED | CONSUMED
```

The reference is application-owned durable state. It does not grant new remote permissions; `remote-exec-mcp` still validates the named tasks and their typed parameters.

### RollbackOperation

Explicit rollback is a separate aggregate/use case:

```text
RollbackOperation
- id
- source_deployment_id
- application
- environment
- state = STARTED | SUCCEEDED | FAILED
```

The source Deployment remains terminal. An explicit rollback never reopens `SUCCEEDED` or rewrites its historical state.

## 6. Deployment state machine

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

> Failures before the first live-artifact mutation do not trigger rollback. Failures after the mutation boundary enter an explicit automatic-rollback outcome only when a rollback point is available; otherwise they end as `FAILED`.

> `SUCCEEDED` remains terminal. Historical explicit rollback is represented by a separate `RollbackOperation`.

## 7. Deployment and rollback semantics

### PRECHECK

Checks deterministic prerequisites before mutation:

- target is reachable;
- required remote-exec task names exist/are allowed;
- artifact is a readable, non-empty regular file and its checksum/size are computed before remote work;
- no conflicting deployment/rollback mutation is active for the same application/environment.

Capability preflight validates task presence by name. The actual typed task-parameter schema remains owned and enforced by `remote-exec-mcp`.

### STAGE_ARTIFACT

Uses `RemoteExecutionPort.upload_file` to copy the artifact to the configured staging location. The returned byte count must equal the size hashed before deployment. A mismatch is treated as `artifact_changed` and the workflow does not proceed to backup/install.

### BACKUP_CURRENT

Creates a rollback point through the configured approved backup task. Backup remains before the live-artifact mutation boundary.

### INSTALL

Moves or installs the staged artifact into the configured live location through the approved install task. This is the first live-artifact mutation.

### RESTART

Invokes the configured approved restart task.

### VERIFY

Invokes the configured deterministic health-check task. v0.1 does not accept AI interpretation of arbitrary logs as the success criterion.

### AUTOMATIC ROLLBACK

After an install/restart/verify failure:

```text
restore backup task
  -> restart task
  -> health-check task
  -> ROLLED_BACK | ROLLBACK_FAILED
```

The rollback outcome never overwrites the primary deployment failure.

### EXPLICIT ROLLBACK

Historical explicit rollback accepts only `deployment_id` and performs:

```text
load terminal source Deployment
  -> load ACTIVE RollbackReference
  -> verify source is still latest for application/environment
  -> verify current environment contract matches persisted capability snapshot
  -> acquire mutation exclusion
  -> check target + rollback/restart/health capabilities
  -> restore persisted backup_path to persisted install_path
  -> restart
  -> verify
  -> RollbackOperation SUCCEEDED | FAILED
```

A newer deployment record conservatively invalidates an older reference before remote mutation, even when the newer attempt may not have changed the live artifact. This fail-closed rule is intentional for v0.1 because a fixed backup location cannot otherwise prove ownership after later deployment activity.

Successful explicit rollback consumes the reference atomically with the operation result. A failed explicit rollback leaves the reference active so a controlled retry can be attempted.

## 8. RemoteExecutionPort

```text
RemoteExecutionPort
- check_target(target)
- list_tasks(target)
- upload_file(target, local_path, remote_path, overwrite)
- run_task(target, task, parameters)
```

The first adapter calls `remote-exec-mcp` over MCP stdio using a configured child process. No SSH library belongs in `deploy-mcp` v0.1.

The adapter owns only MCP request/response conversion and structured remote error decoding. Remote-exec failures preserve their structured `{code, message}` cause and remain distinguishable from transport/protocol failures or malformed responses.

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

The matching `remote-exec-mcp` task definitions must declare and validate these parameters. The names above are a deterministic adapter contract, not arbitrary filesystem access.

## 9. MCP surface

### list_applications

Returns configured application/environment summaries without remote access.

### deploy_application

```json
{
  "application": "demo-service",
  "environment": "test",
  "version": "1.2.3",
  "artifact_path": "/local/demo-service-1.2.3.jar"
}
```

Returns the deployment result plus whether a durable explicit-rollback reference was successfully recorded. It never returns raw SSH command output.

### get_deployment

Returns durable deployment state, transitions, and step outcomes.

### list_deployments

Returns recent deployment records with bounded filtering.

### rollback_deployment

```json
{
  "deployment_id": "deployment-uuid"
}
```

The tool exposes the independent explicit rollback use case. The caller cannot provide remote path, target, task, credential, service, or shell parameters.

No MCP tool accepts raw shell, SSH credentials, arbitrary remote service names, arbitrary remote deployment paths, or shell script bodies.

## 10. Idempotency and concurrency

Deployment and explicit rollback are both mutations of `(application, environment)`.

In-process:

- `DeploymentLockManager` serializes deployment and explicit rollback calls for the same application/environment.

Durable SQLite boundary:

- a partial unique index prevents two non-terminal deployments for one application/environment;
- a partial unique index prevents two `STARTED` rollback operations for one application/environment;
- a trigger prevents new deployment creation while an explicit rollback is `STARTED`;
- a trigger prevents an explicit rollback from starting while a deployment is non-terminal.

The durable database constraint is the final correctness authority. Process-local locking is only early rejection.

A crash that leaves a deployment non-terminal or explicit rollback `STARTED` intentionally fails closed until Phase 7 recovery policy resolves it.

Idempotency-key semantics and same-version/different-checksum retry policy remain Phase 7 hardening items.

## 11. Persistence

SQLite implements separate application-owned persistence ports while using the same durable database.

`DeploymentRepository` stores:

- deployment identity, artifact version/checksum/size, and current state;
- deployment step attempts and their error text;
- state-transition history;
- timestamps for durable records.

`RollbackRepository` stores:

- deployment-bound rollback references and lifecycle state;
- explicit rollback operations and terminal outcome;
- the capability snapshot required to prove that rollback is still valid.

State/history transitions use transactional or expected-state guards. Successful explicit rollback updates the operation and consumes the rollback reference in one transaction.

Rollback-reference persistence happens after the deployment workflow reaches its terminal result. If reference persistence fails, the deployment result is not falsified; the response reports rollback-reference unavailability separately.

Do not persist SSH secrets or arbitrary credentials. Remote-exec owns its own security/audit boundary.

## 12. Error model

Errors are stable and machine-readable. Current categories include:

```text
invalid_request
invalid_configuration
unknown_application
unknown_environment
unknown_deployment
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

Remote-exec errors preserve their structured remote code where useful, while the deployment-level error describes the failed deployment/rollback operation.

## 13. Audit boundary

```text
remote-exec audit
= what remote capability was invoked and how it ended

deploy history
= why that capability was invoked as part of deployment/rollback operation and how durable state changed
```

This separation is intentional because the histories answer different questions.

## 14. Configuration principle

Configuration is declarative:

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

The remote-exec executable/arguments are startup configuration, never MCP tool inputs. Paths are deployment configuration and cross the port only through the structured task contract. Actual authorization, parameter validation, and filesystem/execution safety remain independently enforced by `remote-exec-mcp`.

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

## 16. Design acceptance criteria

The architecture is considered preserved when all of the following remain true:

1. no domain/application module imports SSH/SFTP implementation types;
2. no MCP tool accepts raw shell or remote credentials;
3. deployment paths cross the remote port only as structured task parameters;
4. all deployment target mutations are governed by the deployment lifecycle;
5. success requires deterministic verification;
6. automatic rollback is explicit and separately observable from the primary failure;
7. explicit rollback uses an independent durable `RollbackOperation` and never reopens a terminal Deployment;
8. explicit rollback accepts only deployment intent and uses a deployment-bound durable reference for remote capability/path values;
9. newer deployment activity or environment-contract drift invalidates historical rollback before remote work;
10. deployment and explicit rollback mutation exclusion has a durable database-level boundary, not only an in-memory lock;
11. deployment state, step history, rollback references, and rollback operations are durable;
12. remote execution authorization and typed task validation remain enforced by `remote-exec-mcp`;
13. adding Docker/Kubernetes later does not require transport-specific branches throughout the deployment domain.
