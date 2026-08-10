use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path};

use serde::Deserialize;

use crate::domain::DeploymentMechanismKind;
use crate::error::{AppError, AppResult, ErrorCode};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub remote_exec: RemoteExecConfig,
    #[serde(default)]
    pub local_artifacts: LocalArtifactConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    pub applications: BTreeMap<String, ApplicationConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteExecConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalArtifactConfig {
    #[serde(default)]
    pub allowed_roots: Vec<String>,
}

const MAX_TIMEOUT_MS: u64 = 3_600_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    #[serde(default = "default_deployment_step_timeout_ms")]
    pub deployment_step_timeout_ms: u64,
    #[serde(default = "default_explicit_rollback_timeout_ms")]
    pub explicit_rollback_timeout_ms: u64,
    #[serde(default = "default_verification_max_attempts")]
    pub verification_max_attempts: u32,
    #[serde(default = "default_verification_retry_delay_ms")]
    pub verification_retry_delay_ms: u64,
    #[serde(default = "default_rollback_reference_retention_days")]
    pub rollback_reference_retention_days: u32,
    #[serde(default = "default_rollback_reference_cleanup_batch_size")]
    pub rollback_reference_cleanup_batch_size: u32,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            deployment_step_timeout_ms: default_deployment_step_timeout_ms(),
            explicit_rollback_timeout_ms: default_explicit_rollback_timeout_ms(),
            verification_max_attempts: default_verification_max_attempts(),
            verification_retry_delay_ms: default_verification_retry_delay_ms(),
            rollback_reference_retention_days: default_rollback_reference_retention_days(),
            rollback_reference_cleanup_batch_size: default_rollback_reference_cleanup_batch_size(),
        }
    }
}

const fn default_deployment_step_timeout_ms() -> u64 {
    120_000
}

const fn default_explicit_rollback_timeout_ms() -> u64 {
    300_000
}

const fn default_verification_max_attempts() -> u32 {
    3
}

const fn default_verification_retry_delay_ms() -> u64 {
    1_000
}

const fn default_rollback_reference_retention_days() -> u32 {
    30
}

