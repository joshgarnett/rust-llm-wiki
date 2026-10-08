//! Closed external authored Page reconciliation inside ordinary index sync.
//! Canonical bytes remain the authority; the candidate uses rebuild publication.
use super::{
    Catalog, SyncReport,
    file_types::{BuildIdentity, CatalogSelection},
    maintenance_input::MaintenanceInput,
    normalized_build::{self, BuildLimits},
    normalized_delta::DeltaStats,
    query_types::{QueryCatalog, QueryReadLimits},
    selector,
    source_projection::RefreshProjectionLimits,
    write_projection,
};
use crate::{
    domain::{ErrorCode, ReadSnapshot, RecordId, RecordKind, Result, VaultRelativePath, WikiError},
    vault::{ExpectedState, WriterPermit},
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct PageSyncStats {
    pub comparison_elapsed_ms: u64,
    pub comparison_io_bytes: u64,
    pub final_recheck_io_bytes: u64,
    pub candidate_seal_elapsed_ms: u64,
    pub pages_created: usize,
    pub pages_edited: usize,
    pub pages_deleted: usize,
    pub copy_pages: u64,
    pub copy_bytes: u64,
    pub copy_elapsed_ms: u64,
    pub projection_elapsed_ms: u64,
    pub final_recheck_elapsed_ms: u64,
    pub publication_elapsed_ms: u64,
    pub delta: DeltaStats,
}

pub(super) fn reconcile(
    catalog: &Catalog,
    writer: &WriterPermit,
    input: &MaintenanceInput,
    base: &ReadSnapshot,
    paths: &BTreeMap<VaultRelativePath, (ExpectedState, ExpectedState)>,
    mut limits: BuildLimits,
) -> Result<Option<(SyncReport, PageSyncStats)>> {
    let projection_started = Instant::now();
    if paths.is_empty() || paths.len() > 16 {
        return Ok(None);
    }
    let reader = catalog.cached_query_snapshot(QueryReadLimits::default())?;
    if reader.snapshot() != base {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "Page reconciliation selected publication changed",
        ));
    }
    if let Err(error) = reader.require_policy_layout() {
        return if matches!(
            error.code,
            ErrorCode::IndexCorrupt
                | ErrorCode::OfflineUnavailable
                | ErrorCode::CapabilityUnavailable
        ) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let mut changes = Vec::new();
    let mut bytes = 0usize;
    let mut stats = PageSyncStats::default();
    let mut identities = BTreeMap::<RecordId, VaultRelativePath>::new();
    for (path, (before, after)) in paths {
        input.require_clean()?;
        let Some(old) = admit_cache(reader.document(path))? else {
            return Ok(None);
        };
        let new = input.notes().get(path).cloned();
        match (before, &old) {
            (ExpectedState::Absent, None) => {}
            (ExpectedState::Hash(hash), Some(document))
                if document.hash == *hash
                    && document.kind == Some(RecordKind::Page)
                    && document.source_id.is_none()
                    && document.owner_revision.is_none() =>
            {
                bytes = bytes.saturating_add(document.raw_text.len());
                if bytes > 16 * 1024 * 1024 {
                    return Ok(None);
                }
                let note = crate::records::parse_note(document.raw_text.as_bytes());
                let Some(record) = note
                    .canonical
                    .as_ref()
                    .filter(|r| r.kind() == RecordKind::Page)
                else {
                    return Ok(None);
                };
                if note.source_hash != *hash || document.record_id.as_ref() != Some(record.id()) {
                    return Ok(None);
                }
                let Some(Some(row)) = admit_cache(reader.record(record.id()))? else {
                    return Ok(None);
                };
                let Some(claim) = admit_cache(reader.unique_identity_claim(record.id()))? else {
                    return Ok(None);
                };
                if super::row_projection::canonical_document(path, &note, Some(&row)) != *document {
                    return Ok(None);
                }
                if row.path != *path
                    || row.hash != *hash
                    || row.record != *record
                    || claim.as_ref().is_none_or(|c| {
                        c.path != *path || c.hash != *hash || c.kind != Some(RecordKind::Page)
                    })
                {
                    return Ok(None);
                }
            }
            _ => return Ok(None),
        }
        match (after, &new) {
            (ExpectedState::Absent, None) => {}
            (ExpectedState::Hash(hash), Some(note)) if note.source_hash == *hash => {
                bytes = bytes.saturating_add(note.raw.len());
                if bytes > 16 * 1024 * 1024 {
                    return Ok(None);
                }
                let Some(record) = note
                    .canonical
                    .as_ref()
                    .filter(|r| r.kind() == RecordKind::Page)
                else {
                    return Ok(None);
                };
                if old
                    .as_ref()
                    .is_some_and(|d| d.record_id.as_ref() != Some(record.id()))
                {
                    return Ok(None);
                }
                if old.is_none() {
                    let Some(claim) = admit_cache(reader.unique_identity_claim(record.id()))?
                    else {
                        return Ok(None);
                    };
                    let Some(row) = admit_cache(reader.record(record.id()))? else {
                        return Ok(None);
                    };
                    if claim.is_some() || row.is_some() {
                        return Ok(None);
                    }
                }
            }
            _ => return Ok(None),
        }
        let id = new
            .as_ref()
            .and_then(|n| n.canonical.as_ref())
            .map(|r| r.id().clone())
            .or_else(|| old.as_ref().and_then(|d| d.record_id.clone()))
            .expect("authenticated before or after Page");
        if identities.insert(id, path.clone()).is_some() {
            return Ok(None);
        }
        match (&old, &new) {
            (None, Some(_)) => stats.pages_created += 1,
            (Some(_), None) => stats.pages_deleted += 1,
            (Some(_), Some(_)) => stats.pages_edited += 1,
            _ => return Ok(None),
        }
        changes.push((path.clone(), old, new));
    }
    // Changed identities must have one exact current owner, or no owner for a
    // deletion. Readable malformed claims count too, just as reconstruction.
    let mut current = BTreeMap::<RecordId, Vec<VaultRelativePath>>::new();
    for (path, note) in input.notes().iter() {
        input.require_clean()?;
        for id in crate::sources::identity::readable_ids(note) {
            if identities.contains_key(&id) {
                current.entry(id).or_default().push(path.clone());
            }
        }
    }
    for (path, old, new) in &changes {
        let id = new
            .as_ref()
            .and_then(|n| n.canonical.as_ref())
            .map(|r| r.id())
            .or_else(|| old.as_ref().and_then(|d| d.record_id.as_ref()))
            .expect("authenticated Page");
        let owners = current.get(id).map(Vec::as_slice).unwrap_or(&[]);
        if (new.is_some() && owners != [path.clone()]) || (new.is_none() && !owners.is_empty()) {
            return Ok(None);
        }
    }
    let delta = match write_projection::project_external_pages(
        catalog.fs(),
        &reader,
        &changes,
        &RefreshProjectionLimits::default(),
    ) {
        Ok(delta) => delta,
        Err(error)
            if matches!(
                error.code,
                ErrorCode::BudgetExceeded
                    | ErrorCode::IndexCorrupt
                    | ErrorCode::ReferenceAmbiguous
                    | ErrorCode::ContentConflict
                    | ErrorCode::RecordInvalid
                    | ErrorCode::CapabilityUnavailable
                    | ErrorCode::OfflineUnavailable
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    stats.projection_elapsed_ms = millis(projection_started);
    let header = selector::maintenance_header(
        catalog.fs(),
        catalog.vault_id(),
        Duration::from_millis(catalog.options.busy_timeout_ms),
    )?
    .ok_or_else(|| {
        WikiError::new(
            ErrorCode::RecoveryRequired,
            "Page reconciliation predecessor disappeared",
        )
    })?
    .1;
    if header.snapshot != *base {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "Page reconciliation predecessor changed",
        ));
    }
    let epoch = base.generation.checked_add(1).ok_or_else(|| {
        WikiError::new(ErrorCode::CapabilityUnavailable, "catalog epoch exhausted")
    })?;
    let identity = BuildIdentity {
        selection: CatalogSelection::new(catalog.vault_id().clone(), epoch)?,
        origin: None,
        vector_cache_lost: header.vector_cache_lost,
        vector_loss_unknown: header.vector_loss_unknown,
    };
    selector::prepare(catalog.fs(), writer, &identity.selection)?;
    limits.max_elapsed = limits.max_elapsed.min(input.remaining_time()?);
    let commitments: Vec<_> = paths
        .iter()
        .map(|(path, (before, after))| (path.clone(), before.clone(), after.clone()))
        .collect();
    let candidate = identity.selection.clone();
    let mut candidate_owned = false;
    let result = (|| {
        let copied = normalized_build::copy_selected_and_apply(
            catalog.fs(),
            writer,
            identity,
            reader.connection(),
            base,
            &delta,
            &commitments,
            limits,
        );
        let (completed, copy) = match copied {
            Ok(copied) => copied,
            Err(error)
                if error.code == ErrorCode::BudgetExceeded
                    && error.details.get("maintenance_cleanup_error").is_none() =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        candidate_owned = true;
        stats.copy_pages = copy.pages;
        stats.copy_bytes = copy.bytes;
        stats.copy_elapsed_ms = copy.elapsed_ms;
        stats.delta = copy.delta;
        stats.projection_elapsed_ms = stats
            .projection_elapsed_ms
            .saturating_add(copy.delta_elapsed_ms);
        stats.candidate_seal_elapsed_ms = copy.seal_elapsed_ms;
        // Release our copy source before retirement; independently held readers
        // keep the predecessor file coherent and may defer its deletion.
        drop(reader);
        let started = Instant::now();
        let before = input.usage().io_bytes;
        input.final_recheck()?;
        catalog.guard_current(None)?;
        stats.final_recheck_elapsed_ms = millis(started);
        stats.final_recheck_io_bytes = input.usage().io_bytes.saturating_sub(before);
        let started = Instant::now();
        selector::publish(
            catalog.fs(),
            writer,
            &completed.identity.selection,
            Duration::from_millis(catalog.options.busy_timeout_ms),
        )?;
        stats.publication_elapsed_ms = millis(started);
        Ok(Some((
            SyncReport {
                snapshot: completed.snapshot,
                reused: false,
                vector_cache_lost: header.vector_cache_lost,
                vector_loss_unknown: header.vector_loss_unknown,
            },
            stats,
        )))
    })();
    result.map_err(|mut error:WikiError| {
        if candidate_owned && let Err(cleanup)=selector::retire_unpublished(catalog.fs(),writer,catalog.vault_id(),&candidate,Duration::ZERO) { error.details=serde_json::json!({"original_details":error.details,"maintenance_cleanup_error":cleanup}); }
        error
    })
}

/// These cache-only admission failures cannot certify a selected Page delta.
/// Rebuild from the captured canonical input; authority/freshness/I/O failures
/// still propagate and never become successful fallback admission.
fn admit_cache<T>(result: Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error)
            if matches!(
                error.code,
                ErrorCode::IndexCorrupt | ErrorCode::BudgetExceeded | ErrorCode::ReferenceAmbiguous
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn millis(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

#[cfg(test)]
#[path = "maintenance_page_delta_tests.rs"]
mod tests;
