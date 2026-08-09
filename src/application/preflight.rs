use std::collections::BTreeSet;

use thiserror::Error;

use crate::config::EnvironmentConfig;
use crate::ports::{RemoteExecutionError, RemoteExecutionPort};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePreflightReport {
    pub target: String,
    pub remote_identity: Option<String>,
    pub available_tasks: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RemotePreflightError {
    #[error(transparent)]
    Remote(#[from] RemoteExecutionError),
    #[error("configured target is not reachable: {0}")]
    TargetUnreachable(String),
    #[error("target {target} is missing required remote capabilities: {tasks:?}")]
    MissingCapabilities { target: String, tasks: Vec<String> },
}

pub async fn preflight_remote_capabilities<R>(
    remote: &R,
    environment: &EnvironmentConfig,
) -> Result<RemotePreflightReport, RemotePreflightError>
where
    R: RemoteExecutionPort + ?Sized,
{
    let check = remote.check_target(&environment.target).await?;
    if !check.reachable {
        return Err(RemotePreflightError::TargetUnreachable(
            environment.target.clone(),
        ));
    }

    let available_tasks = remote.list_tasks(&environment.target).await?;
    let required_tasks = required_tasks(environment);
    let missing = required_tasks
        .difference(&available_tasks)
        .cloned()
        .collect::<Vec<_>>();

    if !missing.is_empty() {
        return Err(RemotePreflightError::MissingCapabilities {
            target: environment.target.clone(),
            tasks: missing,
        });
    }

    Ok(RemotePreflightReport {
        target: environment.target.clone(),
        remote_identity: check.remote_identity,
        available_tasks,
    })
}

fn required_tasks(environment: &EnvironmentConfig) -> BTreeSet<String> {
    let mut tasks = BTreeSet::from([
        environment.tasks.backup.clone(),
        environment.tasks.install.clone(),
        environment.tasks.restart.clone(),
        environment.tasks.health_check.clone(),
    ]);
    if let Some(precheck) = &environment.tasks.precheck {
        tasks.insert(precheck.clone());
    }
    if let Some(rollback) = &environment.tasks.rollback {
        tasks.insert(rollback.clone());
    }
    tasks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{FakeRemoteCall, FakeRemoteExecution};
    use crate::config::Config;
    use crate::ports::RemoteTargetCheck;

    const CONFIG: &str = r#"
remote_exec:
  command: remote-exec-mcp
applications:
  demo:
    artifact_type: jar
    environments:
      test:
        target: test-server
        staging_path: /opt/staging/demo.jar
        install_path: /opt/apps/demo/demo.jar
        backup_path: /opt/apps/demo/backup/demo.jar
        tasks:
          precheck: demo-precheck
          backup: demo-backup
          install: demo-install
          restart: demo-restart
          health_check: demo-health
          rollback: demo-rollback
"#;

    fn environment() -> EnvironmentConfig {
        Config::from_yaml(CONFIG)
            .unwrap()
            .environment("demo", "test")
            .unwrap()
            .clone()
    }

    fn all_tasks() -> BTreeSet<String> {
        [
            "demo-precheck",
            "demo-backup",
            "demo-install",
            "demo-restart",
            "demo-health",
            "demo-rollback",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    #[tokio::test]
    async fn preflight_checks_connectivity_then_authorized_tasks() {
        let fake = FakeRemoteExecution::default();
        fake.set_target_check(
            "test-server",
            Ok(crate::ports::RemoteTargetCheck {
                reachable: true,
                remote_identity: Some("test-identity".to_owned()),
            }),
        );
        fake.set_tasks("test-server", Ok(all_tasks()));

        let report = preflight_remote_capabilities(&fake, &environment())
            .await
            .unwrap();
        assert_eq!(report.target, "test-server");
        assert_eq!(report.remote_identity.as_deref(), Some("test-identity"));
        assert_eq!(
            fake.calls(),
            vec![
                FakeRemoteCall::CheckTarget {
                    target: "test-server".to_owned()
                },
                FakeRemoteCall::ListTasks {
                    target: "test-server".to_owned()
                }
            ]
        );
    }

    #[tokio::test]
    async fn preflight_rejects_missing_configured_capability() {
        let fake = FakeRemoteExecution::default();
        fake.set_target_check(
            "test-server",
            Ok(crate::ports::RemoteTargetCheck {
                reachable: true,
                remote_identity: None,
            }),
        );
        let mut tasks = all_tasks();
        tasks.remove("demo-health");
        fake.set_tasks("test-server", Ok(tasks));

        let error = preflight_remote_capabilities(&fake, &environment())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            RemotePreflightError::MissingCapabilities {
                target: "test-server".to_owned(),
                tasks: vec!["demo-health".to_owned()]
            }
        );
    }

    #[tokio::test]
    async fn unreachable_target_fails_before_task_discovery() {
        let fake = FakeRemoteExecution::default();
        fake.set_target_check(
            "test-server",
            Ok(RemoteTargetCheck {
                reachable: false,
                remote_identity: None,
            }),
        );

        let error = preflight_remote_capabilities(&fake, &environment())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            RemotePreflightError::TargetUnreachable("test-server".to_owned())
        );
        assert_eq!(fake.calls().len(), 1);
    }
}
