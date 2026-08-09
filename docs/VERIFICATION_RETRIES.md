# Deterministic verification retries

`deploy-mcp` retries only verification (`health_check`) tasks. It does not retry upload, backup, install, restart, restore, or precheck work because those operations may have side effects.

## Configuration

```yaml
runtime:
  verification_max_attempts: 3
  verification_retry_delay_ms: 1000
```

`verification_max_attempts` includes the first health-check call and must be between 1 and 10. `verification_retry_delay_ms` is a fixed delay from 0 through 60000 milliseconds. There is no jitter, exponential backoff, or random scheduling, so the retry sequence is reproducible from configuration and durable history.

## Deployment and automatic rollback

Each health-check attempt uses the existing deployment-step timeout and creates a separate durable `DeploymentStep::Verify` attempt. Intermediate failures remain visible in deployment history. The first success stops the loop. If all attempts fail, the last failure is returned and the existing state machine decides whether the deployment fails or rolls back.

Automatic rollback uses the same verification policy after restore and restart. Restore and restart themselves remain single-attempt.

## Explicit rollback

Explicit rollback retries only its final health check. Those attempts and fixed delays execute inside the existing whole-operation timeout. If that outer deadline expires, remote state is treated as unknown: the rollback operation remains `STARTED`, further mutation stays blocked, and startup recovery/manual reconciliation must resolve the incident.

## Ownership boundary

This policy belongs to deploy-mcp orchestration. `RemoteExecutionPort` and `remote-exec-mcp` remain unchanged; they execute one requested capability call at a time and do not own deployment-level retry semantics.
