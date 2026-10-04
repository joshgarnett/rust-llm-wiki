//! Explicit, read-only catalog reconciliation. Never called by ordinary reads or updates.
use super::{
    Catalog, CatalogDiagnostic,
    compact_audit::CompactStats,
    full_check_rows::AuditSink,
    full_check_scratch::CheckScratch,
    full_check_types::{CheckBudget, CheckLimits, CheckStats},
    maintenance_input::MaintenanceInput,
    maintenance_types::{MaintenanceLimits, MaintenanceUsage},
    normalized_build::counted_json,
    normalized_read::{self, CatalogHeader},
    scan, selector,
};
use crate::{
    changes::operation_authority::{Authority, Publication},
    domain::{ErrorCode, ReadSnapshot, Result, WikiError},
    vault::WriterPermit,
};
use rusqlite::{Connection, OpenFlags};
use std::time::Duration;

#[cfg(test)]
#[path = "full_check_tests.rs"]
mod tests;

pub(crate) struct CheckReport {
    pub diagnostics: Vec<CatalogDiagnostic>,
    pub snapshot: ReadSnapshot,
    pub input: MaintenanceUsage,
    pub work: CheckStats,
    pub search_index: CompactStats,
    pub scratch_bytes: u64,
}

impl Catalog {
    pub(crate) fn check_normalized(&self, writer: &WriterPermit) -> Result<CheckReport> {
        self.check_normalized_with_limits(writer, CheckLimits::default())
    }

