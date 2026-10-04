//! Shared cooperative budgets for explicit checking, never ordinary queries.
use crate::domain::{ErrorCode, Result, WikiError};
use rusqlite::{Connection, limits::Limit};
use serde::Serialize;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub(crate) struct CheckLimits {
    pub max_elapsed: Duration,
    pub max_vm_steps: u64,
    pub max_rows: u64,
    pub max_postings: u64,
    pub max_row_bytes: u64,
    pub max_source_bytes: u64,
    pub max_scratch_bytes: u64,
    pub sqlite_cache_bytes: u64,
    pub max_history_steps: u64,
    pub max_diagnostics: usize,
    pub max_diagnostic_bytes: u64,
}
impl Default for CheckLimits {
    fn default() -> Self {
        Self {
            max_elapsed: Duration::from_secs(30 * 60),
            max_vm_steps: 100_000_000_000,
            max_rows: 10_000_000,
            max_postings: 10_000_000_000,
            max_row_bytes: 128 * 1024 * 1024,
            max_source_bytes: 128 * 1024 * 1024 * 1024,
            max_scratch_bytes: 32 * 1024 * 1024 * 1024,
            sqlite_cache_bytes: 32 * 1024 * 1024,
            max_history_steps: 10_000_000,
            max_diagnostics: 100_000,
            max_diagnostic_bytes: 16 * 1024 * 1024,
        }
    }
}

struct State {
    started: Instant,
    limits: CheckLimits,
    poisoned: AtomicBool,
    vm_steps: AtomicU64,
    rows: AtomicU64,
    postings: AtomicU64,
    compared_bytes: AtomicU64,
}

/// Clones share one deadline, work accounting and irreversible failure state.
#[derive(Clone)]
pub(crate) struct CheckBudget(Arc<State>);

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CheckStats {
    pub rows: u64,
    pub postings: u64,
    pub compared_bytes: u64,
    pub sql_vm_steps: u64,
    pub elapsed_ms: u64,
}

