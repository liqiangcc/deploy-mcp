use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use deploy_mcp::adapters::RemoteExecMcpAdapter;
use deploy_mcp::application::{DeploymentApi, DeploymentApplication};
use deploy_mcp::config::Config;
use deploy_mcp::mcp::DeployMcp;
use deploy_mcp::persistence::SqliteDeploymentRepository;
use deploy_mcp::ports::RollbackRepository;
use deploy_mcp::rollback_persistence::SqliteRollbackRepository;
use rmcp::{transport::stdio, ServiceExt};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let (config_path, database_path) = startup_paths()?;
    let config = Arc::new(
        Config::load(&config_path)
            .with_context(|| format!("failed to load deploy-mcp config from {config_path}"))?,
    );
    let repository = Arc::new(Mutex::new(
        SqliteDeploymentRepository::open(&database_path)
            .with_context(|| format!("failed to open deployment database at {database_path}"))?,
    ));
    let rollback_repository: Arc<Mutex<Box<dyn RollbackRepository + Send>>> =
        Arc::new(Mutex::new(Box::new(
            SqliteRollbackRepository::open(&database_path)
                .with_context(|| format!("failed to open rollback database at {database_path}"))?,
        )));
    let remote = Arc::new(
        RemoteExecMcpAdapter::spawn_from_config(&config.remote_exec)
            .await
            .context("failed to start remote-exec-mcp child process")?,
    );
    let application: Arc<dyn DeploymentApi> = Arc::new(DeploymentApplication::new(
        Arc::clone(&config),
        remote,
        repository,
        rollback_repository,
    ));

    let service = DeployMcp::new(application).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

fn startup_paths() -> Result<(String, String)> {
    let mut config_path =
        std::env::var("DEPLOY_MCP_CONFIG").unwrap_or_else(|_| "config/example.yaml".to_owned());
    let mut database_path =
        std::env::var("DEPLOY_MCP_DATABASE").unwrap_or_else(|_| "deployments.sqlite".to_owned());

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--config" => {
                config_path = args.next().context("--config requires a path argument")?;
            }
            "--database" => {
                database_path = args.next().context("--database requires a path argument")?;
            }
            "--help" | "-h" => {
                eprintln!(
                    "Usage: deploy-mcp [--config PATH] [--database PATH]\nEnvironment: DEPLOY_MCP_CONFIG, DEPLOY_MCP_DATABASE"
                );
                std::process::exit(0);
            }
            other => bail!("unexpected argument: {other}; use --config PATH --database PATH"),
        }
    }

    Ok((config_path, database_path))
}
