# deploy-mcp v0.2 — Deployment Mechanism Abstraction and Docker Compose

## 1. Status

This document defines the proposed v0.2 architecture.

v0.2 has two goals:

1. extract the deployment-mechanism boundary that is currently implicit in the JAR/systemd workflow;
2. prove that boundary with a second production-shaped mechanism: single-host Docker Compose deployment.

This is an architecture expansion, not a rewrite of the v0.1 deployment lifecycle.

The existing v0.1 guarantees remain authoritative:

- deterministic deployment state;
- durable SQLite history;
- idempotency and mutation exclusion;
- fail-closed timeout/crash handling;
- deterministic verification;
- automatic and explicit rollback;
- startup recovery and operator reconciliation;
- narrow MCP intent surface;
- no unrestricted remote execution in deploy-mcp.

## 2. Problem

v0.1 deliberately optimized for one deployment mechanism:

```text
local JAR
  -> remote staging
  -> backup current JAR
  -> install
  -> systemd restart
  -> health verification
```

The architecture already separates deployment semantics from SSH/SFTP transport, but several current concepts still carry JAR/systemd assumptions:

- local file artifacts;
- staging/install/backup paths;
- install/restart task semantics;
- rollback references containing JAR-oriented capability snapshots.

Adding Docker, Kubernetes, Helm, or multi-host deployment directly to those structures would gradually produce mechanism-specific branches throughout the deployment application.

v0.2 must prevent this.

The target architecture is:

```text
MCP
 |
 v
Deployment Application
 |
 +-- state machine
 +-- persistence
 +-- idempotency
 +-- timeout/recovery
 +-- audit/history
 +-- rollback policy
 |
 v
DeploymentMechanismPort
 |
 +----------------------+----------------------+
 |                                             |
 v                                             v
JarSystemdMechanism                    DockerComposeMechanism
 |                                             |
 +----------------------+----------------------+
                        |
                        v
               RemoteExecutionPort
                        |
                        v
                remote-exec-mcp
```

The lifecycle owns deployment correctness.

The mechanism owns how one configured deployment technology performs an approved lifecycle operation.

The transport continues to own safe remote execution.

## 3. Separation of concerns

### Deployment application owns

The generic application layer continues to own:

- deployment identity;
- configured application/environment lookup;
- mutation exclusion;
- deployment reservation;
- state transitions;
- durable step attempts;
- idempotency;
- timeout interpretation;
- crash ambiguity;
- retry policy;
- automatic rollback decision;
- explicit rollback lifecycle;
- recovery incidents;
- audit/history projection;
- MCP-facing outcomes.

A mechanism implementation MUST NOT directly:

- write Deployment state;
- write RollbackOperation state;
- clear recovery incidents;
- decide that a timed-out mutation completed;
- expose new MCP tools;
- accept arbitrary shell commands;
- bypass mutation guards.

### DeploymentMechanismPort owns

The mechanism boundary owns translation from generic lifecycle intent to mechanism-specific approved capabilities.

Examples:

```text
prepare candidate
capture rollback point
apply candidate
activate candidate
verify candidate
restore rollback point
```

The mechanism may compose `RemoteExecutionPort`, but it does not own SSH/SFTP.

### RemoteExecutionPort owns

The existing boundary remains unchanged:

```text
check_target
list_tasks
upload_file
run_task
```

`remote-exec-mcp` continues to own:

- SSH authentication;
- host-key verification;
- SFTP;
- named-task authorization;
- task parameter validation;
- command construction;
- remote transfer boundaries;
- low-level remote audit.

## 4. DeploymentMechanismPort

The application-owned mechanism port should expose semantic deployment capabilities, not commands.

Conceptually:

```text
DeploymentMechanismPort

resolve_release_identity(...)
precheck(...)
prepare(...)
capture_rollback(...)
apply(...)
activate(...)
verify(...)
rollback(...)
```

