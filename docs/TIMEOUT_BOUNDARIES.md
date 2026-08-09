# Timeout Boundaries

## Goal

Bound deployment orchestration without moving SSH/process ownership out of `remote-exec-mcp`.

## Deployment-step timeout

`runtime.deployment_step_timeout_ms` defaults to 120000 ms and is bounded to 1..=3600000 ms.

The timeout is applied by the deployment application layer to:

- remote capability preflight;
- artifact staging upload;
- configured named tasks, including backup and automatic rollback restore/restart/verification.

A timed-out step attempt is durably completed as `FAILED` with stable code `operation_timed_out`. The orchestration state consequence depends on whether a remote side effect may already have started.

Two boundaries are intentionally separate:

```text
remote-side-effect ambiguity boundary
  = STAGING_ARTIFACT and later

automatic rollback boundary
  = INSTALLING and later
```

The first boundary protects concurrency after a timeout or crash. The second answers whether a completed ordinary failure should trigger automatic rollback. They must not be conflated.

Timeout consequences:

- a `PRECHECK` timeout can terminate the deployment as `FAILED`, because target/capability checks are required to be non-mutating;
- `STAGING_ARTIFACT` and `BACKING_UP` timeout preserve the current non-terminal deployment state because the upload/backup may still complete remotely after deploy-mcp stops waiting;
- `INSTALL`, `RESTART`, or `VERIFY` timeout likewise preserves the current non-terminal state and does **not** immediately start automatic rollback;
- if an automatic rollback restore/restart/verification step times out, the deployment remains `ROLLING_BACK` rather than being claimed as `ROLLBACK_FAILED`.

The retained non-terminal deployment is a durable mutation guard. A later deployment or explicit rollback for the same application/environment is rejected until startup recovery converts the interrupted orchestration into the recovery/reconciliation path.

Ordinary **completed** non-timeout staging/backup failures can still terminate as `FAILED` without automatic rollback. Ordinary completed install/restart/verification failures continue to use the existing automatic rollback policy. A verification timeout is also not retried: deterministic verification retries are reserved for completed health-check attempts that explicitly report failure.

This is intentionally conservative. `tokio::time::timeout` proves only that deploy-mcp stopped waiting. It does not prove that the remote upload or task stopped, was cancelled, or rolled back its side effects.

## Explicit rollback-operation timeout

`runtime.explicit_rollback_timeout_ms` defaults to 300000 ms and is bounded to 1..=3600000 ms.

This timeout covers the complete explicit rollback remote sequence after the durable rollback operation has entered `STARTED`.

On expiry, deploy-mcp does **not** mark the operation `FAILED` and does **not** make the rollback reference immediately retryable. Remote state may be unknown because a side effect can complete after the local caller stops waiting. The durable operation therefore remains `STARTED`; existing SQLite mutation guards block later deployment/rollback mutation for the same application/environment. Startup recovery subsequently converts the interrupted operation into the existing manual-reconciliation path.

This intentionally treats rollback timeout like an interrupted mutation rather than a normal remote error.

## Separation of concerns

deploy-mcp owns orchestration deadlines and durable state-machine consequences. `remote-exec-mcp` continues to own SSH/SFTP connectivity, command/file-transfer limits, transport cancellation, and remote process behavior. A deploy-mcp timeout bounds how long orchestration waits; it does not claim that a remote process tree or file transfer has been terminated.
