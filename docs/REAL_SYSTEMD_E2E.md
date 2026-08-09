# Real remote-exec + systemd end-to-end acceptance

This acceptance test validates the production process boundary instead of the protocol fixture boundary.

## Chain under test

```text
Rust MCP client
  -> deploy-mcp stdio server
  -> RemoteExecMcpAdapter
  -> pinned real remote-exec-mcp stdio server
  -> SSH/SFTP to 127.0.0.1:22222
  -> disposable deploy user
  -> real systemd unit on the GitHub-hosted Ubuntu VM
```

The workflow deliberately keeps responsibilities separated:

- `tests/real_systemd_e2e.rs` owns MCP calls, generated deploy/remote-exec configuration, and acceptance assertions.
- `scripts/e2e/setup_real_systemd_target.sh` owns disposable host setup: SSH key/host key, sshd, deploy user, seeded application, systemd unit, and the narrowly scoped sudo rule for one service restart.
- `remote-exec-mcp` continues to own SSH/SFTP, host-key verification, task policy, task-parameter validation, transfer policy, command serialization, timeouts, and audit output.
- deploy-mcp continues to own orchestration, durable state, rollback references, and the MCP-facing API. It does not gain SSH or systemd implementation code.

## Dependency pin

The workflow checks out `liqiangcc/remote-exec-mcp` at:

```text
d2550b7d69682927be4c0d2077293e8b9df7f4e4
```

The commit is pinned so a deploy-mcp acceptance result refers to an exact remote-exec implementation. Updating that pin is an explicit compatibility change and should be reviewed with the E2E result.

## Disposable target

The GitHub Actions Ubuntu VM is used as the target host itself, but all deploy-mcp remote operations still cross an actual SSH/SFTP connection through a dedicated sshd listening on loopback port `22222`.

The setup creates:

- user `deploy`;
- strict known-host verification with a per-run host key;
- key-only SSH authentication with a per-run client key;
- `/home/deploy/staging`, `/home/deploy/app`, and `/home/deploy/backup` capability roots;
- a real `deploy-mcp-e2e.service` systemd unit;
- a passwordless sudo rule allowing only `systemctl restart deploy-mcp-e2e.service`;
- a seeded v0 Java JAR and a candidate v1 Java JAR.

No long-lived credentials are stored in the repository. `REMOTE_EXEC_SSH_KEY` is created at runtime and inherited by the deploy-mcp child and then by the real remote-exec-mcp child.

## Acceptance scenario

The test performs the following through deploy-mcp MCP tools:

1. `list_applications` confirms the configured application surface.
2. `deploy_application` deploys the v1 JAR.
3. The deployment must finish as `succeeded` and produce an active rollback reference.
4. The actual systemd service must restart and report that the v1 JAR is running.
5. `get_deployment` must show only succeeded step attempts.
6. `get_deployment_history` must remain free of remote paths/task names at the AI-facing boundary.
7. `rollback_deployment` must succeed using only the deployment id.
8. The real systemd service must restart back onto the seeded v0 JAR.
9. The remote-exec audit log must prove that upload/install/rollback operations crossed the real remote-exec process, while not containing the SSH secret reference value.

## Running

The workflow is available as `Real remote-exec systemd E2E`. It runs when its own acceptance assets change and can also be invoked manually with `workflow_dispatch`.

For local execution, reproduce the disposable target setup on a Linux systemd host, export the environment variables emitted by `setup_real_systemd_target.sh`, set `DEPLOY_MCP_E2E_REMOTE_EXEC_BIN` to a built compatible remote-exec-mcp binary, and run:

```bash
DEPLOY_MCP_REAL_E2E=1 cargo test --locked --test real_systemd_e2e -- --nocapture
```

Normal `cargo test` remains hermetic: without `DEPLOY_MCP_REAL_E2E`, the real-host test exits immediately after printing a skip message.