Each operation accepts application-owned typed context.

No operation accepts:

- raw shell;
- argv supplied by the MCP caller;
- arbitrary remote paths;
- arbitrary Docker Compose project names;
- arbitrary systemd service names;
- SSH credentials;
- registry credentials.

The application selects the mechanism from trusted application configuration.

The MCP caller never supplies a mechanism name.

## 5. Generic lifecycle

v0.2 should introduce application-level semantic phases:

```text
VALIDATE
PRECHECK
PREPARE
CAPTURE_ROLLBACK
APPLY
ACTIVATE
VERIFY
```

The first possible live mutation remains `APPLY`.

Therefore the generic rule remains:

```text
failure before APPLY
  -> FAILED
  -> no automatic rollback
```

and:

```text
completed failure from APPLY onward
  -> automatic rollback when a valid rollback point exists
```

A timeout during or after possible live mutation remains ambiguous:

```text
APPLY / ACTIVATE / VERIFY timeout
  -> do not guess remote completion
  -> do not immediately claim rollback is safe
  -> retain fail-closed recovery boundary
```

Existing v0.1 durable state names do not need an unsafe destructive migration merely to rename terminology.

During v0.2, the generic application phase can map to the existing durable states where necessary:

```text
PRECHECK           -> PRECHECKING
PREPARE            -> STAGING_ARTIFACT
CAPTURE_ROLLBACK   -> BACKING_UP
APPLY              -> INSTALLING
ACTIVATE           -> RESTARTING
VERIFY             -> VERIFYING
```

This preserves database and MCP compatibility while removing JAR assumptions from new application orchestration code.

A future schema version may rename persisted states only if there is a concrete compatibility benefit.

## 6. Release identity

The current local-file Artifact model is insufficient for container deployment.

v0.2 should distinguish deployment release identity from mechanism-specific source material.

Conceptually:

```text
ReleaseIdentity
  |
  +-- JarRelease
  |     version
  |     sha256
  |     size_bytes
  |
  +-- ContainerRelease
        version
        configured_repository
        immutable_digest
```

Docker deployment MUST use an immutable digest.

A mutable image tag alone is not sufficient deployment identity.

For example:

```text
registry.example.com/demo@sha256:...
```

The image repository belongs to trusted application configuration.

The MCP caller supplies only the release version and immutable digest required by the configured Docker mechanism.

Registry credentials remain outside deploy-mcp.

## 7. MCP compatibility

The caller must not choose deployment technology.

Existing applications continue using the JAR input contract.

For container applications, the request should use a narrow typed release source rather than Docker command parameters.

Conceptually:

```json
{
  "application": "demo-service",
  "environment": "test",
  "version": "1.3.0",
  "release": {
    "type": "container_image",
    "digest": "sha256:..."
  },
  "idempotency_key": "demo-service-test-1.3.0"
}
```

For JAR deployments:

```json
{
  "application": "legacy-service",
  "environment": "test",
  "version": "1.3.0",
  "release": {
    "type": "local_file",
    "path": "/approved/artifacts/service.jar"
  }
}
```

Migration must preserve compatibility with the existing `artifact_path` request before considering removal or deprecation.

Unknown or mechanism-incompatible release fields fail before remote work.

## 8. Trusted mechanism configuration

An environment selects exactly one mechanism.

Example concept:

```yaml
applications:
  demo-service:
    environments:
      test:
        mechanism:
          type: docker_compose
          target: test-server
          image_repository: registry.example.com/demo-service
          compose_project: demo
          service: app

          tasks:
            precheck: demo-compose-precheck
            prepare: demo-compose-prepare
            capture_rollback: demo-compose-current
            apply: demo-compose-apply
            activate: demo-compose-up
            health_check: demo-compose-health
            rollback: demo-compose-rollback
```

The exact project path, service identity, task names and image repository are trusted configuration.

The AI-facing request cannot replace them.

