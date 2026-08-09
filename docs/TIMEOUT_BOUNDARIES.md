# Timeout Boundaries

## Goal

Bound deployment orchestration without moving SSH/process ownership out of `remote-exec-mcp`.

## Deployment-step timeout

`runtime.deployment_step_timeout_ms` defaults to 120000 ms and is bounded to 1..=3600000 ms.

The timeout is applied by the deployment application layer to:

- remote capability preflight;
- artifact staging upload;
- configured named tasks, including automatic rollback restore/restart/verification.

A timed-out step is durably completed as `FAILED` with stable code `operation_timed_out`. Existing mutation semantics remain authoritative: pre-mutation failures stop without rollback, while install/restart/verification failures enter the normal automatic rollback path.

## Explicit rollback-operation timeout

`runtime.explicit_rollback_timeout_ms` defaults to 300000 ms and is bounded to 1..=3600000 ms.

This timeout covers the complete explicit rollback remote sequence after the durable rollback operation has entered `STARTED`.

On expiry, deploy-mcp does **not** mark the operation `FAILED` and does **not** make the rollback reference immediately retryable. Remote state may be unknown because a side effect can complete after the local caller stops waiting. The durable operation therefore remains `STARTED`; existing SQLite mutation guards block later deployment/rollback mutation for the same application/environment. Startup recovery subsequently converts the interrupted operation into the existing manual-reconciliation path.

This intentionally treats rollback timeout like an interrupted mutation rather than a normal remote error.

## Separation of concerns

deploy-mcp owns orchestration deadlines and state-machine consequences. `remote-exec-mcp` continues to own SSH/SFTP connectivity, command/file-transfer limits, transport cancellation, and remote process behavior. A deploy-mcp timeout bounds how long orchestration waits; it does not claim that a remote process tree has been terminated.
