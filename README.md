# deploy-mcp

A deployment-orchestration MCP for AI agents.

`deploy-mcp` owns **deployment semantics and lifecycle**. It does not own SSH, SFTP, credential handling, host-key verification, or arbitrary remote command execution. Those capabilities are delegated through a narrow `RemoteExecutionPort`, with `remote-exec-mcp` as the first adapter.

## Core boundary

```text
AI Agent
   |
   v
deploy-mcp
   |
   +-- Deployment domain/state machine
   +-- Deployment planning
   +-- Verification / rollback
   +-- Deployment history
   |
   v
RemoteExecutionPort
   |
   v
remote-exec-mcp
   |
   +-- policy / validation / audit
   +-- SSH command execution
   +-- SFTP file transfer
   v
Remote Linux host
```

> `remote-exec-mcp` answers **"how can this operation be executed safely?"**  
> `deploy-mcp` answers **"what does a correct deployment mean, and what happens when a step fails?"**

## v0.1 target

The first release intentionally supports one production-shaped workflow only:

- Java/Spring Boot JAR artifact;
- remote Linux host;
- service managed by systemd;
- staged upload;
- previous-version backup;
- artifact installation;
- service restart;
- deterministic health verification;
- automatic rollback after a post-mutation failure;
- durable deployment status/history.

Docker, Kubernetes, Helm, build pipelines, log querying, and configuration management are explicitly outside the v0.1 boundary.

## Development baseline

The Rust project keeps protocol, application, domain, ports, infrastructure adapters, configuration, and persistence boundaries explicit from the start. The initial configuration shape is documented in [`config/example.yaml`](config/example.yaml).

CI requires:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

See [DESIGN.md](DESIGN.md) and [ROADMAP.md](ROADMAP.md).
