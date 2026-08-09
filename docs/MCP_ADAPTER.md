# MCP adapter contract

## Responsibility

The MCP layer is a protocol adapter over the application-owned `DeploymentApi` inbound port.

```text
AI Agent
  -> MCP stdio
  -> DeployMcp
  -> DeploymentApi
  -> DeploymentApplication / DeployService / RollbackService
  -> DeploymentRepository + RollbackRepository + RemoteExecutionPort
```

The MCP layer does not own deployment state transitions, rollback validity rules, SSH/SFTP, remote command construction, filesystem authorization, or task authorization.

## Current tools

### `list_applications`

Read-only. Returns configured application ids, display names, artifact type, and environment ids. It performs no remote access.

### `deploy_application`

Input:

```json
{
  "application": "demo-service",
  "environment": "test",
  "version": "1.2.3",
  "artifact_path": "/local/artifacts/demo-service-1.2.3.jar"
}
```

The local `artifact_path` is the only path accepted from the caller. Remote staging/install/backup paths remain declarative server configuration.

The input schema rejects undeclared fields. Raw shell, SSH credentials, remote service names, and remote deployment paths are therefore not part of the MCP capability surface.

A successful deployment response also reports whether a durable explicit-rollback reference was recorded. Failure to persist that reference is reported separately and does not rewrite an already successful deployment outcome as failed.

### `get_deployment`

Read-only. Returns the durable deployment aggregate plus state-transition history and step-attempt history.

### `list_deployments`

Read-only. Supports an optional application filter and an optional environment filter. Environment requires application. The record count is bounded to `1..=200` and defaults to 50.

### `rollback_deployment`

Input:

```json
{
  "deployment_id": "deployment-uuid"
}
```

This is an explicit historical rollback use case. The caller supplies only the source deployment id. Backup/install paths, target, task names, credentials, and shell fragments are never caller-controlled.

The application flow is:

```text
source deployment id
  -> load terminal Deployment
  -> load ACTIVE deployment-bound RollbackReference
  -> verify source is still the latest deployment record for application/environment
  -> verify current environment contract still matches the reference snapshot
  -> acquire mutation exclusion
  -> check target + required rollback capabilities
  -> run configured rollback task using persisted backup/install paths
  -> restart
  -> deterministic health check
  -> RollbackOperation SUCCEEDED | FAILED
```

`RollbackReference` snapshots the capability context created for the source deployment:

```text
deployment_id
application / environment
target
backup_path / install_path
rollback_task / restart_task / health_check_task
state = active | superseded | consumed
```

A fixed environment `backup_path` alone is not treated as proof that a historical deployment still owns those bytes. A newer deployment record conservatively invalidates the historical reference, and configuration drift rejects the rollback before remote mutation.

The original `Deployment` remains terminal. Explicit rollback creates an independent durable `RollbackOperation`; it never reopens a `SUCCEEDED` deployment aggregate.

On explicit rollback success, the operation is marked `SUCCEEDED` and the reference is consumed atomically. On rollback failure, the operation is marked `FAILED` and the reference remains active so a controlled retry is possible.

## Mutation concurrency

Deploy and explicit rollback mutate the same environment and therefore share one mutation boundary.

Within one process, `DeploymentLockManager` serializes `(application, environment)` operations. Across separate repository/process instances sharing the SQLite database:

- a non-terminal deployment prevents a rollback operation from entering `STARTED`;
- a `STARTED` rollback operation prevents creation of a new deployment for the same application/environment.

SQLite constraints/triggers are the durable authority. Process-local locks are an early rejection optimization, not the correctness boundary.

A process crash that leaves a rollback operation in `STARTED` requires the Phase 7 startup-recovery policy; until recovered, the durable guard intentionally fails closed.

## Error contract

Application failures are returned as structured MCP tool errors:

```json
{
  "code": "rollback_unavailable",
  "message": "a newer deployment exists for this application/environment"
}
```

The MCP handler does not reinterpret deployment or rollback failures from logs or raw command output.

## Server composition

The binary is a real stdio MCP server. stdout is reserved for JSON-RPC frames; logging is written to stderr.

Startup configuration:

```text
deploy-mcp [--config PATH] [--database PATH]

DEPLOY_MCP_CONFIG
DEPLOY_MCP_DATABASE
```

The default config path is `config/example.yaml`. The default SQLite path is `deployments.sqlite`.

At startup the composition root:

1. loads and validates deploy-mcp configuration;
2. opens the SQLite deployment repository;
3. opens the SQLite rollback repository against the same database;
4. starts the configured `remote-exec-mcp` child process;
5. builds `DeploymentApplication` with deployment and rollback application services;
6. exposes it through `DeployMcp` over stdio.

## Safety evidence

Dedicated explicit-rollback tests cover:

- successful rollback uses the deployment-bound capability snapshot and consumes the reference;
- failed rollback leaves the reference active and can be retried;
- a newer deployment record invalidates historical rollback before any remote call;
- a started rollback blocks a deployment from a separate application/repository instance sharing the same SQLite database;
- environment contract drift rejects rollback before any remote call;
- the MCP rollback schema rejects caller-supplied remote paths and arbitrary task names.
