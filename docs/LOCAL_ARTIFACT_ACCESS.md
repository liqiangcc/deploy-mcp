# Local artifact access policy

## Purpose

`deploy-mcp` reads a caller-supplied local artifact before deployment so it can calculate the artifact size and SHA-256. That local filesystem read is a deploy-mcp security boundary and must not be left to an unrestricted `artifact_path`.

This policy is intentionally separate from `remote-exec-mcp` transfer policy:

```text
AI / MCP caller
  -> deploy-mcp artifact_path
  -> deploy-mcp local_artifacts.allowed_roots
  -> canonical local artifact path
  -> hash / size validation
  -> RemoteExecutionPort::upload_file
  -> remote-exec-mcp local transfer policy
  -> remote target
```

`deploy-mcp` protects its own local read. `remote-exec-mcp` independently protects the file it is willing to transfer. Neither component assumes the other component's policy replaces its own boundary.

## Configuration

```yaml
local_artifacts:
  allowed_roots:
    - /var/lib/deploy-mcp/artifacts
```

The configured roots are capability boundaries, not search paths. A deployment may reference only an artifact whose canonical filesystem path remains below one of these roots.

Configuration rules:

- at most 32 roots are accepted;
- every root must be non-empty and absolute;
- a filesystem root such as `/` is rejected;
- configured roots containing a `..` component are rejected;
- an omitted or empty `allowed_roots` list disables new local-artifact deployments rather than allowing unrestricted access.

The configured directory is resolved when a deployment needs local artifact access. If a configured root no longer exists, cannot be resolved, or is not a directory, the deployment fails closed with a configuration error.

## Runtime authorization

Before deploy-mcp opens or hashes artifact content:

1. each configured allowed root is canonicalized;
2. the requested artifact path is canonicalized;
3. the canonical artifact path must be contained by at least one canonical root;
4. the canonical artifact path is then used for artifact hashing and for `RemoteExecutionPort::upload_file`.

This prevents lexical traversal and symlink escapes from turning an apparently allowed path into a read outside the configured capability boundary.

Examples with `/srv/deploy/artifacts` as the only root:

```text
/srv/deploy/artifacts/demo.jar                  allowed
/srv/deploy/artifacts/releases/demo.jar         allowed
/srv/deploy/artifacts/../secrets/key            denied after canonicalization
/srv/deploy/artifacts/link -> /etc/passwd        denied after canonicalization
/etc/passwd                                     denied
```

Authorization failure uses the stable application error code:

```text
artifact_path_not_allowed
```

The failure occurs before artifact hashing, deployment reservation/state creation, or any `RemoteExecutionPort` call.

## Separation of concerns

The allowlist does **not** authorize or interpret:

- remote staging/install/backup paths;
- SSH or SFTP destinations;
- shell commands or argv;
- remote task names;
- credentials or SSH configuration.

Remote paths and transfer behavior remain governed by configured deployment semantics plus the independent `remote-exec-mcp` security boundary. `deploy-mcp` does not add SSH/SFTP implementation or raw-shell capability as part of local artifact authorization.

## Operational guidance

Prefer a dedicated artifact directory owned by the deployment workflow instead of broad roots such as a user's home directory. Build or download artifacts into that directory first, then pass the resulting artifact path to `deploy_application`.

For example:

```text
/var/lib/deploy-mcp/artifacts/
  demo-service/
    1.2.3/
      demo-service.jar
```

The operating-system account running deploy-mcp should have only the filesystem permissions it actually needs. The application allowlist is an additional capability boundary; it is not a replacement for OS permissions.