## 9. Docker Compose v0.2 scope

The Docker mechanism deliberately supports only:

- one configured Linux target;
- one configured Docker Compose project;
- one managed service;
- one configured image repository;
- immutable image digest deployment;
- deterministic health verification;
- one previous-release rollback point;
- explicit deployment-bound rollback.

It does not include:

- Kubernetes;
- Docker Swarm;
- multiple hosts;
- rolling deployment;
- blue/green;
- canary;
- traffic shifting;
- registry credential management;
- arbitrary compose YAML editing;
- arbitrary Docker command execution;
- arbitrary service selection by the caller.

## 10. Docker Compose mechanism flow

Success:

```text
validate immutable digest
  -> PRECHECK
  -> prepare/pull candidate image
  -> capture current deployed digest
  -> APPLY configured candidate digest
  -> ACTIVATE configured Compose service
  -> deterministic health verification
  -> SUCCEEDED
```

The mechanism-specific operations are performed through approved `remote-exec-mcp` named tasks.

deploy-mcp never constructs raw Docker, Docker Compose, `sed`, or `sh -c` commands from MCP input.

## 11. Rollback

Docker rollback must remain deployment-bound exactly as JAR rollback is today.

The rollback reference should evolve into a typed mechanism snapshot:

```text
RollbackReference
- deployment_id
- application
- environment
- mechanism_kind
- target
- contract_fingerprint
- mechanism_snapshot
- lifecycle_state
```

Conceptual mechanism snapshots:

```text
JarSystemdRollbackSnapshot
- existing JAR/systemd capability data

DockerComposeRollbackSnapshot
- previous immutable image digest
- configured capability identity needed for restore
```

Normal MCP history must not expose internal task names, remote paths, credentials or trusted rollback payload details.

Before explicit rollback:

1. verify the reference is active;
2. verify the source deployment still owns rollback authority;
3. verify no newer deployment invalidated it;
4. verify mechanism kind matches;
5. verify current trusted mechanism contract matches the stored fingerprint;
6. acquire mutation exclusion;
7. preflight capabilities;
8. execute rollback;
9. activate;
10. verify;
11. consume the reference only after success.

## 12. Configuration drift

Comparing individual JAR paths and Docker fields throughout the rollback service would recreate mechanism-specific branches.

v0.2 should therefore introduce a mechanism-owned deterministic configuration fingerprint.

For example:

```text
mechanism_contract_fingerprint =
  SHA-256(canonical security-relevant mechanism configuration)
```

The fingerprint covers fields that define deployment authority.

For Docker this includes, at minimum:

- target;
- configured repository;
- compose project identity;
- service identity;
- named task identities.

A changed contract invalidates historical automatic assumptions before remote mutation.

## 13. Idempotency

The v0.1 semantics remain unchanged.

The durable deployment intent becomes:

```text
application
environment
mechanism
version
release identity
```

For JAR:

```text
version + sha256 + size
```

For Docker:

```text
version + repository + immutable digest
```

Reusing one idempotency key with a different digest fails before remote work.

The same version mapped to different immutable content must remain a stable conflict.

## 14. Timeout and crash recovery

Mechanism implementations return only completed operation outcomes.

The application layer interprets timeout.

This distinction is critical.

If Docker `apply` times out, deploy-mcp must not ask the mechanism to “check and guess” whether it completed and then automatically continue.

The existing rule remains:

```text
possible mutation + unknown completion
  -> fail closed
  -> durable recovery incident
  -> operator reconciliation before later mutation
```

This rule is mechanism-independent.

## 15. Audit

The existing audit separation remains:

```text
deploy-mcp history
= why a deployment lifecycle operation happened

remote-exec audit
= which approved remote capability was executed
```

v0.2 adds mechanism identity and stable mechanism-step codes to deployment history.

It does not duplicate remote-exec stdout/stderr or command details into normal AI-facing history.

## 16. JarSystemd migration

