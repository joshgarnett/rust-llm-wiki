//! Explicit selected Source discovery. Bodies are discarded after each real task
//! is authenticated; planning never opens provider credentials or writes caches.
use super::{
    OfflineApp,
    remote::RemoteRuntime,
    source_relation_inputs::{
        InputLimits, MAX_CAPTURED_BYTES, MAX_CONTENT_BYTES, MAX_CONTROL_BYTES,
    },
    source_relation_planning::plan_captured,
    source_relation_preparation::recipe_hash,
    source_relation_preparation_types::*,
    source_relation_quote::{QuoteBuilder, SourceRelationQuote},
};
use crate::{
    catalog::query_types::{QueryCatalog, QueryReadLimits},
    changes::prepare::read_bounded,
    domain::*,
    jobs,
    records::parse_note,
    sources::{RefreshRecord, SourceRefreshLookup, selected::SelectedSourceBinding},
    vault::VaultFs,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};
const TOTAL_CAPTURE_BYTES: usize = 64 * 1024 * 1024;
pub(crate) struct PreparedSourceRelations {
    pub manifest: Vec<u8>,
    pub quote: Option<SourceRelationQuote>,
}
fn check(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "Source manifest planning deadline exceeded",
        ));
    }
    Ok(())
}
fn read(
    fs: &VaultFs,
    path: &VaultRelativePath,
    limit: usize,
    remaining: &mut usize,
    deadline: Instant,
) -> Result<Vec<u8>> {
    check(deadline)?;
    let bytes = read_bounded(fs, path, limit.min(*remaining))?.ok_or_else(|| {
        WikiError::new(
            ErrorCode::SourceIntegrity,
            format!("selected Source capture missing: {path}"),
        )
    })?;
    *remaining = remaining.checked_sub(bytes.len()).ok_or_else(|| {
        WikiError::new(
            ErrorCode::BudgetExceeded,
            "Source manifest captured bytes exceed64MiB",
        )
    })?;
    check(deadline)?;
    Ok(bytes)
}
fn selected_record(
    fs: &VaultFs,
    selected: &RefreshRecord,
    kind: RecordKind,
    captured: &mut BTreeMap<VaultRelativePath, Vec<u8>>,
    total: &mut usize,
    local: &mut usize,
    deadline: Instant,
) -> Result<CanonicalRecord> {
    let bytes = read(
        fs,
        &selected.path,
        MAX_CONTROL_BYTES.min(*local),
        total,
        deadline,
    )?;
    *local -= bytes.len();
    let record = parse_note(&bytes).canonical.ok_or_else(|| {
        WikiError::new(
            ErrorCode::SourceIntegrity,
            "selected Source control record is malformed",
        )
    })?;
    if record != selected.record
        || record.kind() != kind
        || Blake3Hash::digest(&bytes) != selected.hash
    {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "selected Source control bytes differ from catalog",
        ));
    }
    captured.insert(selected.path.clone(), bytes);
    check(deadline)?;
    Ok(record)
}
fn capture_source(
    fs: &VaultFs,
    lookup: &impl SourceRefreshLookup,
    vault: &RecordId,
    id: &RecordId,
    total: &mut usize,
    deadline: Instant,
) -> Result<(SelectedSourceBinding, BTreeMap<VaultRelativePath, Vec<u8>>)> {
    check(deadline)?;
    if lookup.vault_id() != vault {
        return Err(WikiError::new(
            ErrorCode::SourceIntegrity,
            "selected Source catalog belongs to a different vault",
        ));
    }
    let source = lookup.unique_record(id)?.ok_or_else(|| {
        WikiError::new(
            ErrorCode::RecordNotFound,
            "requested Source is absent from selected catalog",
        )
    })?;
    let mut captured = BTreeMap::new();
    let mut local = MAX_CAPTURED_BYTES;
    let record = selected_record(
        fs,
        &source,
        RecordKind::Source,
        &mut captured,
        total,
        &mut local,
        deadline,
    )?;
    if record.id() != id || record.string("wiki_status") != Some("active") {
        return Err(WikiError::new(
            ErrorCode::SourceIntegrity,
            "requested Source is not active",
        ));
    }
    let revision_id = RecordId::new(record.string("wiki_current_revision").ok_or_else(|| {
        WikiError::new(ErrorCode::SourceIntegrity, "Source has no current revision")
    })?)?;
    let revision = lookup.unique_record(&revision_id)?.ok_or_else(|| {
        WikiError::new(
            ErrorCode::SourceIntegrity,
            "current Source Revision is absent",
        )
    })?;
    let record = selected_record(
        fs,
        &revision,
        RecordKind::Revision,
        &mut captured,
        total,
        &mut local,
        deadline,
    )?;
    let parent = revision
        .path
        .as_str()
        .rsplit_once('/')
        .map(|p| p.0)
        .ok_or_else(|| WikiError::new(ErrorCode::SourceIntegrity, "Revision has no parent"))?;
    let path = |field: &str| -> Result<VaultRelativePath> {
        let relative = record.string(field).ok_or_else(|| {
            WikiError::new(
                ErrorCode::SourceIntegrity,
                "Revision lacks complete payload path",
            )
        })?;
        VaultRelativePath::new(format!("{parent}/{relative}"))
    };
    let content_path = path("wiki_content_path")?;
    let original_path = path("wiki_original_path")?;
    for (path, limit) in [
        (VaultRelativePath::new("WIKI.md")?, MAX_CONTROL_BYTES),
        (content_path.clone(), MAX_CONTENT_BYTES),
        (original_path, MAX_CAPTURED_BYTES),
    ] {
        if captured.contains_key(&path) {
            continue;
        }
        let bytes = read(fs, &path, limit.min(local), total, deadline)?;
        local -= bytes.len();
        captured.insert(path, bytes);
    }
    let binding = SelectedSourceBinding {
        vault_id: vault.clone(),
        source_id: id.clone(),
        revision_id,
        source_path: source.path,
        revision_path: revision.path,
        content_path,
        source_hash: source.hash,
        revision_hash: revision.hash,
        content_hash: Blake3Hash::new(record.string("wiki_content_hash").ok_or_else(|| {
            WikiError::new(
                ErrorCode::SourceIntegrity,
                "Revision lacks complete content hash",
            )
        })?)?,
    };
    Ok((binding, captured))
}
fn unique_ids(
    source_ids: &[RecordId],
    max_output_tokens: u64,
    deadline: Instant,
) -> Result<BTreeSet<RecordId>> {
    check(deadline)?;
    if source_ids.is_empty() || source_ids.len() > jobs::RUN_MAX_TASKS || max_output_tokens == 0 {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "Source planning requires1..4096 IDs and positive output tokens",
        ));
    }
    let ids: BTreeSet<_> = source_ids.iter().cloned().collect();
    if ids.len() != source_ids.len() {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "Source planning IDs must be unique",
        ));
    }
    Ok(ids)
}
fn verify_guards(
    fs: &VaultFs,
    guards: BTreeMap<VaultRelativePath, (Blake3Hash, usize)>,
    remaining: &mut usize,
    deadline: Instant,
) -> Result<()> {
    for (path, (expected, len)) in guards {
        let actual = read(fs, &path, len, remaining, deadline)?;
        if Blake3Hash::digest(actual) != expected {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "selected Source changed during manifest planning",
            ));
        }
    }
    check(deadline)
}
impl OfflineApp {
    #[cfg(test)]
    pub(crate) fn prepare_source_relation_manifest(
        &self,
        source_ids: &[RecordId],
        runtime: &RemoteRuntime,
        max_output_tokens: u64,
        deadline: Instant,
    ) -> Result<Vec<u8>> {
        self.prepare_source_relations(source_ids, runtime, max_output_tokens, false, deadline)
            .map(|prepared| prepared.manifest)
    }

