use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug, Clone, Default)]
pub struct DeploymentLockManager {
    active: Arc<Mutex<BTreeSet<(String, String)>>>,
}

impl DeploymentLockManager {
    pub fn try_acquire(
        &self,
        application: impl Into<String>,
        environment: impl Into<String>,
    ) -> Option<DeploymentLease> {
        let key = (application.into(), environment.into());
        let mut active = lock_recover(&self.active);
        if !active.insert(key.clone()) {
            return None;
        }

        Some(DeploymentLease {
            active: Arc::clone(&self.active),
            key: Some(key),
        })
    }
}

#[derive(Debug)]
pub struct DeploymentLease {
    active: Arc<Mutex<BTreeSet<(String, String)>>>,
    key: Option<(String, String)>,
}

impl Drop for DeploymentLease {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            lock_recover(&self.active).remove(&key);
        }
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_application_environment_is_exclusive_until_lease_drops() {
        let locks = DeploymentLockManager::default();
        let lease = locks.try_acquire("demo", "test").unwrap();
        assert!(locks.try_acquire("demo", "test").is_none());
        assert!(locks.try_acquire("demo", "prod").is_some());
        drop(lease);
        assert!(locks.try_acquire("demo", "test").is_some());
    }
}