const fn default_rollback_reference_cleanup_batch_size() -> u32 {
    500
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationConfig {
    pub display_name: Option<String>,
    pub artifact_type: ArtifactType,
    pub environments: BTreeMap<String, EnvironmentConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactType {
    Jar,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MechanismConfig {
    #[serde(rename = "type")]
    pub kind: DeploymentMechanismKind,
}

impl Default for MechanismConfig {
    fn default() -> Self {
        Self {
            kind: DeploymentMechanismKind::JarSystemd,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentConfig {
    #[serde(default)]
    pub mechanism: MechanismConfig,
    pub target: String,
    pub staging_path: String,
    pub install_path: String,
    pub backup_path: String,
    pub tasks: TaskReferences,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReferences {
    pub precheck: Option<String>,
    pub backup: String,
    pub install: String,
    pub restart: String,
    pub health_check: String,
    pub rollback: Option<String>,
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> AppResult<Self> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path).map_err(|error| {
            AppError::invalid_configuration(format!(
                "failed to read config {}: {error}",
                path.display()
            ))
        })?;
        Self::from_yaml(&raw)
    }

    pub fn from_yaml(raw: &str) -> AppResult<Self> {
        let config: Self = serde_yaml::from_str(raw).map_err(|error| {
            AppError::invalid_configuration(format!("failed to parse config: {error}"))
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> AppResult<()> {
        validate_reference("remote_exec.command", &self.remote_exec.command)?;
        validate_process_value("remote_exec.command", &self.remote_exec.command)?;
        for (index, argument) in self.remote_exec.args.iter().enumerate() {
            validate_process_value(&format!("remote_exec.args[{index}]"), argument)?;
        }
        validate_local_artifact_roots(&self.local_artifacts.allowed_roots)?;
        validate_timeout(
            "runtime.deployment_step_timeout_ms",
            self.runtime.deployment_step_timeout_ms,
        )?;
        validate_timeout(
            "runtime.explicit_rollback_timeout_ms",
            self.runtime.explicit_rollback_timeout_ms,
        )?;
        validate_verification_attempts(self.runtime.verification_max_attempts)?;
        validate_verification_retry_delay(self.runtime.verification_retry_delay_ms)?;
        validate_rollback_reference_retention_days(self.runtime.rollback_reference_retention_days)?;
        validate_rollback_reference_cleanup_batch_size(
            self.runtime.rollback_reference_cleanup_batch_size,
        )?;

        if self.applications.is_empty() {
            return Err(AppError::invalid_configuration(
                "at least one application must be configured",
            ));
        }

        for (application_id, application) in &self.applications {
            validate_reference("application id", application_id)?;
            if application.environments.is_empty() {
                return Err(AppError::invalid_configuration(format!(
                    "application {application_id} must define at least one environment"
                )));
            }

            for (environment_id, environment) in &application.environments {
                validate_reference("environment id", environment_id)?;
                if environment.mechanism.kind != DeploymentMechanismKind::JarSystemd {
                    return Err(AppError::invalid_configuration(format!(
                        "deployment mechanism {} is not implemented in phase 8",
                        environment.mechanism.kind.as_str()
                    )));
                }
                validate_reference("target", &environment.target)?;
                validate_absolute_path("staging_path", &environment.staging_path)?;
                validate_absolute_path("install_path", &environment.install_path)?;
                validate_absolute_path("backup_path", &environment.backup_path)?;
                validate_optional_reference(
                    "precheck task",
                    environment.tasks.precheck.as_deref(),
                )?;
                validate_reference("backup task", &environment.tasks.backup)?;
                validate_reference("install task", &environment.tasks.install)?;
                validate_reference("restart task", &environment.tasks.restart)?;
                validate_reference("health_check task", &environment.tasks.health_check)?;
                validate_optional_reference(
                    "rollback task",
                    environment.tasks.rollback.as_deref(),
                )?;
            }
        }

        Ok(())
    }

    pub fn application(&self, application: &str) -> AppResult<&ApplicationConfig> {
        self.applications.get(application).ok_or_else(|| {
            AppError::new(
                ErrorCode::UnknownApplication,
                format!("unknown application: {application}"),
            )
        })
    }

    pub fn environment(
        &self,
        application: &str,
        environment: &str,
    ) -> AppResult<&EnvironmentConfig> {
        self.application(application)?
            .environments
            .get(environment)
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::UnknownEnvironment,
                    format!("unknown environment {environment} for application {application}"),
                )
            })
    }
}

fn validate_reference(kind: &str, value: &str) -> AppResult<()> {
    if value.trim().is_empty() {
        return Err(AppError::invalid_configuration(format!(
            "{kind} must not be empty"
        )));
    }
    Ok(())
}

fn validate_optional_reference(kind: &str, value: Option<&str>) -> AppResult<()> {
    if let Some(value) = value {
        validate_reference(kind, value)?;
    }
    Ok(())
}

fn validate_absolute_path(kind: &str, value: &str) -> AppResult<()> {
    if !Path::new(value).is_absolute() {
        return Err(AppError::invalid_configuration(format!(
            "{kind} must be absolute: {value}"
        )));
    }
    Ok(())
}

fn validate_local_artifact_roots(roots: &[String]) -> AppResult<()> {
    const MAX_ROOTS: usize = 32;
    if roots.len() > MAX_ROOTS {
        return Err(AppError::invalid_configuration(format!(
            "local_artifacts.allowed_roots must contain at most {MAX_ROOTS} entries"
        )));
    }
    for root in roots {
        validate_reference("local artifact allowed root", root)?;
        let path = Path::new(root);
        if !path.is_absolute() {
            return Err(AppError::invalid_configuration(format!(
                "local artifact allowed root must be absolute: {root}"
            )));
        }
        if path.parent().is_none() {
            return Err(AppError::invalid_configuration(format!(
                "local artifact allowed root must not be a filesystem root: {root}"
            )));
        }
        if path
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(AppError::invalid_configuration(format!(
                "local artifact allowed root must not contain '..': {root}"
            )));
        }
    }
    Ok(())
}

fn validate_process_value(kind: &str, value: &str) -> AppResult<()> {
    if value.contains('\0') {
        return Err(AppError::invalid_configuration(format!(
            "{kind} must not contain NUL"
        )));
    }
    Ok(())
}

fn validate_timeout(kind: &str, value: u64) -> AppResult<()> {
    if value == 0 || value > MAX_TIMEOUT_MS {
        return Err(AppError::invalid_configuration(format!(
            "{kind} must be between 1 and {MAX_TIMEOUT_MS} milliseconds"
        )));
    }
    Ok(())
}

fn validate_verification_attempts(value: u32) -> AppResult<()> {
    const MAX_ATTEMPTS: u32 = 10;
    if value == 0 || value > MAX_ATTEMPTS {
        return Err(AppError::invalid_configuration(format!(
            "runtime.verification_max_attempts must be between 1 and {MAX_ATTEMPTS}"
        )));
    }
    Ok(())
}

fn validate_verification_retry_delay(value: u64) -> AppResult<()> {
    const MAX_DELAY_MS: u64 = 60_000;
    if value > MAX_DELAY_MS {
        return Err(AppError::invalid_configuration(format!(
            "runtime.verification_retry_delay_ms must be between 0 and {MAX_DELAY_MS} milliseconds"
        )));
    }
    Ok(())
}

fn validate_rollback_reference_retention_days(value: u32) -> AppResult<()> {
    const MAX_RETENTION_DAYS: u32 = 3_650;
    if value == 0 || value > MAX_RETENTION_DAYS {
        return Err(AppError::invalid_configuration(format!(
            "runtime.rollback_reference_retention_days must be between 1 and {MAX_RETENTION_DAYS} days"
        )));
    }
    Ok(())
}

fn validate_rollback_reference_cleanup_batch_size(value: u32) -> AppResult<()> {
    const MAX_BATCH_SIZE: u32 = 5_000;
    if value == 0 || value > MAX_BATCH_SIZE {
        return Err(AppError::invalid_configuration(format!(
            "runtime.rollback_reference_cleanup_batch_size must be between 1 and {MAX_BATCH_SIZE}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_CONFIG: &str = r#"
remote_exec:
  command: remote-exec-mcp
  args:
    - --config
    - /etc/remote-exec/config.yaml
local_artifacts:
  allowed_roots:
    - /var/lib/deploy-mcp/artifacts
applications:
  demo-service:
    display_name: Demo Service
    artifact_type: jar
    environments:
      test:
        target: test-server
        staging_path: /opt/staging/demo-service.jar
        install_path: /opt/apps/demo-service/demo-service.jar
        backup_path: /opt/apps/demo-service/backup/demo-service.jar
        tasks:
          precheck: demo-precheck
          backup: demo-backup
          install: demo-install
          restart: demo-restart
          health_check: demo-health
          rollback: demo-rollback
"#;

    #[test]
    fn parses_and_validates_declarative_application_config() {
        let config = Config::from_yaml(VALID_CONFIG).unwrap();
        let environment = config.environment("demo-service", "test").unwrap();
        assert_eq!(config.remote_exec.command, "remote-exec-mcp");
        assert_eq!(
            config.local_artifacts.allowed_roots,
            vec!["/var/lib/deploy-mcp/artifacts"]
        );
        assert_eq!(environment.target, "test-server");
        assert_eq!(
            environment.mechanism.kind,
            DeploymentMechanismKind::JarSystemd
        );
        assert_eq!(environment.tasks.restart, "demo-restart");
        assert_eq!(
            config.application("demo-service").unwrap().artifact_type,
            ArtifactType::Jar
        );
        assert_eq!(config.runtime.rollback_reference_retention_days, 30);
        assert_eq!(config.runtime.rollback_reference_cleanup_batch_size, 500);
    }

    #[test]
    fn rejects_unknown_configuration_fields_at_every_capability_boundary() {
        let cases = [
            VALID_CONFIG.replace("remote_exec:", "unexpected_top_level: true\nremote_exec:"),
            VALID_CONFIG.replace(
                "  command: remote-exec-mcp",
                "  command: remote-exec-mcp\n  unexpected_remote_exec: true",
            ),
            VALID_CONFIG.replace(
                "local_artifacts:",
                "local_artifacts:\n  unexpected_local_artifact: true",
            ),
            VALID_CONFIG.replace(
                "applications:",
                "runtime:\n  deployment_step_timout_ms: 120000\napplications:",
            ),
            VALID_CONFIG.replace(
                "    artifact_type: jar",
                "    artifact_type: jar\n    unexpected_application: true",
            ),
            VALID_CONFIG.replace(
                "        target: test-server",
                "        target: test-server\n        unexpected_environment: true",
            ),
            VALID_CONFIG.replace(
                "          restart: demo-restart",
                "          restart: demo-restart\n          unexpected_task: true",
            ),
        ];

        for raw in cases {
            let error = Config::from_yaml(&raw).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidConfiguration);
            assert!(error.message.contains("unknown field"), "{}", error.message);
        }
    }

    #[test]
    fn parses_explicit_trusted_jar_systemd_mechanism() {
        let raw = VALID_CONFIG.replace(
            "        target: test-server",
            "        mechanism:\n          type: jar_systemd\n        target: test-server",
        );
        let config = Config::from_yaml(&raw).unwrap();
        assert_eq!(
            config
                .environment("demo-service", "test")
                .unwrap()
                .mechanism
                .kind,
            DeploymentMechanismKind::JarSystemd
        );
    }

    #[test]
    fn rejects_unimplemented_mechanism_before_remote_work() {
        let raw = VALID_CONFIG.replace(
            "        target: test-server",
            "        mechanism:\n          type: docker_compose\n        target: test-server",
        );
        let error = Config::from_yaml(&raw).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfiguration);
        assert!(error.message.contains("docker_compose"));
        assert!(error.message.contains("not implemented in phase 8"));
    }

    #[test]
    fn rejects_empty_remote_exec_command() {
        let raw = VALID_CONFIG.replace("command: remote-exec-mcp", "command: ''");
        let error = Config::from_yaml(&raw).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfiguration);
        assert!(error
            .message
            .contains("remote_exec.command must not be empty"));
    }

    #[test]
    fn rejects_relative_deployment_paths() {
        let raw =
            VALID_CONFIG.replace("/opt/staging/demo-service.jar", "relative/demo-service.jar");
        let error = Config::from_yaml(&raw).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfiguration);
        assert!(error.message.contains("staging_path must be absolute"));
    }

    #[test]
    fn rejects_empty_task_references() {
        let raw = VALID_CONFIG.replace("restart: demo-restart", "restart: ''");
        let error = Config::from_yaml(&raw).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfiguration);
        assert!(error.message.contains("restart task must not be empty"));
    }

    #[test]
    fn rejects_unsafe_local_artifact_roots() {
        let relative = VALID_CONFIG.replace("/var/lib/deploy-mcp/artifacts", "relative/artifacts");
        let error = Config::from_yaml(&relative).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfiguration);
        assert!(error.message.contains("allowed root must be absolute"));

        let filesystem_root = VALID_CONFIG.replace("/var/lib/deploy-mcp/artifacts", "/");
        let error = Config::from_yaml(&filesystem_root).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfiguration);
        assert!(error.message.contains("must not be a filesystem root"));

        let traversal = VALID_CONFIG.replace(
            "/var/lib/deploy-mcp/artifacts",
            "/var/lib/deploy-mcp/../artifacts",
        );
        let error = Config::from_yaml(&traversal).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfiguration);
        assert!(error.message.contains("must not contain '..'"));
    }

    #[test]
    fn missing_local_artifact_policy_is_fail_closed_at_deploy_time() {
        let raw = VALID_CONFIG.replace(
            "local_artifacts:\n  allowed_roots:\n    - /var/lib/deploy-mcp/artifacts\n",
            "",
        );
        let config = Config::from_yaml(&raw).unwrap();
        assert!(config.local_artifacts.allowed_roots.is_empty());
    }

    #[test]
    fn rejects_invalid_rollback_retention_bounds() {
        let raw = VALID_CONFIG.replace(
            "applications:",
            "runtime:\n  rollback_reference_retention_days: 0\napplications:",
        );
        let error = Config::from_yaml(&raw).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfiguration);
        assert!(error.message.contains("rollback_reference_retention_days"));

        let raw = VALID_CONFIG.replace(
            "applications:",
            "runtime:\n  rollback_reference_cleanup_batch_size: 5001\napplications:",
        );
        let error = Config::from_yaml(&raw).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfiguration);
        assert!(error
            .message
            .contains("rollback_reference_cleanup_batch_size"));
    }

    #[test]
    fn unknown_application_and_environment_use_stable_codes() {
        let config = Config::from_yaml(VALID_CONFIG).unwrap();
        assert_eq!(
            config.application("missing").unwrap_err().code,
            ErrorCode::UnknownApplication
        );
        assert_eq!(
            config.environment("demo-service", "prod").unwrap_err().code,
            ErrorCode::UnknownEnvironment
        );
    }

    #[test]
    fn load_reports_io_failure_as_stable_configuration_error() {
        let directory = tempfile::tempdir().unwrap();
        let error = Config::load(directory.path().join("missing.yaml")).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidConfiguration);
    }
}