    pub(crate) fn prepare_source_relations(
        &self,
        source_ids: &[RecordId],
        runtime: &RemoteRuntime,
        max_output_tokens: u64,
        quote: bool,
        deadline: Instant,
    ) -> Result<PreparedSourceRelations> {
        let ids = unique_ids(source_ids, max_output_tokens, deadline)?;
        let mut quoting = quote
            .then(|| QuoteBuilder::new(runtime, deadline))
            .transpose()?;
        let catalog = self.catalog();
        if catalog.operation_state()?.is_none() {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "Source relation preparation requires normalized catalog; rebuild with --normalized",
            ));
        }
        catalog.guard_query()?;
        check(deadline)?;
        let mut remaining = TOTAL_CAPTURE_BYTES;
        let mut entries = Vec::with_capacity(ids.len());
        let mut guards = BTreeMap::new();
        let mut keys = BTreeSet::new();
        for id in ids {
            check(deadline)?;
            // A selected reader per Source keeps the existing4096-row ceiling
            // independent of the caller's finite4096-Source count.
            let reader = catalog.cached_query_snapshot(QueryReadLimits {
                max_rows: 16,
                max_row_bytes: MAX_CONTROL_BYTES,
                max_bytes: 4 * MAX_CONTROL_BYTES,
                max_elapsed_ms: deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .clamp(1, 30_000) as u64,
                max_vm_steps: 100_000,
            })?;
            let (source, captured) = capture_source(
                &self.fs,
                &reader,
                &self.vault_id,
                &id,
                &mut remaining,
                deadline,
            )?;
            let plan = plan_captured(
                &self.fs,
                &source,
                &captured,
                &runtime.service,
                max_output_tokens,
                InputLimits {
                    max_captured_bytes: MAX_CAPTURED_BYTES as u64,
                    max_hash_bytes: 14 * MAX_CAPTURED_BYTES as u64,
                    deadline,
                },
            )?;
            if !keys.insert(plan.generation.task.key.clone()) {
                return Err(WikiError::new(
                    ErrorCode::SourceIntegrity,
                    "selected Source task keys are duplicate",
                ));
            }
            for (path, bytes) in &captured {
                let guard = (Blake3Hash::digest(bytes), bytes.len());
                if guards
                    .insert(path.clone(), guard.clone())
                    .is_some_and(|old| old != guard)
                {
                    return Err(WikiError::new(
                        ErrorCode::FreshnessConflict,
                        "Source captures disagree on shared dependencies",
                    ));
                }
            }
            reader.check_query_budget()?;
            reader.verify_operations(&catalog)?;
            if let Some(quoting) = &mut quoting {
                quoting.push(&source.source_id, &plan.generation, runtime, deadline)?;
            }
            entries.push(SourceRelationManifestEntry {
                source,
                task: plan.generation.task,
            });
            check(deadline)?;
        }
        let manifest = SourceRelationManifest {
            version: 1,
            vault_id: self.vault_id.clone(),
            profile_id: runtime.service.summary().profile_id,
            config_fingerprint: runtime.service.summary().config_fingerprint,
            recipe_hash: recipe_hash()?,
            max_output_tokens,
            entries,
        };
        let bytes = jobs::events::bounded_json(&manifest, MANIFEST_MAX_BYTES)?;
        // Final currentness is Source-local. This recheck shares the64MiB
        // cumulative read allowance and never scans unrelated canonical bodies.
        verify_guards(&self.fs, guards, &mut remaining, deadline)?;
        catalog.guard_query()?;
        let quote = if let Some(quoting) = quoting {
            runtime.service.recheck(&self.fs)?;
            let quote = quoting.finish(runtime, Blake3Hash::digest(&bytes), deadline)?;
            runtime.service.recheck(&self.fs)?;
            Some(quote)
        } else {
            None
        };
        check(deadline)?;
        Ok(PreparedSourceRelations {
            manifest: bytes,
            quote,
        })
    }
}
#[cfg(test)]
#[path = "source_relation_prepare_tests.rs"]
mod tests;
