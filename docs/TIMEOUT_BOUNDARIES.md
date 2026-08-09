# Timeout Boundaries

## Goal

Bound deployment orchestration without moving SSH/process ownership out of `remote-exec-mcp`.

## Deployment-step timeout

`runtime.deployment_step_timeout_ms` defaults to 120000 ms and is bounded to 1..=3600000 ms.

The timeout is applied by the deployment application layer to:

- remote capability preflight;
- artifact staging upload;
- configured named tasks, including automatic rollback restore/restart/verification.

A timed-out step attempt is durably completed as `FAILED` with stable code `operation_timed_out`. The orchestration state consequence depends on whether live remote mutation may already have started:

- `PRECHECK`, staging, and backup timeout before the live `INSTALL` boundary can terminate the deployment as `FAILED`;
- `INSTALL`, `RESTART`, or `VERIFY` timeout does **not** immediately start automatic rollback, because the timed-out remote action may still complete after deploy-mcp stopped waiting;
- a post-mutation timeout therefore leaves the deployment in its current non-terminal state (`INSTALLING`, `RESTARTING`, or `VERIFYING`) so the existing durable active-deployment guard blocks later mutation until startup recovery/manual reconciliation;
- if an automatic rollback restore/restart/verification step itself times out, the deployment remains `ROLLING_BACK` rather than being claimed as `ROLLBACK_FAILED`.

Ordinary non-timeout install/restart/verification failures still use the existing automatic rollback policy. A verification timeout is also not retried: deterministic verification retries are reserved for completed health-check attempts that explicitly report failure. Timeout proves only that the local deadline expired, not that the remote action stopped or failed.

## Explicit rollback-operation timeout

`runtime.explicit_rollback_timeout_ms` defaults to 300000 ms and is bounded to 1..=3600000 ms.

This timeout covers the complete explicit rollback remote sequence after the durable rollback operation has entered `STARTED`.

On expiry, deploy-mcp does **not** mark the operation `FAILED` and does **not** make the rollback reference immediately retryable. Remote state may be unknown because a side effect can complete after the local caller stops waiting. The durable operation therefore remains `STARTED`; existing SQLite mutation guards block later deployment/rollback mutation for the same application/environment. Startup recovery subsequently converts the interrupted operation into the existing manual-reconciliation path.

This intentionally treats rollback timeout like an interrupted mutation rather than a normal remote error.

## Separation of concerns

deploy-mcp owns orchestration deadlines and state-machine consequences. `remote-exec-mcp` continues to own SSH/SFTP connectivity, command/file-transfer limits, transport cancellation, and remote process behavior. A deploy-mcp timeout bounds how long orchestration waits; it does not claim that a remote process tree has been terminated.
