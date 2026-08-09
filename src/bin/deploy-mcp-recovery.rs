use anyhow::{bail, Context, Result};
use deploy_mcp::application::{RecoveryAcknowledgementRequest, RecoveryAdminService};
use deploy_mcp::domain::RecoverySubjectKind;
use deploy_mcp::recovery_persistence::SqliteRecoveryRepository;
use serde_json::json;

fn main() -> Result<()> {
    let (database_path, command) = parse_args()?;
    let repository = SqliteRecoveryRepository::open(&database_path)
        .with_context(|| format!("failed to open recovery database at {database_path}"))?;
    let mut service = RecoveryAdminService::new(repository);

    match command {
        Command::List => {
            let incidents = service
                .unresolved_incidents()
                .context("failed to list unresolved recovery incidents")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({ "incidents": incidents }))?
            );
        }
        Command::Acknowledge(request) => {
            let record = service
                .acknowledge(request)
                .context("failed to acknowledge recovery incident")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({ "acknowledgement": record }))?
            );
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    List,
    Acknowledge(RecoveryAcknowledgementRequest),
}

fn parse_args() -> Result<(String, Command)> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from<I>(args: I) -> Result<(String, Command)>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let mut database_path =
        std::env::var("DEPLOY_MCP_DATABASE").unwrap_or_else(|_| "deployments.sqlite".to_owned());

    let first = args.next().context(usage())?;
    let command = if first == "--database" {
        database_path = args.next().context("--database requires a path argument")?;
        args.next().context(usage())?
    } else {
        first
    };

    match command.as_str() {
        "list" => {
            if let Some(extra) = args.next() {
                bail!("unexpected argument for list: {extra}");
            }
            Ok((database_path, Command::List))
        }
        "acknowledge" => Ok((
            database_path,
            Command::Acknowledge(parse_acknowledge_args(args)?),
        )),
        "--help" | "-h" | "help" => bail!(usage()),
        other => bail!("unknown command: {other}\n{}", usage()),
    }
}

fn parse_acknowledge_args<I>(args: I) -> Result<RecoveryAcknowledgementRequest>
where
    I: IntoIterator<Item = String>,
{
    let mut incident_id = None;
    let mut application = None;
    let mut environment = None;
    let mut subject_kind = None;
    let mut subject_id = None;
    let mut operator = None;
    let mut evidence = None;
    let mut args = args.into_iter();

    while let Some(flag) = args.next() {
        let value = args
            .next()
            .with_context(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--incident-id" => {
                incident_id = Some(
                    value
                        .parse::<u64>()
                        .context("--incident-id must be a positive integer")?,
                );
            }
            "--application" => application = Some(value),
            "--environment" => environment = Some(value),
            "--subject-kind" => subject_kind = Some(parse_subject_kind(&value)?),
            "--subject-id" => subject_id = Some(value),
            "--operator" => operator = Some(value),
            "--evidence" => evidence = Some(value),
            other => bail!("unexpected acknowledge argument: {other}"),
        }
    }

    Ok(RecoveryAcknowledgementRequest {
        incident_id: incident_id.context("--incident-id is required")?,
        application: application.context("--application is required")?,
        environment: environment.context("--environment is required")?,
        subject_kind: subject_kind.context("--subject-kind is required")?,
        subject_id: subject_id.context("--subject-id is required")?,
        operator: operator.context("--operator is required")?,
        evidence: evidence.context("--evidence is required")?,
    })
}

fn parse_subject_kind(value: &str) -> Result<RecoverySubjectKind> {
    match value {
        "deployment" => Ok(RecoverySubjectKind::Deployment),
        "rollback_operation" => Ok(RecoverySubjectKind::RollbackOperation),
        other => bail!("invalid --subject-kind {other}; expected deployment or rollback_operation"),
    }
}

fn usage() -> &'static str {
    "Usage:\n  deploy-mcp-recovery [--database PATH] list\n  deploy-mcp-recovery [--database PATH] acknowledge --incident-id ID --application APP --environment ENV --subject-kind deployment|rollback_operation --subject-id ID --operator NAME --evidence NOTE\nEnvironment: DEPLOY_MCP_DATABASE"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledge_requires_complete_incident_identity() {
        let error = parse_args_from(
            [
                "acknowledge",
                "--incident-id",
                "1",
                "--application",
                "demo",
                "--environment",
                "test",
                "--subject-kind",
                "deployment",
                "--operator",
                "alice",
                "--evidence",
                "verified service",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap_err();
        assert!(error.to_string().contains("--subject-id is required"));
    }

    #[test]
    fn list_rejects_extra_arguments() {
        let error =
            parse_args_from(["list", "unexpected"].into_iter().map(str::to_owned)).unwrap_err();
        assert!(error.to_string().contains("unexpected argument"));
    }
}