Before Docker implementation, the current JAR workflow should be moved behind the new mechanism boundary without changing externally observable behavior.

This is the primary architecture test.

Required regression:

```text
existing v0.1 request
  -> same durable states
  -> same rollback behavior
  -> same idempotency behavior
  -> same timeout/recovery behavior
  -> same MCP disclosure boundary
  -> same real systemd E2E result
```

If extracting `JarSystemdMechanism` changes v0.1 semantics, the abstraction is incorrect.

## 17. Security invariants

v0.2 must retain all v0.1 threat-model constraints and add:

- MCP callers cannot select the deployment mechanism;
- MCP callers cannot select Docker target/project/service/repository;
- image references must use the configured repository and immutable digest;
- no raw Docker/Compose command reaches the MCP surface;
- no registry credential reaches deployment DTOs;
- mechanism adapters cannot directly change durable deployment state;
- mechanism adapters cannot bypass recovery/mutation guards;
- rollback payloads remain trusted/internal and redacted from AI-facing history;
- remote-exec independently authorizes every named task and typed parameter.

## 18. Testing strategy

### Architecture tests

Prove that:

- generic deployment orchestration imports no Docker/systemd transport implementation;
- MCP DTOs contain no target/task/command/credential control;
- mechanism selection comes only from trusted configuration.

### JAR regression

All current JAR unit, persistence, timeout, recovery and real-systemd tests remain green after extraction.

### Docker deterministic tests

Use a fake mechanism/remote port to prove:

- success;
- pre-mutation failure;
- apply failure;
- activation failure;
- verification exhaustion;
- automatic rollback;
- rollback failure;
- timeout ambiguity;
- idempotency;
- configuration drift;
- explicit rollback.

### Real Docker E2E

Add a Linux CI acceptance test:

```text
MCP client
  -> deploy-mcp
  -> DockerComposeMechanism
  -> real remote-exec-mcp
  -> SSH
  -> Docker Compose
  -> deploy immutable image digest
  -> health verification
  -> explicit rollback
```

The real E2E must prove both the new mechanism and the unchanged security boundary.

## 19. Implementation order

v0.2 implementation should proceed in this order.

### Phase A — mechanism architecture

- introduce mechanism kind/config model;
- introduce `DeploymentMechanismPort`;
- introduce generic lifecycle operation vocabulary;
- introduce generic release identity;
- generalize rollback capability snapshot/fingerprint;
- move JAR/systemd behind `JarSystemdMechanism`;
- preserve all v0.1 behavior.

### Phase B — architecture regression

- run all existing tests;
- run real systemd E2E;
- prove no behavioral regression.

### Phase C — Docker Compose mechanism

- add Docker Compose trusted configuration;
- add container release identity;
- implement `DockerComposeMechanism` through `RemoteExecutionPort`;
- add deterministic rollback;
- add security regression tests.

### Phase D — real Docker acceptance

- disposable Docker Compose environment;
- real remote-exec-mcp;
- immutable-digest deployment;
- real health check;
- explicit rollback;
- audit/redaction assertions.

## 20. v0.2 completion boundary

v0.2 is complete when:

1. JAR/systemd and Docker Compose use one generic deployment application lifecycle;
2. adding Docker does not introduce Docker branches throughout domain/persistence/MCP orchestration;
3. current JAR/systemd behavior remains backward compatible;
4. Docker deployment uses immutable image identity;
5. Docker target/project/service/repository authority comes only from trusted configuration;
6. both mechanisms preserve deterministic verification and rollback;
7. timeout/crash ambiguity remains fail-closed;
8. existing durable recovery/idempotency/audit boundaries work for both mechanisms;
9. real JAR/systemd E2E remains green;
10. a real Docker Compose E2E proves deploy + rollback through real remote-exec-mcp.

Only after that boundary is stable should Kubernetes, multi-host rollout, blue/green, or canary work begin.
