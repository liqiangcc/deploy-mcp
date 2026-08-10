from pathlib import Path

# application/mod.rs
p = Path('src/application/mod.rs')
s = p.read_text()
assert 'mod mechanism;' not in s
s = s.replace('mod lock;\nmod preflight;\n', 'mod lock;\nmod mechanism;\nmod preflight;\n')
s = s.replace(
    'pub use lock::{DeploymentLease, DeploymentLockManager};\n',
    'pub use lock::{DeploymentLease, DeploymentLockManager};\n'
    'pub use mechanism::{\n'
    '    DeploymentMechanismAction, DeploymentMechanismPort, JarSystemdMechanism,\n'
    '    MechanismTaskExecution, RollbackMechanismAction, RollbackPreflightError,\n'
    '};\n',
)
p.write_text(s)

# application/api.rs
p = Path('src/application/api.rs')
s = p.read_text()
s = s.replace(
    '    DeployRequest, DeployService, DeploymentLockManager, DeploymentOutcome, RollbackOutcome,\n'
    '    RollbackRequest, RollbackService,\n',
    '    DeployRequest, DeployService, DeploymentLockManager, DeploymentOutcome, JarSystemdMechanism,\n'
    '    RollbackOutcome, RollbackRequest, RollbackService,\n',
)
s = s.replace(
    '    deploy: DeployService<R, D>,\n    rollback: RollbackService<R, D>,',
    '    deploy: DeployService<JarSystemdMechanism<R>, D>,\n'
    '    rollback: RollbackService<JarSystemdMechanism<R>, D>,',
)
old = '''        let locks = DeploymentLockManager::default();
        let deploy = DeployService::new(
            Arc::clone(&config),
            Arc::clone(&remote),
            Arc::clone(&repository),
        )
        .with_lock_manager(locks.clone());
        let rollback = RollbackService::new(
            Arc::clone(&config),
            remote,
            Arc::clone(&repository),
            Arc::clone(&rollback_repository),
            locks,
        );
'''
new = '''        let locks = DeploymentLockManager::default();
        let mechanism = Arc::new(JarSystemdMechanism::new(remote));
        let deploy = DeployService::with_mechanism(
            Arc::clone(&config),
            Arc::clone(&mechanism),
            Arc::clone(&repository),
        )
        .with_lock_manager(locks.clone());
        let rollback = RollbackService::with_mechanism(
            Arc::clone(&config),
            mechanism,
            Arc::clone(&repository),
            Arc::clone(&rollback_repository),
            locks,
        );
'''
assert old in s
s = s.replace(old, new)
p.write_text(s)

