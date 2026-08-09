use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{AppError, AppResult, ErrorCode};
use crate::ports::{RepositoryError, RollbackRetentionRepository};

const MILLIS_PER_DAY: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollbackRetentionReport {
    pub pruned_references: usize,
    pub cutoff_unix_ms: i64,
}

pub struct RollbackRetentionService<'a> {
    repository: &'a mut dyn RollbackRetentionRepository,
    retention_days: u32,
    batch_size: usize,
}

impl<'a> RollbackRetentionService<'a> {
    pub fn new(
        repository: &'a mut dyn RollbackRetentionRepository,
        retention_days: u32,
        batch_size: usize,
    ) -> Self {
        Self {
            repository,
            retention_days,
            batch_size,
        }
    }

    pub fn cleanup(&mut self) -> AppResult<RollbackRetentionReport> {
        self.cleanup_at(now_unix_ms())
    }

    fn cleanup_at(&mut self, now_unix_ms: i64) -> AppResult<RollbackRetentionReport> {
        let retention_ms = i64::from(self.retention_days).saturating_mul(MILLIS_PER_DAY);
        let cutoff_unix_ms = now_unix_ms.saturating_sub(retention_ms);
        let pruned_references = self
            .repository
            .prune_inactive_reference_snapshots(cutoff_unix_ms, self.batch_size)
            .map_err(repository_error)?;
        Ok(RollbackRetentionReport {
            pruned_references,
            cutoff_unix_ms,
        })
    }
}

fn repository_error(error: RepositoryError) -> AppError {
    AppError::new(ErrorCode::PersistenceFailed, error.to_string())
}

fn now_unix_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::RepositoryResult;

    #[derive(Default)]
    struct FakeRetentionRepository {
        cutoff: Option<i64>,
        limit: Option<usize>,
        result: usize,
    }

    impl RollbackRetentionRepository for FakeRetentionRepository {
        fn prune_inactive_reference_snapshots(
            &mut self,
            cutoff_unix_ms: i64,
            limit: usize,
        ) -> RepositoryResult<usize> {
            self.cutoff = Some(cutoff_unix_ms);
            self.limit = Some(limit);
            Ok(self.result)
        }
    }

    #[test]
    fn cleanup_uses_bounded_age_cutoff_without_remote_work() {
        let mut repository = FakeRetentionRepository {
            result: 7,
            ..Default::default()
        };
        {
            let mut service = RollbackRetentionService::new(&mut repository, 30, 500);
            let report = service.cleanup_at(100 * MILLIS_PER_DAY).unwrap();
            assert_eq!(report.pruned_references, 7);
            assert_eq!(report.cutoff_unix_ms, 70 * MILLIS_PER_DAY);
        }
        assert_eq!(repository.cutoff, Some(70 * MILLIS_PER_DAY));
        assert_eq!(repository.limit, Some(500));
    }
}