    pub(crate) fn check_normalized_with_limits(
        &self,
        writer: &WriterPermit,
        limits: CheckLimits,
    ) -> Result<CheckReport> {
        let budget = CheckBudget::new(limits)?;
        let mut phase = "source_admission";
        let mut checked_snapshot = None;
        let mut scratch_cleaned = None;
        let result = (|| {
            writer.require_root(self.fs.root())?;
            self.fs.require_storage_ready()?;
            if self.options.busy_timeout_ms > 30_000 {
                return Err(WikiError::new(
                    ErrorCode::ConfigInvalid,
                    "catalog busy timeout exceeds 30 seconds",
                ));
            }
            let authority = self.operation_state()?.ok_or_else(|| {
                WikiError::new(
                    ErrorCode::OfflineUnavailable,
                    "normalized catalog is absent; check cannot audit it",
                )
            })?;
            authority.require_publication(authority.publication())?;
            let selected = self.check_selection(&authority, &budget)?;
            let (connection, header) = selected.value();
            checked_snapshot = Some(header.snapshot.clone());
            phase = "scratch_admission";
            let scratch = CheckScratch::create()?;
            let checked = (|| {
                phase = "native_integrity";
                let mut sink = AuditSink::new(connection, scratch.connection(), budget.clone())?;
                phase = "canonical_projection";
                let mut input_limits = MaintenanceLimits::rebuild();
                input_limits.max_elapsed = budget.remaining_time()?;
                let input = MaintenanceInput::capture(&self.fs, &self.vault_id, input_limits)?;
                let projection = scan::project_maintenance_with_sink(&input, &mut sink)?;
                budget.guard()?;
                if projection.validation.diagnostics.len() > budget.limits().max_diagnostics {
                    return Err(budget.fail(
                        ErrorCode::BudgetExceeded,
                        "explicit check diagnostic count exceeds allowance",
                    ));
                }
                counted_json(
                    &projection.validation.diagnostics,
                    budget.limits().max_diagnostic_bytes,
                )?;
                phase = "metadata_and_search_index";
                let search_index = sink.finish(&projection, &self.fs, writer)?;
                phase = "final_recheck";
                input.final_recheck()?;
                budget.guard()?;
                self.check_authority(&authority)?;
                // Open a fresh transaction: re-reading the pinned header alone
                // cannot detect an out-of-band epoch change during this check.
                let current = self.check_selection(&authority, &budget)?;
                if current.selection() != selected.selection()
                    || current.value().1.snapshot != header.snapshot
                {
                    return Err(budget.fail(
                        ErrorCode::ContentConflict,
                        "selected catalog changed during explicit check",
                    ));
                }
                self.check_authority(&authority)?;
                scratch.verify()?;
                let scratch_bytes = scratch.logical_bytes()?;
                budget.guard()?;
                Ok(CheckReport {
                    diagnostics: projection.validation.diagnostics,
                    snapshot: header.snapshot.clone(),
                    input: input.usage(),
                    work: budget.stats(),
                    search_index,
                    scratch_bytes,
                })
            })();
            // Close all statements/borrowed audit state before exact owned cleanup.
            if checked.is_ok() {
                phase = "scratch_cleanup";
            }
            let cleanup = scratch.cleanup();
            scratch_cleaned = Some(cleanup.is_ok());
            match (checked, cleanup) {
                (Ok(report), Ok(())) => Ok(report),
                (Err(mut error), Err(cleanup)) => {
                    error.details =
                        serde_json::json!({"cause":error.details,"cleanup_errors":[cleanup]});
                    Err(error)
                }
                (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            }
        })();
        result.and_then(|mut report| {
            budget.guard()?;
            report.work = budget.stats();
            Ok(report)
        }).map_err(|mut error| {
            // Any failure invalidates this invocation; scratch cannot be retried
            // after a failed FTS mutation with journal_mode=OFF.
            let _ = budget.fail(error.code, "explicit check failed");
            error.details = serde_json::json!({
                "complete":false,"cache_matches_canonical":null,
                "phase":phase,"checked_snapshot":checked_snapshot,"scratch_cleaned":scratch_cleaned,
                "work":budget.stats(),"cause":error.details,
            });
            if error.code == ErrorCode::IndexCorrupt && error.hint.is_none() {
                error.hint = Some("Run index sync for external edits, or index rebuild to replace a corrupt cache.".into());
            }
            error
        })
    }

    fn check_authority(&self, expected: &Authority) -> Result<()> {
        if self
            .operation_state()?
            .as_ref()
            .is_none_or(|actual| !expected.same_revision(actual))
        {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "catalog operation authority changed during explicit check",
            ));
        }
        Ok(())
    }

    fn check_selection(
        &self,
        authority: &Authority,
        budget: &CheckBudget,
    ) -> Result<selector::Selected<(Connection, CatalogHeader)>> {
        budget.guard()?;
        selector::acquire(&self.fs, &self.vault_id,
            Duration::from_millis(self.options.busy_timeout_ms), |path, selection| {
                let connection = Connection::open_with_flags(path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
                        | OpenFlags::SQLITE_OPEN_NOFOLLOW).map_err(|e|budget.sql_error(e))?;
                budget.configure_sql(&connection)?;
                connection.execute_batch("PRAGMA query_only=ON; BEGIN DEFERRED")
                    .map_err(|e|budget.sql_error(e))?;
                let header = normalized_read::header(&connection, selection)?;
                authority.require_publication(&Publication {
                    file_id:selection.file_id.clone(), epoch:header.snapshot.generation,
                })?;
                let (count,proof,ownership):(i64,i64,i64) = connection.query_row(
                    "SELECT (SELECT count(*) FROM catalog_meta),proof_layout_version,revision_ownership_version FROM catalog_meta WHERE singleton=1",
                    [],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))
                    .map_err(|e|budget.sql_error(e))?;
                if count != 1 || proof != 2 || ownership != 1
                    || header.snapshot.parser_fingerprint != scan::parser_fingerprint()
                {
                    return Err(budget.fail(ErrorCode::IndexCorrupt,
                        "selected catalog has an incompatible header; run index rebuild"));
                }
                budget.guard()?;
                Ok((connection,header))
            })?.ok_or_else(|| budget.fail(ErrorCode::IndexCorrupt,
                "selected normalized catalog is absent; run index rebuild"))
    }
}
