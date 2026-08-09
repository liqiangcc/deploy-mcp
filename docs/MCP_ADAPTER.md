# MCP adapter contract

## Responsibility

The MCP layer is a protocol adapter over the application-owned `DeploymentApi` inbound port.

```text
AI Agent
  -> MCP stdio
  -> DeployMcp
  -> DeploymentApi
  -> DeploymentApplication / DeployService
  -> DeploymentRepository + RemoteExecutionPort
```

The MCP layer does not own deployment state transitions, rollback decisions, SSH/SFTP, remote command construction, filesystem authorization, or task authorization.

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

### `get_deployment`

Read-only. Returns the durable deployment aggregate plus state-transition history and step-attempt history.

### `list_deployments`

Read-only. Supports an optional application filter and an optional environment filter. Environment requires application. The record count is bounded to `1..=200` and defaults to 50.

## Error contract

Application failures are returned as structured MCP tool errors:

```json
{
  "code": "unknown_application",
  "message": "unknown application: demo"
}
```

The MCP handler does not reinterpret deployment failures from logs or raw command output.

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
3. starts the configured `remote-exec-mcp` child process;
4. builds `DeploymentApplication`;
5. exposes it through `DeployMcp` over stdio.

## Why `rollback_deployment` is not exposed yet

Automatic rollback during a deployment is safe because it consumes the rollback point created by that same active deployment before any later deployment can replace it.

Explicit rollback by historical `deployment_id` has a different requirement. The current environment model uses a fixed `backup_path`. A later deployment can overwrite that path, so the historical deployment id alone does not prove that the bytes at `backup_path` still belong to that deployment.

Therefore `rollback_deployment` must not be implemented as:

```text
historical deployment id
  -> current configured backup_path
  -> restore
```

That would turn a deterministic API into a potentially incorrect mutation.

Before exposing explicit rollback, deploy-mcp needs a separate rollback application model with a durable deployment-bound rollback reference (or an equivalently strong validity rule). `SUCCEEDED` remains terminal for the original `Deployment` aggregate; explicit rollback must be a separate operation rather than reopening it.