# application/deploy.rs
p = Path('src/application/deploy.rs')
s = p.read_text()
s = s.replace('use std::collections::BTreeMap;\n', '')
s = s.replace('use serde_json::Value;\n', '')
s = s.replace(
    'use super::{\n'
    '    artifact_access::resolve_allowed_artifact_path, preflight_remote_capabilities,\n'
    '    DeploymentLockManager, RemotePreflightError,\n'
    '};',
    'use super::{\n'
    '    artifact_access::resolve_allowed_artifact_path,\n'
    '    mechanism::{DeploymentMechanismAction, DeploymentMechanismPort, JarSystemdMechanism},\n'
    '    DeploymentLockManager, RemotePreflightError,\n'
    '};',
)
old = '''pub struct DeployService<R, D>
where
    R: RemoteExecutionPort + ?Sized,
    D: DeploymentRepository + Send,
{
    config: Arc<Config>,
    remote: Arc<R>,
    repository: Arc<Mutex<D>>,
    locks: DeploymentLockManager,
}

impl<R, D> DeployService<R, D>
where
    R: RemoteExecutionPort + ?Sized,
    D: DeploymentRepository + Send,
{
    pub fn new(config: Arc<Config>, remote: Arc<R>, repository: Arc<Mutex<D>>) -> Self {
        Self {
            config,
            remote,
            repository,
            locks: DeploymentLockManager::default(),
        }
    }

'''
new = '''pub struct DeployService<M, D>
where
    M: DeploymentMechanismPort + ?Sized,
    D: DeploymentRepository + Send,
{
    config: Arc<Config>,
    mechanism: Arc<M>,
    repository: Arc<Mutex<D>>,
    locks: DeploymentLockManager,
}

impl<R, D> DeployService<JarSystemdMechanism<R>, D>
where
    R: RemoteExecutionPort + ?Sized,
    D: DeploymentRepository + Send,
{
    pub fn new(config: Arc<Config>, remote: Arc<R>, repository: Arc<Mutex<D>>) -> Self {
        Self::with_mechanism(config, Arc::new(JarSystemdMechanism::new(remote)), repository)
    }
}

impl<M, D> DeployService<M, D>
where
    M: DeploymentMechanismPort + ?Sized,
    D: DeploymentRepository + Send,
{
    pub fn with_mechanism(
        config: Arc<Config>,
        mechanism: Arc<M>,
        repository: Arc<Mutex<D>>,
    ) -> Self {
        Self {
            config,
            mechanism,
            repository,
            locks: DeploymentLockManager::default(),
        }
    }

'''
assert old in s
s = s.replace(old, new)
old = '''        if let Some(precheck) = &environment.tasks.precheck {
            if let Some(failure) = self
                .execute_named_task(
                    deployment.id(),
                    DeploymentStep::Precheck,
                    &environment.target,
                    precheck,
                    BTreeMap::new(),
                    ErrorCode::PrecheckFailed,
                    "configured precheck task failed",
                )
                .await?
            {
                return self
                    .finish_primary_failure(deployment, &plan, &environment, failure)
                    .await;
            }
        }
'''
new = '''        if self
            .mechanism
            .action_is_configured(&environment, DeploymentMechanismAction::Precheck)
        {
            if let Some(failure) = self
                .execute_mechanism_task(
                    deployment.id(),
                    DeploymentStep::Precheck,
                    &environment,
                    DeploymentMechanismAction::Precheck,
                    ErrorCode::PrecheckFailed,
                    "configured precheck task failed",
                )
                .await?
            {
                return self
                    .finish_primary_failure(deployment, &plan, &environment, failure)
                    .await;
            }
        }
'''
assert old in s
s = s.replace(old, new)
replacements = [
('''            .execute_named_task(
                deployment.id(),
                DeploymentStep::BackupCurrent,
                &environment.target,
                &environment.tasks.backup,
                backup_parameters(&environment),
                ErrorCode::RemoteExecutionFailed,
                "backup task failed",
            )''', '''            .execute_mechanism_task(
                deployment.id(),
                DeploymentStep::BackupCurrent,
                &environment,
                DeploymentMechanismAction::CaptureRollback,
                ErrorCode::RemoteExecutionFailed,
                "backup task failed",
            )'''),
('''            .execute_named_task(
                deployment.id(),
                DeploymentStep::Install,
                &environment.target,
                &environment.tasks.install,
                install_parameters(&environment),
                ErrorCode::RemoteExecutionFailed,
                "install task failed",
            )''', '''            .execute_mechanism_task(
                deployment.id(),
                DeploymentStep::Install,
                &environment,
                DeploymentMechanismAction::Apply,
                ErrorCode::RemoteExecutionFailed,
                "install task failed",
            )'''),
('''            .execute_named_task(
                deployment.id(),
                DeploymentStep::Restart,
                &environment.target,
                &environment.tasks.restart,
                BTreeMap::new(),
                ErrorCode::RemoteExecutionFailed,
                "restart task failed",
            )''', '''            .execute_mechanism_task(
                deployment.id(),
                DeploymentStep::Restart,
                &environment,
                DeploymentMechanismAction::Activate,
                ErrorCode::RemoteExecutionFailed,
                "restart task failed",
            )'''),
('''            .execute_verification(
                deployment.id(),
                &environment.target,
                &environment.tasks.health_check,
                ErrorCode::VerificationFailed,
                "health-check task failed",
            )''', '''            .execute_verification(
                deployment.id(),
                &environment,
                ErrorCode::VerificationFailed,
                "health-check task failed",
            )'''),
]
for old, new in replacements:
    assert old in s
    s = s.replace(old, new)
