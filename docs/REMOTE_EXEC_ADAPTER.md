# Remote execution adapter

`deploy-mcp` owns deployment orchestration. It does not own SSH, SFTP, host-key policy, credential handling, remote command quoting, or remote filesystem authorization.

The v0.1 integration is an MCP client adapter:

```text
deploy-mcp application
        |
        v
RemoteExecutionPort
        |
        v
RemoteExecMcpAdapter
        |
        v
remote-exec-mcp child process (stdio MCP)
        |
        v
SSH / SFTP
```

## Port surface

The application-owned port intentionally mirrors only the remote capabilities needed by deployment:

- `check_target`
- `list_tasks`
- `upload_file`
- `run_task`

There is no raw-shell method and no generic SSH session handle.

## Capability preflight

Before deployment mutation, the application layer checks:

1. the configured target is reachable through `check_target`;
2. the target exposes every configured task required by the environment;
3. only after those checks may later workflow phases stage or mutate an artifact.

The adapter does not decide which tasks are required. That is deployment configuration/application logic.

## Process ownership

For v0.1, `deploy-mcp` starts one configured `remote-exec-mcp` child process and communicates over MCP stdio. The executable and arguments are startup configuration, not MCP tool inputs.

Example:

```yaml
remote_exec:
  command: /opt/remote-exec-mcp/bin/remote-exec-mcp
  args:
    - --config
    - /etc/remote-exec-mcp/config.yaml
```

The `--config` flag is part of the remote-exec CLI contract; a bare path is not a valid argument.

This preserves an explicit process boundary while avoiding a second network API. `remote-exec-mcp` continues to own its own configuration, credentials, policies, audit log, timeouts, and concurrency limits.

Because the child is spawned by deploy-mcp, secret environment variables required by remote-exec can be inherited from the deploy-mcp process. Secret values should not be placed in deploy-mcp application configuration or MCP tool arguments.

## Local artifact boundary

The same canonical artifact file crosses two independent authorization layers:

```text
caller artifact_path
  -> deploy-mcp local_artifacts.allowed_roots
  -> canonical local path
  -> RemoteExecutionPort::upload_file
  -> remote-exec-mcp runtime.allowed_local_upload_roots
  -> SFTP remote upload policy
```

`deploy-mcp` authorizes its own local read/hash operation. `remote-exec-mcp` separately authorizes transfer. Neither service treats the other's allowlist as a substitute for its own policy.

## Error boundary

Remote MCP tool errors are decoded as structured `{code, message}` causes. Protocol/transport failures and malformed responses remain distinguishable from remote-exec application errors. Deployment workflow code maps these causes into deployment-level failures while retaining structured remote causes where appropriate.

## Testing

Two test layers intentionally serve different concerns:

1. `FakeRemoteExecution` provides deterministic application/workflow tests without starting a process or SSH connection. It records typed calls and returns configured results.
2. `tests/remote_exec_protocol.rs` starts a disposable Rust MCP stdio server from `tests/fixtures/remote_exec_protocol_fixture.rs` and drives the real `RemoteExecMcpAdapter` through a child process.

The protocol integration suite verifies:

- `check_target`, `list_tasks`, `upload_file`, and `run_task` success shapes over real MCP stdio;
- structured remote `{code, message}` error preservation;
- malformed successful responses are classified as `InvalidResponse` instead of being confused with remote application errors.

The fixture is enabled only by the `protocol-fixture` Cargo feature. CI uses `--all-features`, so the child-process integration is part of the normal quality gate while requiring no SSH host or credentials.

See [`SETUP.md`](SETUP.md) for full composition and client setup.