impl CheckBudget {
    pub(crate) fn new(limits: CheckLimits) -> Result<Self> {
        let started = Instant::now();
        if limits.max_elapsed.is_zero()
            || started.checked_add(limits.max_elapsed).is_none()
            || limits.max_vm_steps < 1000
            || limits.max_rows == 0
            || limits.max_postings == 0
            || limits.max_row_bytes == 0
            || limits.max_row_bytes > (i32::MAX as u64 - 65536)
            || limits.max_source_bytes < 4096
            || limits.max_scratch_bytes < 4096
            || limits.max_scratch_bytes / 4096 > 4_294_967_294
            || limits.sqlite_cache_bytes < 1024
            || limits.sqlite_cache_bytes / 1024 > i32::MAX as u64
            || limits.max_history_steps == 0
            || limits.max_diagnostics == 0
            || limits.max_diagnostic_bytes == 0
        {
            return Err(WikiError::new(
                ErrorCode::ConfigInvalid,
                "invalid explicit-check limits",
            ));
        }
        Ok(Self(Arc::new(State {
            started,
            limits,
            poisoned: AtomicBool::new(false),
            vm_steps: AtomicU64::new(0),
            rows: AtomicU64::new(0),
            postings: AtomicU64::new(0),
            compared_bytes: AtomicU64::new(0),
        })))
    }
    pub(crate) fn limits(&self) -> &CheckLimits {
        &self.0.limits
    }
    pub(crate) fn remaining_time(&self) -> Result<Duration> {
        self.guard()?;
        Ok(self
            .0
            .limits
            .max_elapsed
            .saturating_sub(self.0.started.elapsed()))
    }
    pub(crate) fn guard(&self) -> Result<()> {
        if self.0.poisoned.load(Ordering::Relaxed) {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "explicit check is poisoned; discard its scratch",
            ));
        }
        if self.0.started.elapsed() >= self.0.limits.max_elapsed {
            return Err(self.fail(
                ErrorCode::BudgetExceeded,
                "explicit check deadline exceeded",
            ));
        }
        Ok(())
    }
    pub(crate) fn fail(&self, code: ErrorCode, message: impl Into<String>) -> WikiError {
        self.0.poisoned.store(true, Ordering::Relaxed);
        WikiError::new(code, message)
    }
    pub(crate) fn sql_error(&self, error: rusqlite::Error) -> WikiError {
        self.0.poisoned.store(true, Ordering::Relaxed);
        if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) {
            return WikiError::new(
                ErrorCode::BudgetExceeded,
                "explicit check exhausted its temporary database allowance or available disk space",
            );
        }
        super::sql::sql_error(error)
    }
    pub(crate) fn admit_row(&self, bytes: u64) -> Result<()> {
        self.guard()?;
        if bytes > self.0.limits.max_row_bytes {
            return Err(self.fail(
                ErrorCode::BudgetExceeded,
                "explicit check row exceeds byte limit",
            ));
        }
        Self::increment(&self.0.rows, 1, self.0.limits.max_rows).ok_or_else(|| {
            self.fail(
                ErrorCode::BudgetExceeded,
                "explicit check row limit exceeded",
            )
        })?;
        self.account_bytes(bytes)
    }
    pub(crate) fn admit_posting(&self, bytes: u64) -> Result<()> {
        self.guard()?;
        if bytes > self.0.limits.max_row_bytes {
            return Err(self.fail(
                ErrorCode::BudgetExceeded,
                "explicit check posting exceeds byte limit",
            ));
        }
        Self::increment(&self.0.postings, 1, self.0.limits.max_postings).ok_or_else(|| {
            self.fail(
                ErrorCode::BudgetExceeded,
                "explicit check posting limit exceeded",
            )
        })?;
        self.account_bytes(bytes)
    }
    fn increment(counter: &AtomicU64, amount: u64, ceiling: u64) -> Option<u64> {
        counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                old.checked_add(amount).filter(|new| *new <= ceiling)
            })
            .ok()
    }
    fn account_bytes(&self, bytes: u64) -> Result<()> {
        Self::increment(&self.0.compared_bytes, bytes, u64::MAX).ok_or_else(|| {
            self.fail(
                ErrorCode::BudgetExceeded,
                "explicit check byte accounting overflow",
            )
        })?;
        Ok(())
    }
    /// Set ceilings before schema preparation. The caller separately selects
    /// READ_ONLY/query_only or the exact owned scratch database.
    pub(crate) fn configure_sql(&self, connection: &Connection) -> Result<()> {
        self.guard()?;
        connection
            .busy_timeout(Duration::ZERO)
            .map_err(|e| self.sql_error(e))?;
        connection
            .set_limit(
                Limit::SQLITE_LIMIT_LENGTH,
                self.0.limits.max_row_bytes as i32,
            )
            .map_err(|e| self.sql_error(e))?;
        connection
            .set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 256 * 1024)
            .map_err(|e| self.sql_error(e))?;
        let budget = self.clone();
        connection
            .progress_handler(
                1000,
                Some(move || {
                    let interrupted = budget.0.poisoned.load(Ordering::Relaxed)
                        || budget.0.started.elapsed() >= budget.0.limits.max_elapsed
                        || Self::increment(&budget.0.vm_steps, 1000, budget.0.limits.max_vm_steps)
                            .is_none();
                    if interrupted {
                        budget.0.poisoned.store(true, Ordering::Relaxed);
                    }
                    interrupted
                }),
            )
            .map_err(|e| self.sql_error(e))?;
        connection
            .execute_batch(&format!(
                "PRAGMA mmap_size=0; PRAGMA cache_size=-{}; PRAGMA temp_store=FILE;",
                self.0.limits.sqlite_cache_bytes / 1024,
            ))
            .map_err(|e| self.sql_error(e))?;
        Ok(())
    }
    pub(crate) fn stats(&self) -> CheckStats {
        CheckStats {
            rows: self.0.rows.load(Ordering::Relaxed),
            postings: self.0.postings.load(Ordering::Relaxed),
            compared_bytes: self.0.compared_bytes.load(Ordering::Relaxed),
            sql_vm_steps: self.0.vm_steps.load(Ordering::Relaxed),
            elapsed_ms: self.0.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        }
    }
}