s = s.replace(
    '            preflight_remote_capabilities(self.remote.as_ref(), environment),',
    '            self.mechanism.preflight(environment),',
)
old = '''            self.remote.upload_file(
                &environment.target,
                local_path,
                &environment.staging_path,
                true,
            ),'''
assert old in s
s = s.replace(old, '            self.mechanism.prepare(environment, local_path),')
start = s.index('    #[allow(clippy::too_many_arguments)]\n    async fn execute_named_task(')
end = s.index('    async fn finish_primary_failure(', start)
s = s[:start] + '''    async fn execute_mechanism_task(
        &self,
        deployment_id: &DeploymentId,
        step: DeploymentStep,
        environment: &EnvironmentConfig,
        mechanism_action: DeploymentMechanismAction,
        failure_code: ErrorCode,
        action: &str,
    ) -> AppResult<Option<DeploymentFailure>> {
        let attempt = self.start_attempt(deployment_id, step)?;
        let timeout_ms = self.config.runtime.deployment_step_timeout_ms;
        let result = timeout(
            Duration::from_millis(timeout_ms),
            self.mechanism.execute(environment, mechanism_action),
        )
        .await;
        let failure = match result {
            Err(_) => Some(deployment_timeout_failure(step, timeout_ms)),
            Ok(Ok(execution)) if execution.result.success => None,
            Ok(Ok(execution)) => Some(task_result_failure(
                step,
                failure_code,
                action,
                &execution.task,
                &execution.result,
            )),
            Ok(Err(error)) => Some(DeploymentFailure::from_remote(
                step,
                failure_code,
                action,
                error,
            )),
        };

        self.finish_from_failure(attempt, failure.as_ref())?;
        Ok(failure)
    }

    async fn execute_verification(
        &self,
        deployment_id: &DeploymentId,
        environment: &EnvironmentConfig,
        failure_code: ErrorCode,
        action: &str,
    ) -> AppResult<Option<DeploymentFailure>> {
        let max_attempts = self.config.runtime.verification_max_attempts;
        let retry_delay_ms = self.config.runtime.verification_retry_delay_ms;
        for attempt in 1..=max_attempts {
            let failure = self
                .execute_mechanism_task(
                    deployment_id,
                    DeploymentStep::Verify,
                    environment,
                    DeploymentMechanismAction::Verify,
                    failure_code,
                    action,
                )
                .await?;
            match failure {
                None => return Ok(None),
                Some(failure) if failure.code == ErrorCode::OperationTimedOut => {
                    return Ok(Some(failure));
                }
                Some(failure) if attempt == max_attempts => return Ok(Some(failure)),
                Some(_) => sleep(Duration::from_millis(retry_delay_ms)).await,
            }
        }
        unreachable!("validated verification policy always has at least one attempt")
    }

''' + s[end:]
old = '''        let rollback_task = match environment.tasks.rollback.as_deref() {
            Some(task) => task,
            None => {
                return Ok(Some(DeploymentFailure::new(
                    DeploymentStep::Install,
                    ErrorCode::RollbackUnavailable,
                    "rollback was required but no rollback task is configured",
                )))
            }
        };

        if let Some(failure) = self
            .execute_named_task(
                deployment_id,
                DeploymentStep::Install,
                &environment.target,
                rollback_task,
                rollback_parameters(environment),
                ErrorCode::RollbackFailed,
                "rollback restore task failed",
            )
            .await?
        {
            return Ok(Some(failure));
        }

        if let Some(failure) = self
            .execute_named_task(
                deployment_id,
                DeploymentStep::Restart,
                &environment.target,
                &environment.tasks.restart,
                BTreeMap::new(),
                ErrorCode::RollbackFailed,
                "rollback restart task failed",
            )
            .await?
        {
            return Ok(Some(failure));
        }

        self.execute_verification(
            deployment_id,
            &environment.target,
            &environment.tasks.health_check,
            ErrorCode::RollbackFailed,
            "rollback verification task failed",
        )
        .await
'''
new = '''        if !self
            .mechanism
            .action_is_configured(environment, DeploymentMechanismAction::RollbackRestore)
        {
            return Ok(Some(DeploymentFailure::new(
                DeploymentStep::Install,
                ErrorCode::RollbackUnavailable,
                "rollback was required but no rollback task is configured",
            )));
        }

        if let Some(failure) = self
            .execute_mechanism_task(
                deployment_id,
                DeploymentStep::Install,
                environment,
                DeploymentMechanismAction::RollbackRestore,
                ErrorCode::RollbackFailed,
                "rollback restore task failed",
            )
            .await?
        {
            return Ok(Some(failure));
        }

        if let Some(failure) = self
            .execute_mechanism_task(
                deployment_id,
                DeploymentStep::Restart,
                environment,
                DeploymentMechanismAction::Activate,
                ErrorCode::RollbackFailed,
                "rollback restart task failed",
            )
            .await?
        {
            return Ok(Some(failure));
        }

        self.execute_verification(
            deployment_id,
            environment,
            ErrorCode::RollbackFailed,
            "rollback verification task failed",
        )
        .await
'''
assert old in s
s = s.replace(old, new)
for fn_name in ['backup_parameters', 'install_parameters', 'rollback_parameters']:
    marker = f'fn {fn_name}('
    i = s.index(marker)
    b = s.index('{', i)
    depth = 0
    j = b
    while j < len(s):
        if s[j] == '{':
            depth += 1
        elif s[j] == '}':
            depth -= 1
            if depth == 0:
                j += 1
                while j < len(s) and s[j] == '\n':
                    j += 1
                break
        j += 1
    s = s[:i] + s[j:]
