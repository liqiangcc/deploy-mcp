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
  command: remote-exec-mcp
  args:
    - /etc/remote-exec/config.yaml
```

This preserves an explicit process boundary while avoiding a second network API. `remote-exec-mcp` continues to own its own configuration, credentials, policies, audit log, timeouts, and concurrency limits.

## Error boundary

Remote MCP tool errors are decoded as structured `{code, message}` causes. Protocol/transport failures and malformed responses remain distinguishable from remote-exec application errors. A later deployment workflow maps these causes into deployment-level errors such as `remote_capability_missing`, `remote_execution_failed`, or `verification_failed`.

## Testing

A deterministic fake implementation of `RemoteExecutionPort` is provided for application/workflow tests. It records calls and returns configured target checks, task sets, upload results, and task results without starting SSH or an MCP process.
