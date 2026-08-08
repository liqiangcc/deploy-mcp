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
   +--> upload_file
   +--> run_task
   v
Remote Linux host
```

Dependency direction:

```text
Domain <- Application <- Adapters
```

The domain/application layers know only `RemoteExecutionPort`. They do not know SSH, SFTP, `rmcp`, or the concrete `remote-exec-mcp` process.

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
2. verify target connectivity/capability;
3. stage the artifact to a bounded remote staging path;
4. record current deployed version when available;
5. back up the current artifact before mutation;
6. install the staged artifact;
7. restart the configured service;
8. verify health deterministically;
9. mark success only after verification;
10. rollback automatically after a post-mutation failure when rollback is possible;
11. persist terminal deployment status.

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
- restart_task
- health_check_task
- optional precheck_task
- optional install_task
- optional rollback_task
```

`target` and task names are references to externally configured capabilities. The deployment domain must not translate them into raw SSH commands.

### Artifact

```text
Artifact
- source_path
- version
- sha256
- size_bytes
```

`version` identifies the logical release. `sha256` identifies the bytes being deployed. A deployment record must preserve both so that retries cannot silently substitute different content under the same version.

### Deployment

```text
Deployment
- id
- application
- environment
- requested_version
- artifact_sha256
- previous_version
- state
- started_at
- finished_at
- failure
- rollback_state
```

### DeploymentPlan

A plan is generated before mutating the target.

```text
DeploymentPlan
- deployment_id
- target
- artifact
- ordered steps
- rollback boundary
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
CREATED / PRECHECKING / STAGING_ARTIFACT
        |
        +---- failure ----> FAILED

BACKING_UP / INSTALLING / RESTARTING / VERIFYING
        |
        +---- failure ----> ROLLING_BACK
                              |
                         +----+----+
                         |         |
                         v         v
                   ROLLED_BACK  ROLLBACK_FAILED
```

Important invariant:

> A deployment is never `SUCCEEDED` merely because the process restarted. It becomes `SUCCEEDED` only after the configured verification step passes.

Another invariant:

> Failures before the first target mutation do not trigger rollback. Failures after the mutation boundary must enter an explicit rollback outcome when rollback is configured.

## 7. Deployment step semantics

### PRECHECK

Checks deterministic prerequisites before mutation, for example:

- target reachable;
- required remote-exec tasks exist/are allowed;
- artifact is readable and checksum is stable;
- no conflicting deployment is active for the same application/environment.

### STAGE_ARTIFACT

Uses `RemoteExecutionPort.upload_file` to copy the artifact to a bounded staging location. Staging must not replace the live artifact.

### BACKUP_CURRENT

Creates/restores a known rollback point through an approved remote task. The deployment record stores the previous version/backup reference when available.

### INSTALL

Moves or installs the staged artifact into the configured live location through an approved task. `deploy-mcp` decides *when* install occurs; `remote-exec-mcp` decides whether the underlying operation is authorized and how it is safely executed.

### RESTART

Invokes the environment's approved restart task.

### VERIFY

Invokes a deterministic health-check task. v0.1 does not accept AI interpretation of arbitrary logs as the success criterion.

### ROLLBACK

Restores the previous artifact and restarts/verifies the service using configured approved tasks. Rollback has its own terminal result and must not overwrite the original failure reason.

## 8. RemoteExecutionPort

The application layer depends on a narrow port similar to:

```text
RemoteExecutionPort
- check_target(target)
- list_tasks(target)
- upload_file(target, local_path, remote_path, overwrite)
- run_task(target, task, parameters)
```

The first adapter calls `remote-exec-mcp`. No SSH library belongs in `deploy-mcp` v0.1.

Why retain `list_tasks` even though deployment config already contains task names: startup/precheck can fail early if a configured capability is missing or not authorized for the target.

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

Explicitly rolls back a successful or failed deployment when its rollback point remains valid.

No MCP tool accepts a raw command, SSH credential, arbitrary service name, arbitrary install path, or shell script body.

## 10. Idempotency and concurrency

Deployment concurrency is different from remote command concurrency.

v0.1 invariant:

> At most one mutating deployment may run for a given `(application, environment)` at a time.

A retry with the same deployment/idempotency key must not create a second mutation sequence.

A retry that reuses the same logical version with a different artifact checksum must be rejected unless explicitly modeled as a different deployment/version.

## 11. Persistence

Deployment state must survive the MCP process lifetime. In-memory state is insufficient because losing the process during `INSTALLING` or `ROLLING_BACK` must not erase what happened.

For v0.1, use a small durable repository abstraction. SQLite is the preferred initial implementation because it provides transactional local persistence without introducing another service.

Suggested persisted records:

- deployment;
- deployment step attempt;
- state transition;
- artifact checksum/version metadata;
- rollback reference/result.

Do not persist SSH secrets or arbitrary remote-exec task parameters containing secret values.

## 12. Error model

Errors should be stable and machine-readable. Initial categories:

```text
unknown_application
unknown_environment
invalid_version
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

Remote-exec errors should be preserved as structured causes where useful, but the public deployment error should describe the deployment-level failure.

Example:

```text
verification_failed
  caused_by: remote_execution_failed
    caused_by: execution_timeout
```

## 13. Audit boundary

Two audit layers are expected and must not be confused:

```text
remote-exec audit
= what remote capability was invoked and how it ended

deploy audit/history
= why that capability was invoked as part of deployment X and how the deployment state changed
```

This duplication is intentional because they answer different questions.

## 14. Configuration principle

Configuration is declarative. It maps deployment concepts to approved remote capabilities.

Example shape:

```yaml
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

Paths are deployment configuration, but actual filesystem authorization remains enforced independently by `remote-exec-mcp` policy.

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
3. all target mutations are represented by deployment state transitions;
4. success requires deterministic verification;
5. rollback is explicit and separately observable;
6. deployment state is durable;
7. remote execution authorization is still enforced by `remote-exec-mcp`;
8. adding Docker/Kubernetes later does not require changing the core deployment state-machine concepts.
