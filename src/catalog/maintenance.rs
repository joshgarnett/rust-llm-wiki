//! Explicit reconstruction. Ordinary readers and document updates never enter here.
use super::{
    Catalog, SyncReport,
    file_types::{BuildIdentity, CatalogSelection},
    maintenance_input::MaintenanceInput,
    maintenance_types::{MaintenanceLimits, MaintenanceUsage},
    normalized_build::{BuildLimits, BuildStats, NormalizedBuilder},
    scan, selector, sql,
};
use crate::{
    domain::{ErrorCode, Result, WikiError},
    vault::WriterPermit,
};
use std::time::{Duration, Instant};

#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod tests;

pub(crate) struct MaintenanceReport {
    pub report: SyncReport,
    pub input: MaintenanceUsage,
    pub build: Option<BuildStats>,
    pub page_sync: Option<super::maintenance_page_delta::PageSyncStats>,
    pub resumed: bool,
    pub retirement_deferred: bool,
    pub cleanup_errors: Vec<WikiError>,
    pub abandoned_rebuild_candidates: Vec<super::missing_cache::Abandoned>,
}

impl MaintenanceLimits {
    /// Cooperative input ceilings; native memory and whole-command time are
    /// separately qualified by the public lifecycle benchmark.
    pub(crate) fn rebuild() -> Self {
        Self {
            max_files: 2_000_000,
            max_path_steps: 10_000_000,
            max_manifest_bytes: 512 * 1024 * 1024,
            max_file_bytes: 64 * 1024 * 1024,
            max_retained_note_bytes: 512 * 1024 * 1024,
            max_io_bytes: 128 * 1024 * 1024 * 1024,
            max_elapsed: Duration::from_secs(4 * 60 * 60),
        }
    }
}

impl Catalog {
    pub(crate) fn rebuild_normalized(&self, writer: &WriterPermit) -> Result<MaintenanceReport> {
        self.rebuild_normalized_with_limits(
            writer,
            MaintenanceLimits::rebuild(),
            BuildLimits::default(),
        )
    }

    pub(crate) fn sync_normalized(&self, writer: &WriterPermit) -> Result<MaintenanceReport> {
        let mut limits = MaintenanceLimits::rebuild();
        limits.max_elapsed = Duration::from_secs(30 * 60);
        self.maintain_normalized(writer, false, limits, BuildLimits::default())
    }

    pub(crate) fn rebuild_normalized_with_limits(
        &self,
        writer: &WriterPermit,
        input_limits: MaintenanceLimits,
        build_limits: BuildLimits,
    ) -> Result<MaintenanceReport> {
        self.maintain_normalized(writer, true, input_limits, build_limits)
    }

