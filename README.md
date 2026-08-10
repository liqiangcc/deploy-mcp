# deploy-mcp

A deployment-orchestration MCP for AI agents.

`deploy-mcp` owns **deployment semantics and lifecycle**. It does not own SSH, SFTP, credential handling, host-key verification, or arbitrary remote command execution. Those capabilities are delegated through a narrow `RemoteExecutionPort`, with `remote-exec-mcp` as the v0.1 adapter.

## Core boundary

```text
MCP client / AI agent
   |
   v
deploy-mcp
   |
   +-- deployment state machine
   +-- planning / idempotency / mutation guards
   +-- verification / rollback / recovery
   +-- durable SQLite history
   |
   v
RemoteExecutionPort
   |
   v
RemoteExecMcpAdapter
   |
   | MCP stdio child process
   v
remote-exec-mcp
   |
   +-- policy / validation / audit
   +-- SSH command execution
   +-- SFTP file transfer
   v
Remote Linux host
```

> `remote-exec-mcp` answers **"how can this approved remote capability be executed safely?"**  
> `deploy-mcp` answers **"what does a correct deployment mean, and what happens when a step fails?"**

## v0.1 scope

The first release intentionally supports one production-shaped workflow only:

- Java/Spring Boot JAR artifact;
- remote Linux host;
- service managed by systemd;
- staged upload;
- previous-version backup;
- artifact installation;
- service restart;
- deterministic health verification with bounded retry;
- automatic rollback after a completed post-mutation failure;
- explicit deployment-bound rollback;
- durable SQLite deployment, rollback, recovery, and structured history;
- idempotent request replay and same-version artifact identity protection;
- fail-closed startup recovery and operator reconciliation.

Docker, Kubernetes, Helm, build pipelines, log querying, and configuration management are outside the v0.1 boundary.

## Build

Requires a current stable Rust toolchain.

```bash
cargo build --locked --release
```

Main binaries:

```text
target/release/deploy-mcp
target/release/deploy-mcp-recovery
```

`deploy-mcp` also requires a built/configured `remote-exec-mcp` executable.

## Quick start

Start from [`config/example.yaml`](config/example.yaml). The remote-exec child process must be configured as a command plus its real CLI arguments:

```yaml
remote_exec:
  command: /opt/remote-exec-mcp/bin/remote-exec-mcp
  args:
    - --config
    - /etc/remote-exec-mcp/config.yaml

local_artifacts:
  allowed_roots:
    - /var/lib/deploy-mcp/artifacts
```

The same local artifact directory must also be permitted by `remote-exec-mcp`'s `runtime.allowed_local_upload_roots`. The two checks are independent defense-in-depth boundaries.

Run with explicit paths:

```bash
/opt/deploy-mcp/bin/deploy-mcp \
  --config /etc/deploy-mcp/config.yaml \
  --database /var/lib/deploy-mcp/deployments.sqlite
```

The equivalent environment variables are `DEPLOY_MCP_CONFIG` and `DEPLOY_MCP_DATABASE`. Standard output is reserved for MCP stdio frames; diagnostics go to standard error.

For the complete remote-exec contract, artifact-root setup, secrets, recovery operations, smoke-test sequence, and troubleshooting, see **[docs/SETUP.md](docs/SETUP.md)**.

## MCP client configuration

A generic MCP client entry looks like:

```json
{
  "mcpServers": {
    "deploy": {
      "command": "/opt/deploy-mcp/bin/deploy-mcp",
      "args": [
        "--config",
        "/etc/deploy-mcp/config.yaml",
        "--database",
        "/var/lib/deploy-mcp/deployments.sqlite"
      ]
    }
  }
}
```

Secrets required by the remote-exec child should be injected through the operating system, service manager, or a protected client environment mechanism. Do not store SSH credentials in deploy-mcp application configuration or caller tool arguments.

The MCP client connects to `deploy-mcp`; deploy-mcp starts and owns its configured `remote-exec-mcp` child process.

## MCP tools

- `list_applications` — list configured deployment targets without remote access;
- `deploy_application` — run the deterministic JAR/systemd deployment workflow;
- `get_deployment` — read one durable deployment and its transition/attempt details;
- `get_deployment_history` — read a bounded structured history projection;
- `list_deployments` — list recent durable deployments with bounded filters;
- `rollback_deployment` — explicitly roll back through a durable deployment-bound rollback reference.

A deploy request is intentionally narrow:

```json
{
  "application": "demo-service",
  "environment": "test",
  "version": "1.2.3",
  "artifact_path": "/var/lib/deploy-mcp/artifacts/demo-service-1.2.3.jar",
  "idempotency_key": "demo-service-test-1.2.3"
}
```

Callers cannot provide remote staging/install/backup paths, task names, shell commands, argv, SSH credentials, or arbitrary service names.

## Deployment semantics

Success path:

```text
validate + authorize artifact
  -> acquire mutation guard
  -> persist CREATED
  -> check target + required capabilities
  -> stage artifact
  -> backup current artifact
  -> install staged artifact        # first live mutation boundary
  -> restart
  -> verify
  -> SUCCEEDED
```

Failures before the live `INSTALL` boundary terminate without automatic rollback. Completed install/restart/verification failures can enter the configured rollback path when a rollback point exists. A timeout after possible live mutation is treated as unknown remote state and remains fail-closed until recovery/reconciliation instead of guessing that rollback is safe.

## Safety invariants

- no SSH/SFTP implementation in deploy-mcp;
- no raw-shell MCP tool;
- caller DTOs reject undeclared target/path/task/command/credential controls;
- local artifact access is canonicalized and allowlisted before hashing or remote work;
- remote-exec independently enforces its own local/remote transfer policy;
- application/environment configuration is the authority for targets, paths, and named tasks;
- mutation guards exist both in-process and durably across SQLite repository instances;
- rollback authority is bound to durable deployment records, not caller-provided paths/tasks;
- unresolved crash/timeout state blocks mutation until deterministic recovery or operator reconciliation.

See [`docs/THREAT_MODEL.md`](docs/THREAT_MODEL.md) for the full trust model.

## Development and validation

CI requires:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

The all-features test suite includes a disposable real MCP stdio child-process fixture for `RemoteExecMcpAdapter`. It verifies a successful protocol round-trip, structured remote error preservation, and malformed-response classification without requiring a live SSH host.

## Documentation

- [Setup / client guide](docs/SETUP.md)
- [Design](DESIGN.md)
- [Roadmap](ROADMAP.md)
- [Remote execution adapter](docs/REMOTE_EXEC_ADAPTER.md)
- [MCP adapter](docs/MCP_ADAPTER.md)
- [Threat model](docs/THREAT_MODEL.md)
- [Startup recovery](docs/STARTUP_RECOVERY.md)
- [Operator reconciliation](docs/OPERATOR_RECONCILIATION.md)
- [Structured audit history](docs/STRUCTURED_AUDIT_HISTORY.md)
- [Idempotency](docs/DEPLOYMENT_IDEMPOTENCY.md)
