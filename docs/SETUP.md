# deploy-mcp v0.1 setup guide

This guide covers the production-shaped v0.1 path:

```text
MCP client
  -> deploy-mcp
  -> RemoteExecMcpAdapter
  -> remote-exec-mcp child process over stdio
  -> SSH / SFTP
  -> remote Linux host + systemd service
```

`deploy-mcp` owns deployment lifecycle and durable state. `remote-exec-mcp` owns SSH/SFTP, credentials, host-key policy, remote task authorization, transfer policy, and its own audit log.

## 1. Prerequisites

- a current stable Rust toolchain;
- a Linux host with the application managed by systemd;
- a working `remote-exec-mcp` configuration for that host;
- a local directory dedicated to deployment artifacts;
- filesystem access for a durable SQLite database.

Do not expose an unrestricted shell to `deploy-mcp`. The deployment adapter intentionally uses only `check_target`, `list_tasks`, `upload_file`, and `run_task` from `remote-exec-mcp`.

## 2. Build

Build `remote-exec-mcp` from its repository and `deploy-mcp` from this repository:

```bash
cargo build --release
```

The deploy binaries are produced under `target/release/`:

```text
deploy-mcp
deploy-mcp-recovery
```

For a machine-wide installation, copy the binaries to stable absolute paths such as `/opt/deploy-mcp/bin/` and `/opt/remote-exec-mcp/bin/` rather than depending on a developer checkout.

## 3. Configure remote-exec-mcp first

`deploy-mcp` never creates SSH targets or remote tasks. Those capabilities must already be declared and authorized in `remote-exec-mcp`.

For each deploy environment, the remote-exec configuration must provide:

- a target identifier matching `applications.<app>.environments.<env>.target`;
- the deployment task names referenced by deploy-mcp;
- a local upload root that permits the artifact directory used by deploy-mcp;
- a remote upload root that permits the configured staging path;
- normal remote-exec authentication, host-key, timeout, transfer-size, and audit policies.

A deployment environment typically references these declarative tasks:

```text
precheck (optional deployment-specific check)
backup
install
restart
health_check
rollback
```

The task names are capabilities, not shell strings supplied by the MCP caller. `deploy-mcp` validates at preflight that all configured required tasks are currently exposed by `remote-exec-mcp` before staging or live mutation begins.

### Artifact-root contract

There are two independent local-file authorization checks:

```text
MCP caller artifact_path
  -> deploy-mcp local_artifacts.allowed_roots
  -> canonical artifact path
  -> remote-exec-mcp runtime.allowed_local_upload_roots
  -> SFTP upload
```

The same artifact directory therefore needs to be allowed by both services. This is intentional defense in depth; deploy-mcp authorization does not replace remote-exec transfer policy.

Example shared root:

```text
/var/lib/deploy-mcp/artifacts
```

Configured roots must exist before use so they can be canonicalized safely.

## 4. Configure deploy-mcp

Start from [`../config/example.yaml`](../config/example.yaml).

A representative configuration is:

```yaml
remote_exec:
  command: /opt/remote-exec-mcp/bin/remote-exec-mcp
  args:
    - --config
    - /etc/remote-exec-mcp/config.yaml

local_artifacts:
  allowed_roots:
    - /var/lib/deploy-mcp/artifacts

runtime:
  deployment_step_timeout_ms: 120000
  explicit_rollback_timeout_ms: 300000
  verification_max_attempts: 3
  verification_retry_delay_ms: 1000
  rollback_reference_retention_days: 30
  rollback_reference_cleanup_batch_size: 500

applications:
  demo-service:
    display_name: Demo Service
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

Important boundaries:

- `remote_exec.command` and `remote_exec.args` are startup configuration, never MCP inputs;
- staging/install/backup paths are operator configuration, never MCP inputs;
- task names are operator configuration, never MCP inputs;
- `local_artifacts.allowed_roots` is fail-closed: an omitted or empty list disables new artifact deployments;
- callers can choose only a configured application/environment plus a local artifact path that survives canonical allowlist validation.

## 5. Secrets and environment inheritance

`remote-exec-mcp` should resolve credentials from its supported secret references, for example an environment variable containing an SSH private key.

Because v0.1 starts `remote-exec-mcp` as a child process, environment variables available to `deploy-mcp` are inherited by that child unless the operating environment overrides inheritance.

For example:

```bash
export REMOTE_EXEC_SSH_KEY="$(cat ~/.ssh/id_ed25519)"
```

Do not put private keys, passwords, or other secret values in deploy-mcp application configuration or MCP client arguments.

## 6. Run deploy-mcp

Use explicit absolute configuration and database paths in production:

```bash
/opt/deploy-mcp/bin/deploy-mcp \
  --config /etc/deploy-mcp/config.yaml \
  --database /var/lib/deploy-mcp/deployments.sqlite
