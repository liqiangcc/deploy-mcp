# Deployment idempotency and artifact identity

## Purpose

A deployment retry must not accidentally launch a second remote mutation, and a published application version must not silently change its bytes.

`deploy-mcp` therefore treats request idempotency and artifact-version identity as two related but separate invariants:

```text
idempotency key
  -> identifies one deployment intent

(application, environment, version)
  -> identifies immutable artifact bytes
```

Neither invariant depends on SSH, remote-exec-mcp, or an AI agent remembering what it already requested. SQLite is the durable correctness boundary.

## MCP contract

`deploy_application` accepts an optional `idempotency_key` in addition to the existing deployment intent:

```json
{
  "application": "demo",
  "environment": "test",
  "version": "1.2.3",
  "artifact_path": "/local/demo.jar",
  "idempotency_key": "release-20260809-demo-123"
}
```

The key is optional for backward compatibility. When supplied it must:

- contain 1 through 128 bytes;
- have no leading or trailing whitespace;
- contain no control characters.

The key never controls a remote path, task, service name, credential, or shell fragment.

## Exact replay

Before a fresh deployment record is committed, deploy-mcp hashes the local artifact and obtains its size. The durable request identity is:

```text
application
+ environment
+ version
+ artifact SHA-256
+ artifact size
```

The SQLite `deployment_idempotency` table binds one global idempotency key to one deployment record.

If a later request uses the same key and the same durable request identity, the repository returns the original Deployment instead of inserting another one. The application response exposes:

```json
{
  "idempotent_replay": true
}
```

No remote-exec capability check, upload, backup, install, restart, verification, or rollback is repeated for that replay.

### In-flight policy

`deploy-mcp` does not join an already executing deployment call. The existing application/environment mutation lease remains authoritative while an orchestration is active.

Therefore a concurrent retry while the original deployment is still non-terminal may receive `conflicting_deployment`. The caller can query durable deployment state and retry the same idempotency key after the active mutation finishes. Once accepted as an idempotent replay, the original deployment record is returned and no second remote workflow is started.

This keeps two concerns separate:

- mutation lease: serialize live deployment work;
- idempotency binding: prevent a completed/accepted intent from being executed as a new deployment on retry.

## Key reuse with different intent

An idempotency key is immutable after it is bound.

Changing any durable identity field while reusing the key is rejected with:

```text
idempotency_conflict
```

A caller cannot deliberately or accidentally recycle a key for another application, environment, version, checksum, or artifact size.

## Immutable version identity

For one `(application, environment)`, a version string cannot later refer to different artifact bytes.

Before inserting a new deployment, SQLite checks all earlier deployment records for the same:

```text
application
+ environment
+ version
```

If the requested SHA-256 or size differs, the request fails with:

```text
artifact_version_conflict
```

The failure occurs before any remote work.

This rule applies even when the caller uses a different idempotency key. Idempotency keys identify requests; they do not authorize mutable version labels.

A later explicit redeployment of the same version and exactly the same checksum/size is allowed when no mutation guard is active. It is a new deployment when a new key (or no key) is supplied.

## SQLite transaction boundary

`SqliteDeploymentRepository::reserve` uses an immediate SQLite transaction.

Within one transaction it:

1. checks whether the idempotency key is already bound;
2. returns the existing deployment for an exact replay or rejects mismatched reuse;
3. verifies version/checksum/size immutability;
4. inserts the fresh deployment record;
5. binds the idempotency key to that deployment;
6. commits.

The transaction serializes competing writers sharing the same SQLite database. Process-local memory is not the correctness boundary.

Normal mutation constraints remain in force. Recovery incidents, active explicit rollback operations, and non-terminal deployment uniqueness can still reject a fresh reservation.

## Stable errors

The application/MCP error surface includes:

```text
idempotency_conflict
artifact_version_conflict
conflicting_deployment
```

They have different meanings:

- `idempotency_conflict`: one key was reused for a different intent;
- `artifact_version_conflict`: one application/environment/version was reused for different bytes;
- `conflicting_deployment`: another live mutation currently owns the environment.

## Safety evidence

The implementation has dedicated repository and application tests proving that:

- an exact key replay survives reopening SQLite and resolves to the original deployment;
- an exact replay after a completed deployment produces no additional remote calls;
- one key cannot be rebound to a different deployment intent;
- one application/environment/version cannot be rebound to a changed SHA-256;
- key/conflicting-version failures happen before additional remote work;
- MCP accepts the optional idempotency field while continuing to reject undeclared raw execution fields.