p.write_text(s)

# application/rollback.rs
p = Path('src/application/rollback.rs')
s = p.read_text()
s = s.replace('use std::collections::{BTreeMap, BTreeSet};\n', '')
s = s.replace('use serde_json::Value;\n', '')
s = s.replace(
    'use super::DeploymentLockManager;',
    'use super::{\n'
    '    mechanism::{\n'
    '        DeploymentMechanismPort, JarSystemdMechanism, RollbackMechanismAction,\n'
    '        RollbackPreflightError,\n'
    '    },\n'
    '    DeploymentLockManager,\n'
    '};',
)
old = '''pub struct RollbackService<R, D>
where
    R: RemoteExecutionPort + ?Sized,
    D: DeploymentRepository + Send,
{
    config: Arc<Config>,
    remote: Arc<R>,
    deployments: Arc<Mutex<D>>,
    rollbacks: Arc<Mutex<Box<dyn RollbackRepository + Send>>>,
    locks: DeploymentLockManager,
}

impl<R, D> RollbackService<R, D>
where
    R: RemoteExecutionPort + ?Sized,
    D: DeploymentRepository + Send,
{
    pub fn new(
        config: Arc<Config>,
        remote: Arc<R>,
        deployments: Arc<Mutex<D>>,
        rollbacks: Arc<Mutex<Box<dyn RollbackRepository + Send>>>,
        locks: DeploymentLockManager,
    ) -> Self {
        Self {
            config,
            remote,
            deployments,
            rollbacks,
            locks,
        }
    }

'''
new = '''pub struct RollbackService<M, D>
where
    M: DeploymentMechanismPort + ?Sized,
    D: DeploymentRepository + Send,
{
    config: Arc<Config>,
    mechanism: Arc<M>,
    deployments: Arc<Mutex<D>>,
    rollbacks: Arc<Mutex<Box<dyn RollbackRepository + Send>>>,
    locks: DeploymentLockManager,
}

impl<R, D> RollbackService<JarSystemdMechanism<R>, D>
where
    R: RemoteExecutionPort + ?Sized,
    D: DeploymentRepository + Send,
{
    pub fn new(
        config: Arc<Config>,
        remote: Arc<R>,
        deployments: Arc<Mutex<D>>,
        rollbacks: Arc<Mutex<Box<dyn RollbackRepository + Send>>>,
        locks: DeploymentLockManager,
    ) -> Self {
        Self::with_mechanism(
            config,
            Arc::new(JarSystemdMechanism::new(remote)),
            deployments,
            rollbacks,
            locks,
        )
    }
}

impl<M, D> RollbackService<M, D>
where
    M: DeploymentMechanismPort + ?Sized,
    D: DeploymentRepository + Send,
{
    pub fn with_mechanism(
        config: Arc<Config>,
        mechanism: Arc<M>,
        deployments: Arc<Mutex<D>>,
        rollbacks: Arc<Mutex<Box<dyn RollbackRepository + Send>>>,
        locks: DeploymentLockManager,
    ) -> Self {
        Self {
            config,
            mechanism,
            deployments,
            rollbacks,
            locks,
        }
    }

'''
assert old in s
s = s.replace(old, new)
old = '''            if let Some(failure) = self
                .run_task(
                    &reference,
                    reference.rollback_task(),
                    BTreeMap::from([
                        (
                            "backup_path".to_owned(),
                            Value::String(reference.backup_path().to_owned()),
                        ),
                        (
                            "install_path".to_owned(),
                            Value::String(reference.install_path().to_owned()),
                        ),
                    ]),
                    "rollback restore task failed",
                )
                .await
'''
new = '''            if let Some(failure) = self
                .run_task(
                    &reference,
                    RollbackMechanismAction::Restore,
                    "rollback restore task failed",
                )
                .await
'''
assert old in s
s = s.replace(old, new)
old = '''            if let Some(failure) = self
                .run_task(
                    &reference,
                    reference.restart_task(),
                    BTreeMap::new(),
                    "rollback restart task failed",
                )
                .await
'''
new = '''            if let Some(failure) = self
                .run_task(
                    &reference,
                    RollbackMechanismAction::Activate,
                    "rollback restart task failed",
                )
                .await
'''
assert old in s
s = s.replace(old, new)
start = s.index('    async fn preflight(&self, reference: &RollbackReference) -> Option<RollbackFailure> {')
end = s.index('    fn finish_failed(', start)
s = s[:start] + '''    async fn preflight(&self, reference: &RollbackReference) -> Option<RollbackFailure> {
        match self.mechanism.preflight_rollback(reference).await {
            Ok(_) => None,
            Err(RollbackPreflightError::TargetUnreachable(target)) => Some(RollbackFailure::new(
                ErrorCode::PrecheckFailed,
                format!("rollback target is not reachable: {target}"),
            )),
            Err(RollbackPreflightError::MissingCapabilities { tasks, .. }) => {
                Some(RollbackFailure::new(
                    ErrorCode::RemoteCapabilityMissing,
                    format!("rollback target is missing capabilities: {tasks:?}"),
                ))
            }
            Err(RollbackPreflightError::TargetCheck(error)) => Some(RollbackFailure::from_remote(
                ErrorCode::PrecheckFailed,
                "rollback target preflight failed",
                error,
            )),
            Err(RollbackPreflightError::CapabilityDiscovery(error)) => {
                Some(RollbackFailure::from_remote(
                    ErrorCode::PrecheckFailed,
                    "rollback capability preflight failed",
                    error,
                ))
            }
        }
    }

    async fn run_task(
        &self,
        reference: &RollbackReference,
        mechanism_action: RollbackMechanismAction,
        action: &str,
    ) -> Option<RollbackFailure> {
        match self
            .mechanism
            .execute_rollback(reference, mechanism_action)
            .await
        {
            Ok(execution) if execution.result.success => None,
            Ok(execution) => Some(task_failure(action, &execution.task, &execution.result)),
            Err(error) => Some(RollbackFailure::from_remote(
                ErrorCode::RollbackFailed,
                action,
                error,
            )),
        }
    }

    async fn run_verification_with_retry(
        &self,
        reference: &RollbackReference,
    ) -> Option<RollbackFailure> {
        let max_attempts = self.config.runtime.verification_max_attempts;
        let retry_delay_ms = self.config.runtime.verification_retry_delay_ms;
        for attempt in 1..=max_attempts {
            let failure = self
                .run_task(
                    reference,
                    RollbackMechanismAction::Verify,
                    "rollback verification task failed",
                )
                .await;
            match failure {
                None => return None,
                Some(failure) if attempt == max_attempts => return Some(failure),
                Some(_) => sleep(Duration::from_millis(retry_delay_ms)).await,
            }
        }
        unreachable!("validated verification policy always has at least one attempt")
    }

''' + s[end:]
p.write_text(s)