```

Equivalent environment variables are supported:

```text
DEPLOY_MCP_CONFIG
DEPLOY_MCP_DATABASE
```

Standard output is reserved for MCP stdio frames. Diagnostics are written to standard error.

Startup order is intentionally fail-closed:

1. load and validate deploy configuration;
2. recover interrupted deployment/rollback records;
3. apply rollback-reference retention maintenance;
4. open durable repositories;
5. start the configured `remote-exec-mcp` child process;
6. serve the deploy MCP over stdio.

Unresolved post-mutation recovery incidents continue to block mutation until an operator explicitly reconciles them. See [`OPERATOR_RECONCILIATION.md`](OPERATOR_RECONCILIATION.md) and [`STARTUP_RECOVERY.md`](STARTUP_RECOVERY.md).

## 7. MCP client configuration

A generic stdio MCP client entry is:

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

If the child `remote-exec-mcp` requires secret environment variables, inject them through the operating system, service manager, or the MCP client's protected environment mechanism. Do not embed secret values in source-controlled client configuration.

The MCP client connects only to `deploy-mcp`. It does not need a second direct remote-exec connection for the deployment workflow because deploy-mcp owns and starts its configured remote-exec child process.

## 8. AI-facing tool surface

v0.1 exposes these deployment tools:

- `list_applications` — list configured applications/environments without remote access;
- `deploy_application` — execute the deterministic JAR/systemd deployment workflow;
- `get_deployment` — read one durable deployment and its transitions/attempts;
- `get_deployment_history` — read a bounded structured audit timeline;
- `list_deployments` — list recent durable deployments with bounded filters;
- `rollback_deployment` — explicitly roll back using the durable deployment-bound rollback reference.

A deployment request is intentionally narrow:

```json
{
  "application": "demo-service",
  "environment": "test",
  "version": "1.2.3",
  "artifact_path": "/var/lib/deploy-mcp/artifacts/demo-service-1.2.3.jar",
  "idempotency_key": "demo-service-test-1.2.3"
}
```

The caller cannot provide target identifiers beyond the configured environment mapping, remote deployment paths, remote task names, service names, shell commands, argv, SSH passwords, or private keys.

## 9. Smoke-test sequence

Before the first mutation, verify the composition in this order:

1. start the same deploy-mcp command the MCP client will use;
2. call `list_applications` and confirm the expected application/environment is present;
3. place a test JAR below an allowed local artifact root;
4. call `deploy_application` with a unique version and idempotency key;
5. read the returned deployment with `get_deployment`;
6. inspect `get_deployment_history` for durable transitions and step attempts;
7. confirm the systemd service is healthy through the configured health-check task.

Preflight will fail before deployment mutation if the target is unreachable or required remote-exec tasks are missing/not authorized.

## 10. Deployment and rollback semantics

The success path is fixed:

```text
validate + authorize local artifact
  -> acquire deployment mutation guard
  -> persist CREATED
  -> remote capability preflight
  -> stage artifact
  -> backup current artifact
  -> install staged artifact        # first live-artifact mutation boundary
  -> restart
  -> verify
  -> SUCCEEDED
```

A completed post-mutation failure with a valid rollback point triggers the configured automatic rollback path. A remote operation timeout after live mutation is treated differently: completion is unknown, so deploy-mcp preserves a durable mutation guard instead of guessing that rollback is safe.

An explicit historical rollback accepts only `deployment_id`. It uses the durable rollback capability snapshot recorded for that deployment and rejects stale/newer-deployment or configuration-drift cases before remote mutation.

## 11. Idempotency

Use an idempotency key when an MCP client may retry a deployment request after a transport interruption.

An exact replay of a completed deployment intent returns the original durable deployment without repeating remote work. Reusing a key with different application/environment/version/artifact identity fails with `idempotency_conflict`.

A version string is also immutable for one application/environment with respect to artifact checksum and size. Reusing the same version with changed bytes fails with `artifact_version_conflict`.

## 12. Recovery operations

`deploy-mcp-recovery` is an operator-facing companion binary for unresolved recovery incidents. Recovery/reconciliation is intentionally separate from the normal AI-facing mutation tools so an AI caller cannot acknowledge unknown remote state on its own.

See:

- [`STARTUP_RECOVERY.md`](STARTUP_RECOVERY.md)
- [`OPERATOR_RECONCILIATION.md`](OPERATOR_RECONCILIATION.md)
- [`STRUCTURED_AUDIT_HISTORY.md`](STRUCTURED_AUDIT_HISTORY.md)

## 13. Troubleshooting

### remote-exec child fails to start

Check that `remote_exec.command` is an executable path or resolvable command and that its arguments use the remote-exec CLI contract:

```yaml
args:
  - --config
  - /absolute/path/to/remote-exec-config.yaml
```

### deployment says a remote capability is missing

The target must expose every task configured for that deploy environment. Compare deploy-mcp task references with the target's `allowed_tasks` and the remote-exec task catalog.

### artifact path is rejected

Check all of the following:

- the artifact exists;
- the path canonicalizes below a deploy-mcp `local_artifacts.allowed_roots` entry;
- no symlink escapes that root;
- the same canonical path is allowed by remote-exec `runtime.allowed_local_upload_roots`;
- the file is within remote-exec transfer-size policy.

### a new deployment remains blocked after a crash/timeout

Inspect startup recovery output and the durable history. If remote mutation may have completed but its result is unknown, the fail-closed guard requires operator reconciliation rather than automatic continuation.

## 14. Validation before release

The repository quality gates are:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

`cargo test --all-features` also starts a disposable MCP stdio protocol fixture and exercises `RemoteExecMcpAdapter` through a real child process. The integration suite verifies successful protocol round-trips, structured remote errors, and malformed-success response classification without requiring SSH or a live remote host.