    fn maintain_normalized(
        &self,
        writer: &WriterPermit,
        force: bool,
        input_limits: MaintenanceLimits,
        build_limits: BuildLimits,
    ) -> Result<MaintenanceReport> {
        #[cfg(test)]
        super::query_diagnostics::access("catalog_maintenance");
        writer.require_root(self.fs.root())?;
        self.fs.require_storage_ready()?;
        if self.options.busy_timeout_ms > 30_000 {
            return Err(WikiError::new(
                ErrorCode::ConfigInvalid,
                "catalog busy timeout exceeds 30 seconds",
            ));
        }
        let timeout = Duration::from_millis(self.options.busy_timeout_ms);
        let reconstruction =
            super::missing_cache::prepare(&self.fs, writer, &self.vault_id, force, timeout)?;
        // Optional discovery for exact predecessor retirement only. The recovery
        // and admission below independently propagate all control/binding errors.
        let mut predecessors: Vec<_> =
            selector::maintenance_header(&self.fs, &self.vault_id, timeout)
                .ok()
                .flatten()
                .map(|(selection, _)| selection)
                .into_iter()
                .collect();
        // Finish only a previously acknowledged exact sibling. A canonical
        // operation in progress must use changes recovery instead.
        let resumed = reconstruction.resumed
            || (reconstruction.candidate.is_none()
                && selector::resume_acknowledged_rebuild(
                    &self.fs,
                    writer,
                    &self.vault_id,
                    timeout,
                )?);
        self.guard_current(None)?;
        let authority = self.operation_state()?;
        let previous = if reconstruction.candidate.is_some() {
            None
        } else {
            match selector::maintenance_header(&self.fs, &self.vault_id, timeout) {
                Ok(header) => header,
                Err(error)
                    if force
                        && authority.is_some()
                        && matches!(
                            error.code,
                            ErrorCode::IndexCorrupt | ErrorCode::OfflineUnavailable
                        ) =>
                {
                    let selected = selector::validate_rebuild_predecessor(
                        &self.fs,
                        writer,
                        &self.vault_id,
                        timeout,
                    )?;
                    if !predecessors.contains(&selected) {
                        predecessors.push(selected);
                    }
                    None
                }
                Err(error) => return Err(error),
            }
        };
        if let Some((selected, _)) = &previous
            && !predecessors.iter().any(|old| old == selected)
        {
            predecessors.push(selected.clone());
        }
        let mut input = None;
        if previous.is_some() && (!force || resumed) {
            let comparison_started = Instant::now();
            let captured =
                MaintenanceInput::capture(&self.fs, &self.vault_id, input_limits.clone())?;
            let comparison = super::maintenance_match::compare(self, &captured)?;
            let comparison_elapsed_ms = comparison_started
                .elapsed()
                .as_millis()
                .min(u64::MAX as u128) as u64;
            let comparison_io_bytes = captured.usage().io_bytes;
            match comparison {
                super::maintenance_match::Comparison::Unchanged(report) => {
                    let (retirement_deferred, cleanup_errors) = self
                        .retire_maintenance_predecessor(writer, &predecessors, &report.snapshot);
                    return Ok(MaintenanceReport {
                        retirement_deferred,
                        cleanup_errors,
                        report,
                        input: captured.usage(),
                        build: None,
                        page_sync: None,
                        resumed,
                        abandoned_rebuild_candidates: reconstruction.abandoned.clone(),
                    });
                }
                super::maintenance_match::Comparison::Changed {
                    base,
                    paths,
                    page_only: true,
                } if !force => {
                    if let Some((report, mut page_sync)) = super::maintenance_page_delta::reconcile(
                        self,
                        writer,
                        &captured,
                        &base,
                        &paths,
                        build_limits.clone(),
                    )? {
                        page_sync.comparison_elapsed_ms = comparison_elapsed_ms;
                        page_sync.comparison_io_bytes = comparison_io_bytes;
                        let (retirement_deferred, cleanup_errors) = self
                            .retire_maintenance_predecessor(
                                writer,
                                &predecessors,
                                &report.snapshot,
                            );
                        return Ok(MaintenanceReport {
                            retirement_deferred,
                            cleanup_errors,
                            report,
                            input: captured.usage(),
                            build: None,
                            page_sync: Some(page_sync),
                            resumed,
                            abandoned_rebuild_candidates: reconstruction.abandoned.clone(),
                        });
                    }
                }
                _ => {}
            }
            input = Some(captured);
        }
        let (epoch, vector_cache_lost, vector_loss_unknown) = match previous {
            Some((selection, header)) => {
                self.operation_state()?
                    .ok_or_else(|| {
                        WikiError::new(ErrorCode::RecoveryRequired, "catalog authority disappeared")
                    })?
                    .require_publication(&crate::changes::operation_authority::Publication {
                        file_id: selection.file_id,
                        epoch: header.snapshot.generation,
                    })?;
                (
                    header.snapshot.generation.checked_add(1).ok_or_else(|| {
                        WikiError::new(ErrorCode::CapabilityUnavailable, "catalog epoch exhausted")
                    })?,
                    header.vector_cache_lost,
                    header.vector_loss_unknown,
                )
            }
            None if authority.is_some() => {
                let authority = authority.as_ref().expect("present normalized authority");
                authority.require_publication(authority.publication())?;
                (
                    authority
                        .publication()
                        .epoch
                        .checked_add(1)
                        .ok_or_else(|| {
                            WikiError::new(
                                ErrorCode::CapabilityUnavailable,
                                "catalog epoch exhausted",
                            )
                        })?,
                    false,
                    true,
                )
            }
            None => {
                // Embeddings live in a separate cache. Carry existing notices;
                // an unreadable predecessor cannot establish its loss history.
                let notices = sql::open(&self.cache_path()?, self.options.busy_timeout_ms, false)
                    .and_then(|connection| sql::cache_loss_notices(&connection))
                    .unwrap_or((false, true));
                (1, notices.0, notices.1)
            }
        };
        let input = match input {
            Some(input) => input,
            None => MaintenanceInput::capture(&self.fs, &self.vault_id, input_limits)?,
        };
        let identity = BuildIdentity {
            selection: match reconstruction.candidate.clone() {
                Some(candidate) => {
                    if candidate.creation_epoch != epoch {
                        return Err(WikiError::new(
                            ErrorCode::RecoveryRequired,
                            "cache rebuild reservation epoch changed",
                        ));
                    }
                    candidate
                }
                None => CatalogSelection::new(self.vault_id.clone(), epoch)?,
            },
            origin: None,
            vector_cache_lost,
            vector_loss_unknown,
        };
        selector::prepare(&self.fs, writer, &identity.selection)?;
        let mut build_limits = build_limits;
        build_limits.max_elapsed = build_limits.max_elapsed.min(input.remaining_time()?);
        let candidate = identity.selection.clone();
        let mut builder = NormalizedBuilder::begin(&self.fs, writer, identity, build_limits)?;
        // Cleanup is authorized only after successful exclusive builder creation;
        // a failed begin may instead report somebody else's name collision.
        let result = (|| {
            let projection = scan::project_maintenance_with_sink(&input, &mut builder)?;
            let completed = builder.finish_normalized(&projection)?;
            // The writer stays held across capture, construction, recheck and
            // publication. External edits still require this exact input recheck.
            input.final_recheck()?;
            self.guard_current(None)?;
            selector::publish(&self.fs, writer, &completed.identity.selection, timeout)?;
            let mut abandoned_rebuild_candidates =
                super::missing_cache::finish(&self.fs, writer, &self.vault_id)?;
            if abandoned_rebuild_candidates.is_empty() {
                abandoned_rebuild_candidates = reconstruction.abandoned.clone();
            }
            let (retirement_deferred, cleanup_errors) =
                self.retire_maintenance_predecessor(writer, &predecessors, &completed.snapshot);
            Ok(MaintenanceReport {
                retirement_deferred,
                cleanup_errors,
                report: SyncReport {
                    snapshot: completed.snapshot,
                    reused: false,
                    vector_cache_lost,
                    vector_loss_unknown,
                },
                input: input.usage(),
                build: Some(completed.stats),
                page_sync: None,
                resumed,
                abandoned_rebuild_candidates,
            })
        })();
        result.map_err(|mut error: WikiError| {
            if let Err(cleanup) = selector::retire_unpublished(&self.fs, writer, &self.vault_id, &candidate, Duration::ZERO) {
                error.details = serde_json::json!({"original_details":error.details,"maintenance_cleanup_error":cleanup});
            }
            error
        })
    }

    fn retire_maintenance_predecessor(
        &self,
        writer: &WriterPermit,
        predecessors: &[CatalogSelection],
        snapshot: &crate::domain::ReadSnapshot,
    ) -> (bool, Vec<WikiError>) {
        let mut deferred = false;
        let mut errors = Vec::new();
        for previous in predecessors {
            if snapshot
                .publication()
                .is_some_and(|binding| binding.file_id == previous.file_id)
            {
                continue;
            }
            match selector::retire(&self.fs, writer, &self.vault_id, previous, Duration::ZERO) {
                Ok(retired) => deferred |= !retired,
                Err(error) => {
                    deferred = true;
                    errors.push(error);
                }
            }
        }
        (deferred, errors)
    }
}
