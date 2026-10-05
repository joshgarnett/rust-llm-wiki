//! Explicit, finite, resumable local-file capture. Operational progress is not
//! canonical authority: each group uses the normal indexed change executor.
use super::{OfflineApp, source_import_state::ImportStore, source_import_types::*};
use crate::{
    catalog::{
        capture_projection::project_capture_batch,
        query_types::{QueryCatalog, QueryReadLimits},
        source_projection::RefreshProjectionLimits,
        source_refresh::{IndexedRefreshSession, NamedIndexedIntent},
    },
    changes::{
        ChangeEngine, ChangeEvent, ChangeStatus, PreparedChange, journal,
        prepare::{manifest_path, read_bounded},
        types::NamedChangeIdentity,
    },
    domain::{Blake3Hash, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath, WikiError},
    sources::{
        CaptureRequest, ExtractionInput, SourceOrigin, SourceStore,
        import_manifest::{prepare_manifest, read_manifest_batch, validate_manifest},
        import_manifest_types::{
            ImportExtraction, ImportManifestItem, MAX_IMPORT_ITEM_BYTES, ManifestPreparation,
        },
        types::CaptureAllocation,
    },
    vault::{ExpectedState, WriterPermit},
};
use serde::Serialize;
use std::{
    fs::{self, File, Metadata},
    io::Read,
    path::{Path, PathBuf},
};

const GROUP_BYTES: u64 = 4 * 1024 * 1024;
const RETAINED_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct SourceImportPreparation {
    pub path: String,
    pub manifest_hash: Blake3Hash,
    pub items: u64,
    pub input_bytes: u64,
    pub manifest_bytes: u64,
    pub preview: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct SourceImportedItem {
    pub ordinal: u64,
    pub source_id: RecordId,
    pub revision_id: RecordId,
}
#[derive(Debug, Clone, Serialize)]
pub struct SourceImportGroup {
    pub group: u64,
    pub change: PreparedChange,
    pub items: Vec<SourceImportedItem>,
}
#[derive(Debug, Clone, Serialize)]
pub struct SourceImportPendingItem {
    pub ordinal: u64,
    pub path: String,
    pub title: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct SourceImportOutcome {
    pub key: String,
    pub manifest_hash: Blake3Hash,
    pub total_items: u64,
    pub imported_items: u64,
    pub groups_committed: u64,
    pub completed: bool,
    pub preview: bool,
    pub last_group: Option<SourceImportGroup>,
    pub pending_change: Option<RecordId>,
    pub pending_group: Option<u64>,
    pub pending_items: Vec<SourceImportPendingItem>,
    /// All acknowledged groups are recorded here as bounded JSON Lines frames.
    pub results_path: VaultRelativePath,
}

pub fn prepare_source_import(
    input_list: &Path,
    output: &Path,
    dry_run: bool,
) -> Result<SourceImportPreparation> {
    let prepared = prepare_manifest(input_list, output, dry_run)?;
    Ok(SourceImportPreparation {
        path: prepared.path,
        manifest_hash: prepared.manifest_hash,
        items: prepared.items,
        input_bytes: prepared.input_bytes,
        manifest_bytes: prepared.manifest_bytes,
        preview: prepared.preview,
    })
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
fn recovery(message: &str) -> WikiError {
    WikiError::new(ErrorCode::RecoveryRequired, message)
}
fn io(error: impl std::fmt::Display) -> WikiError {
    conflict(&format!("import input: {error}"))
}
fn bounds(group_size: usize, max_groups: usize) -> Result<()> {
    if !(1..=8).contains(&group_size) || !(1..=10_000).contains(&max_groups) {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "import requires group size 1–8 and max groups 1–10000",
        ));
    }
    Ok(())
}
fn identity() -> Result<NamedChangeIdentity> {
    Ok(NamedChangeIdentity {
        change_id: RecordId::generate(RecordKind::Change)?,
        created_at: crate::sources::revision::timestamp()?,
    })
}
fn manifest_summary(value: &ManifestPreparation) -> StoredImportManifest {
    StoredImportManifest {
        hash: value.manifest_hash.clone(),
        items: value.items,
        bytes: value.manifest_bytes,
        first_item_offset: value.first_item_offset,
    }
}
fn outcome(store: &ImportStore, p: &ImportProgress, preview: bool) -> SourceImportOutcome {
    SourceImportOutcome {
        key: p.key.clone(),
        manifest_hash: p.manifest.hash.clone(),
        total_items: p.manifest.items,
        imported_items: p.next_ordinal,
        groups_committed: p.groups_committed,
        completed: p.completed,
        preview,
        last_group: p.last_group.as_ref().map(|group| SourceImportGroup {
            group: group.group,
            change: group.change.clone(),
            items: group
                .items
                .iter()
                .map(|item| SourceImportedItem {
                    ordinal: item.ordinal,
                    source_id: item.source_id.clone(),
                    revision_id: item.revision_id.clone(),
                })
                .collect(),
        }),
        pending_change: p
            .pending
            .as_ref()
            .map(|group| group.change.change_id.clone()),
        pending_group: p.pending.as_ref().map(|group| group.group),
        pending_items: p
            .pending
            .as_ref()
            .map(|group| {
                group
                    .captures
                    .iter()
                    .map(|capture| SourceImportPendingItem {
                        ordinal: capture.item.ordinal,
                        path: capture.item.path.clone(),
                        title: capture.item.title.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        results_path: store.results_path(),
    }
}
fn fresh_progress(
    app: &OfflineApp,
    store: &ImportStore,
    key: &str,
    group_size: usize,
    manifest: StoredImportManifest,
) -> Result<ImportProgress> {
    Ok(ImportProgress {
        version: IMPORT_STATE_VERSION,
        vault_id: app.vault_id.clone(),
        key: key.into(),
        group_size,
        manifest_offset: manifest.first_item_offset,
        results_hash: store.initial_results_hash(&manifest)?,
        completed: manifest.items == 0,
        manifest,
        next_ordinal: 0,
        groups_committed: 0,
        results_offset: 0,
        last_group: None,
        pending: None,
    })
}
fn save(
    store: &ImportStore,
    writer: &WriterPermit,
    p: &ImportProgress,
    hash: &mut Blake3Hash,
) -> Result<()> {
    *hash = store.save(writer, p, ExpectedState::Hash(hash.clone()))?;
    Ok(())
}

impl OfflineApp {
    pub fn source_import_status(&self, key: &str) -> Result<SourceImportOutcome> {
        let store = ImportStore::new(self.fs.clone(), self.vault_id.clone(), key)?;
        let (progress, _) = store
            .load()?
            .ok_or_else(|| conflict("import key has no saved progress"))?;
        Ok(outcome(&store, &progress, false))
    }
    pub fn source_import_run(
        &self,
        manifest: &Path,
        key: &str,
        group_size: usize,
        max_groups: usize,
    ) -> Result<SourceImportOutcome> {
        bounds(group_size, max_groups)?;
        self.import_options()?;
        let _epoch_scope =
            crate::storage::ImportEpochScope::begin(self.fs.root(), !self.options.dry_run);
        let store = ImportStore::new(self.fs.clone(), self.vault_id.clone(), key)?;
        let summary =
            validate_manifest(manifest).map_err(|error| self.import_error(&store, error))?;
        let expected = manifest_summary(&summary);
        if self.options.dry_run {
            let progress = match store.load()? {
                Some((p, _)) if p.manifest == expected && p.group_size == group_size => p,
                Some(_) => {
                    return Err(conflict(
                        "import key is already bound to different manifest or group size",
                    ));
                }
                None => fresh_progress(self, &store, key, group_size, expected)?,
            };
            self.preview_import(manifest, &progress)?;
            return Ok(outcome(&store, &progress, true));
        }
        {
            let writer = self.writer()?;
            let catalog = self.catalog();
            catalog.operation_state()?.ok_or_else(|| {
                WikiError::new(
                    ErrorCode::CapabilityUnavailable,
                    "source import requires a normalized catalog; run index rebuild --normalized",
                )
            })?;
            let existing = store.load()?;
            match existing {
                Some((p, _)) if p.manifest == expected && p.group_size == group_size => {}
                Some(_) => {
                    return Err(conflict(
                        "import key is already bound to different manifest or group size",
                    ));
                }
                None => {
                    store.initialize_directory(&writer)?;
                    let p = fresh_progress(self, &store, key, group_size, expected)?;
                    store.save(&writer, &p, ExpectedState::Absent)?;
                }
            }
            store
                .install_manifest(&writer, manifest, &summary)
                .map_err(|error| self.import_error(&store, error))?;
        }
        self.continue_import(&store, max_groups)
            .map_err(|error| self.import_error(&store, error))
    }
    pub fn source_import_resume(
        &self,
        key: &str,
        max_groups: usize,
    ) -> Result<SourceImportOutcome> {
        bounds(1, max_groups)?;
        self.import_options()?;
        let _epoch_scope =
            crate::storage::ImportEpochScope::begin(self.fs.root(), !self.options.dry_run);
        let store = ImportStore::new(self.fs.clone(), self.vault_id.clone(), key)?;
        let (progress, _) = store
            .load()?
            .ok_or_else(|| conflict("import key has no saved progress"))?;
        if self.options.dry_run {
            self.preview_import(&self.fs.root().resolve(&store.manifest_path())?, &progress)?;
            return Ok(outcome(&store, &progress, true));
        }
        self.continue_import(&store, max_groups)
            .map_err(|error| self.import_error(&store, error))
    }
    fn import_error(&self, store: &ImportStore, mut error: WikiError) -> WikiError {
        if let Ok(Some((progress, _))) = store.load() {
            let details = serde_json::to_value(outcome(store, &progress, false))
                .expect("serializable import progress");
            if !error.details.is_object() {
                error.details = serde_json::json!({"cause":error.details});
            }
            error
                .details
                .as_object_mut()
                .expect("object details")
                .insert("import".into(), details);
            let key = format!("'{}'", progress.key.replace('\'', "'\\''"));
            let missing_copy = self
                .fs
                .root()
                .resolve(&store.manifest_path())
                .ok()
                .is_some_and(|path| {
                    fs::symlink_metadata(path)
                        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                });
            if error.hint.is_none() {
                error.hint = Some(if missing_copy {
                    format!(
                        "Restore the original prepared manifest, then use source import run --manifest /path/to/original-manifest.jsonl --key {key} --group-size {}",
                        progress.group_size
                    )
                } else {
                    format!(
                        "Preserve the pending group; correct the reported input or use recover if required, then source import resume --key {key}"
                    )
                });
            }
        }
        error
    }
    fn import_options(&self) -> Result<()> {
        if self.options.stage_only {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "source import commits each group; --stage is not supported",
            ));
        }
        Ok(())
    }
    /// Preview reads only the manifest and first bounded input group. Admission
    /// and identities are resolved by run, without touching SQLite or state.
    fn preview_import(&self, path: &Path, p: &ImportProgress) -> Result<()> {
        if p.completed {
            return Ok(());
        }
        if let Some(pending) = &p.pending {
            if pending.intent.is_some() {
                return Ok(());
            }
            if let Some(reference) = &pending.input_intent {
                let store = ImportStore::new(self.fs.clone(), self.vault_id.clone(), &p.key)?;
                let expected = store.read_intent(reference)?;
                bind_input_intent(pending, &expected)?;
                let engine = self.engine()?;
                for capture in &pending.captures {
                    if retained_original(&engine, &expected, capture)?.is_none() {
                        original(&capture.item)?;
                    }
                }
                return Ok(());
            }
        }
        let items = next_items(path, p)?;
        for item in items.0 {
            original(&item)?;
        }
        Ok(())
    }
    fn continue_import(
        &self,
        store: &ImportStore,
        max_groups: usize,
    ) -> Result<SourceImportOutcome> {
        let (initial, mut initial_hash) = store
            .load()?
            .ok_or_else(|| conflict("import progress disappeared"))?;
        // Historical completion does not depend on old input or retained payloads.
        if initial.completed {
            let writer = self.writer()?;
            self.resync_import_progress(store, &writer, &initial, &mut initial_hash)?;
            return Ok(outcome(store, &initial, false));
        }
        let path = self.fs.root().resolve(&store.manifest_path())?;
        let summary = validate_manifest(&path)?;
        if manifest_summary(&summary) != initial.manifest {
            return Err(conflict(
                "owned import manifest differs from its frozen hash",
            ));
        }
        let pinned = fs::symlink_metadata(&path).map_err(io)?;
        let mut closed_attempt = false;
        let mut committed = 0;
        while committed < max_groups {
            let writer = self.writer()?;
            let (mut p, mut hash) = store
                .load()?
                .ok_or_else(|| conflict("import progress disappeared"))?;
            self.resync_import_progress(store, &writer, &p, &mut hash)?;
            if p.completed {
                return Ok(outcome(store, &p, false));
            }
            unchanged(&path, &pinned)?;
            if self.reconcile_import_tail(store, &writer, &mut p, &mut hash, &mut closed_attempt)? {
                committed += 1;
                continue;
            }
            if p.pending.is_none() {
                let (items, next_offset) = next_items(&path, &p)?;
                let captures = items
                    .into_iter()
                    .map(|item| {
                        Ok(ImportPendingCapture {
                            item,
                            allocation: CaptureAllocation {
                                source_id: RecordId::generate(RecordKind::Source)?,
                                revision_id: RecordId::generate(RecordKind::Revision)?,
                                captured_at: crate::sources::revision::timestamp()?,
                            },
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                if captures.is_empty() {
                    return Err(conflict("incomplete progress has no next manifest item"));
                }
                p.pending = Some(ImportPendingGroup {
                    group: p.groups_committed,
                    first_ordinal: p.next_ordinal,
                    next_manifest_offset: next_offset,
                    captures,
                    change: identity()?,
                    intent: None,
                    input_intent: None,
                });
                save(store, &writer, &p, &mut hash)?;
            }
            self.check_pending_manifest(&path, &p)?;
            match self.publish_import_group(store, &writer, &mut p, &mut hash)? {
                GroupPublication::Committed(change) => {
                    let result = group_result(p.pending.as_ref().expect("pending group"), change);
                    let (offset, checksum) = store.append_result(
                        &writer,
                        p.results_offset,
                        &p.results_hash,
                        ImportResultEvent::GroupCommitted {
                            result: result.clone(),
                        },
                    )?;
                    acknowledge_commit(&mut p, result, offset, checksum)?;
                    save(store, &writer, &p, &mut hash)?;
                    committed += 1;
                }
                GroupPublication::Close(reason) => {
                    if closed_attempt {
                        return Err(recovery(
                            "another stale import attempt requires an explicit resume",
                        ));
                    }
                    self.close_import_attempt(store, &writer, &mut p, &mut hash, reason)?;
                    closed_attempt = true;
                }
            }
        }
        let (p, _) = store
            .load()?
            .ok_or_else(|| conflict("import progress disappeared"))?;
        Ok(outcome(store, &p, false))
    }
    fn resync_import_progress(
        &self,
        store: &ImportStore,
        writer: &WriterPermit,
        p: &ImportProgress,
        hash: &mut Blake3Hash,
    ) -> Result<()> {
        // A prior rename/sync error can leave an observable, unacknowledged
        // header. Reestablish exact naming durability before any retention.
        store.initialize_directory(writer)?;
        save(store, writer, p, hash)?;
        if let Some(reference) = p
            .pending
            .as_ref()
            .and_then(|pending| pending.intent.as_ref())
        {
            let expected = store.read_intent(reference)?;
            bind_intent(p.pending.as_ref().expect("pending group"), &expected)?;
            if store.retain_intent(writer, &expected)? != *reference {
                return Err(conflict("known import intent changed while resyncing"));
            }
        }
        if let Some(pending) = &p.pending
            && let Some(reference) = &pending.input_intent
        {
            let expected = store.read_intent(reference)?;
            bind_input_intent(pending, &expected)?;
            if store.retain_intent(writer, &expected)? != *reference {
                return Err(conflict("input-only import anchor changed while resyncing"));
            }
        }
        Ok(())
    }
    fn check_pending_manifest(&self, path: &Path, p: &ImportProgress) -> Result<()> {
        let pending = p.pending.as_ref().expect("pending group");
        let batch = read_manifest_batch(
            path,
            p.manifest_offset,
            p.next_ordinal,
            pending.captures.len(),
        )?;
        if batch.next_offset != pending.next_manifest_offset
            || batch.items.len() != pending.captures.len()
            || batch
                .items
                .iter()
                .zip(&pending.captures)
                .any(|(item, capture)| item != &capture.item)
        {
            return Err(conflict(
                "pending captures differ from the frozen manifest cursor",
            ));
        }
        Ok(())
    }
    fn publish_import_group(
        &self,
        store: &ImportStore,
        writer: &WriterPermit,
        p: &mut ImportProgress,
        hash: &mut Blake3Hash,
    ) -> Result<GroupPublication> {
        let pending = p.pending.as_ref().expect("pending group").clone();
        let catalog = self.catalog();
        let engine = self.engine()?;
        let authority = catalog
            .operation_state()?
            .ok_or_else(|| recovery("normalized import authority is absent"))?;
        let intent = pending
            .intent
            .as_ref()
            .map(|reference| store.read_intent(reference))
            .transpose()?;
        if let Some(expected) = &intent {
            bind_intent(&pending, expected)?;
            check_control_links(&engine, expected)?;
            require_retention_order(&engine, expected)?;
            if let Some(report) =
                engine.indexed_refresh_terminal_report(writer, &expected.proof.change)?
            {
                match report.status {
                    ChangeStatus::Committed => {
                        return Ok(GroupPublication::Committed(report.change));
                    }
                    ChangeStatus::Aborted => {
                        return Ok(GroupPublication::Close(ImportAttemptCloseReason::StaleBase));
                    }
                    _ => return Err(recovery("import terminal outcome has an unexpected status")),
                }
            }
            if let Some(proof) = engine.load_indexed_refresh_proof(&expected.proof.change)? {
                if proof != expected.proof {
                    return Err(conflict(
                        "retained indexed proof differs from import intent",
                    ));
                }
                let state = journal::load_journal(
                    &self.fs,
                    &expected.manifest,
                    &expected.proof.change.manifest_hash,
                )?;
                let ever_applying = has_applying(&state);
                if authority.active().is_none() && !ever_applying {
                    catalog.guard_query()?;
                    let reader = catalog.query_snapshot(QueryReadLimits::default())?;
                    if reader.snapshot() != &expected.proof.base {
                        return Ok(GroupPublication::Close(ImportAttemptCloseReason::StaleBase));
                    }
                }
                let mut session = IndexedRefreshSession::resume(&catalog, writer, proof)?;
                let report = engine.apply_indexed_refresh(writer, &mut session)?;
                if report.status != ChangeStatus::Committed {
                    return Err(recovery("import group did not commit"));
                }
                return Ok(GroupPublication::Committed(report.change));
            }
            let state = journal::load_journal(
                &self.fs,
                &expected.manifest,
                &expected.proof.change.manifest_hash,
            )?;
            if authority.active().is_none()
                && !has_applying(&state)
                && state.status == Some(ChangeStatus::Aborted)
            {
                return Ok(GroupPublication::Close(ImportAttemptCloseReason::StaleBase));
            }
            if authority.active().is_some()
                || has_applying(&state)
                || !matches!(state.status, None | Some(ChangeStatus::Prepared))
            {
                return Err(recovery(
                    "active import attempt has lost its retained indexed proof; use recovery",
                ));
            }
        } else if authority.active().is_some() {
            return Err(recovery(
                "an active canonical operation requires completion before importing",
            ));
        }
        catalog.guard_query()?;
        let reader = catalog.query_snapshot(QueryReadLimits::default())?;
        reader.require_policy_layout()?;
        if intent
            .as_ref()
            .is_some_and(|expected| reader.snapshot() != &expected.proof.base)
        {
            return Ok(GroupPublication::Close(ImportAttemptCloseReason::StaleBase));
        }
        let store_sources = SourceStore::new(self.fs.clone());
        let inherited = pending
            .input_intent
            .as_ref()
            .map(|reference| store.read_intent(reference))
            .transpose()?;
        if let Some(expected) = &inherited {
            bind_input_intent(&pending, expected)?;
        }
        let requests = pending
            .captures
            .iter()
            .map(|capture| {
                let retained = intent
                    .as_ref()
                    .map(|expected| retained_original(&engine, expected, capture))
                    .transpose()?
                    .flatten();
                let retained = match retained {
                    Some(bytes) => Some(bytes),
                    None => inherited
                        .as_ref()
                        .map(|expected| retained_original(&engine, expected, capture))
                        .transpose()?
                        .flatten(),
                };
                let bytes = retained
                    .map(Ok)
                    .unwrap_or_else(|| original(&capture.item))?;
                store_sources.plan_capture_named(request(&capture.item, bytes), &capture.allocation)
            })
            .collect::<Result<Vec<_>>>()?;
        let projected = project_capture_batch(
            &self.fs,
            &reader,
            requests,
            &RefreshProjectionLimits::default(),
        )?;
        let sealed = IndexedRefreshSession::seal_named_write(
            &catalog,
            writer,
            projected,
            pending.change.clone(),
        )?;
        let expected = match intent {
            Some(expected) if &expected == sealed.intent() => expected,
            Some(_) => {
                return Err(conflict(
                    "reconstructed import differs from frozen preparation commitments",
                ));
            }
            None => {
                let expected = sealed.intent().clone();
                let reference = store.retain_intent(writer, &expected)?;
                p.pending.as_mut().expect("pending group").intent = Some(reference);
                save(store, writer, p, hash)?;
                expected
            }
        };
        let mut session =
            match IndexedRefreshSession::prepare_named_write(&catalog, writer, &expected, sealed) {
                Ok(session) => session,
                Err(error)
                    if error
                        .details
                        .get("named_attempt_state")
                        .and_then(serde_json::Value::as_str)
                        == Some("ambiguous_prefix") =>
                {
                    return Ok(GroupPublication::Close(
                        ImportAttemptCloseReason::AmbiguousPrefix,
                    ));
                }
                Err(error) => return Err(error),
            };
        let report = engine.apply_indexed_refresh(writer, &mut session)?;
        if report.status != ChangeStatus::Committed {
            return Err(recovery("import group did not commit"));
        }
        Ok(GroupPublication::Committed(report.change))
    }
    fn prove_import_closable(
        &self,
        pending: &ImportPendingGroup,
        expected: &NamedIndexedIntent,
    ) -> Result<Option<PreparedChange>> {
        bind_intent(pending, expected)?;
        let engine = self.engine()?;
        check_control_links(&engine, expected)?;
        require_retention_order(&engine, expected)?;
        let authority = self
            .catalog()
            .operation_state()?
            .ok_or_else(|| recovery("import authority is absent"))?;
        if authority.active().is_some() {
            return Err(recovery(
                "active reservation prevents import attempt replacement",
            ));
        }
        let state = journal::load_journal(
            &self.fs,
            &expected.manifest,
            &expected.proof.change.manifest_hash,
        )?;
        if has_applying(&state)
            || !matches!(
                state.status,
                None | Some(ChangeStatus::Prepared) | Some(ChangeStatus::Aborted)
            )
        {
            return Err(recovery(
                "applying or committed import attempt cannot be replaced",
            ));
        }
        engine.require_revision_baseline(&expected.manifest)?;
        engine.validate_named_revision_receipt(
            &expected.manifest,
            &expected.proof.change.manifest_hash,
        )?;
        engine.require_abandoned_revision_trees(&expected.manifest)?;
        if crate::changes::outcome::terminal_ever_applying(
            &self.fs,
            &expected.manifest,
            &expected.proof.change.manifest_hash,
        )? == Some(true)
        {
            return Err(recovery(
                "retained outcome proves Applying; preserve the exact attempt",
            ));
        }
        if let Some(report) = crate::changes::outcome::terminal_report(
            &self.fs,
            &expected.manifest,
            &expected.proof.change.manifest_hash,
        )? && report.status != ChangeStatus::Aborted
        {
            return Err(recovery("terminal commit cannot be replaced"));
        }
        if let Some(proof) = engine.load_indexed_refresh_proof(&expected.proof.change)?
            && proof != expected.proof
        {
            return Err(conflict("closing attempt has an unfamiliar indexed proof"));
        }
        if let Some(delta) = read_bounded(
            &self.fs,
            &crate::catalog::source_refresh::delta_path(&expected.proof.change)?,
            RETAINED_BYTES,
        )? && Blake3Hash::digest(&delta) != expected.proof.delta_hash
        {
            return Err(conflict(
                "closing attempt has unfamiliar retained delta bytes",
            ));
        }
        let mut remaining = RETAINED_BYTES;
        for (index, operation) in expected.manifest.operations.iter().enumerate() {
            let Some(reference) = &operation.after_payload else {
                continue;
            };
            self.fs
                .validate_paths(std::slice::from_ref(&reference.path))?;
            engine.require_named_single_link(&reference.path)?;
            match fs::symlink_metadata(self.fs.root().resolve(&reference.path)?) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io(error)),
                Ok(metadata) => {
                    if reference.byte_len > remaining as u64 || metadata.len() != reference.byte_len
                    {
                        return Err(conflict(
                            "closing retained payload exceeds its declared byte bound",
                        ));
                    }
                    engine.verify_payload_with_limit(
                        &expected.manifest.change_id,
                        index,
                        "proposed",
                        &operation.target,
                        (&operation.after, &operation.after_payload),
                        remaining,
                    )?;
                    remaining -= reference.byte_len as usize;
                }
            }
        }
        // Publication absence is checked on every exact declared new target,
        // not inferred from the mutable Source head alone.
        for operation in &expected.manifest.operations {
            self.fs
                .validate_paths(std::slice::from_ref(&operation.target))?;
            match fs::symlink_metadata(self.fs.root().resolve(&operation.target)?) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io(error)),
                Ok(_) => {
                    return Err(conflict(
                        "a pending capture target is already occupied; preserve the attempt",
                    ));
                }
            }
        }
        let note = manifest_path(&pending.change.change_id)?;
        if read_bounded(
            &self.fs,
            &note,
            crate::changes::prepare::MAX_MANIFEST_BYTES + 65_536,
        )?
        .is_some()
        {
            let mut remaining = RETAINED_BYTES;
            let (manifest, hash) =
                engine.load_manifest_with_budget(&pending.change.change_id, &mut remaining)?;
            if manifest != expected.manifest || hash != expected.proof.change.manifest_hash {
                return Err(conflict("known import manifest differs"));
            }
            Ok(Some(expected.proof.change.clone()))
        } else if state.status.is_some() {
            Err(recovery(
                "journaled import attempt has lost its complete manifest",
            ))
        } else {
            Ok(None)
        }
    }
    fn close_import_attempt(
        &self,
        store: &ImportStore,
        writer: &WriterPermit,
        p: &mut ImportProgress,
        hash: &mut Blake3Hash,
        reason: ImportAttemptCloseReason,
    ) -> Result<()> {
        let pending = p.pending.as_ref().expect("pending group");
        let expected = store.read_intent(
            pending
                .intent
                .as_ref()
                .ok_or_else(|| recovery("attempt closure requires its frozen intent"))?,
        )?;
        let prepared = self.prove_import_closable(pending, &expected)?;
        let input_intent = self.import_input_anchor(writer, pending, &expected)?;
        if let Some(change) = &prepared {
            self.engine()?.abort(writer, change)?;
        }
        let event = ImportResultEvent::AttemptClosed {
            group: pending.group,
            change: pending.change.clone(),
            prepared,
            reason,
        };
        let (offset, checksum) =
            store.append_result(writer, p.results_offset, &p.results_hash, event)?;
        p.results_offset = offset;
        p.results_hash = checksum;
        let pending = p.pending.as_mut().expect("pending group");
        pending.input_intent = input_intent;
        pending.change = identity()?;
        pending.intent = None;
        save(store, writer, p, hash)
    }
    /// Reconcile only one unacknowledged frame. Its canonical outcome is checked
    /// before moving the cursor; never scan earlier groups to rediscover work.
    fn reconcile_import_tail(
        &self,
        store: &ImportStore,
        writer: &WriterPermit,
        p: &mut ImportProgress,
        hash: &mut Blake3Hash,
        closed: &mut bool,
    ) -> Result<bool> {
        let tail = store.read_tail(p.results_offset, &p.results_hash)?;
        let mut committed = false;
        if tail.frame.is_some() {
            let path = store.results_path();
            self.fs.validate_paths(std::slice::from_ref(&path))?;
            self.engine()?.require_named_single_link(&path)?;
            journal::require_sync(self.fs.sync_target(&path, writer)?)?;
            let durable = store.read_tail(p.results_offset, &p.results_hash)?;
            if durable.frame != tail.frame
                || durable.next_offset != tail.next_offset
                || durable.torn_tail != tail.torn_tail
            {
                return Err(conflict("import result tail changed while syncing"));
            }
        }
        if let Some(frame) = tail.frame {
            let pending = p
                .pending
                .as_ref()
                .ok_or_else(|| conflict("unacknowledged result has no pending group"))?;
            let expected = store.read_intent(
                pending
                    .intent
                    .as_ref()
                    .ok_or_else(|| conflict("unacknowledged result has no known intent"))?,
            )?;
            bind_intent(pending, &expected)?;
            match frame.event {
                ImportResultEvent::GroupCommitted { result } => {
                    check_control_links(&self.engine()?, &expected)?;
                    let report = self
                        .engine()?
                        .indexed_refresh_terminal_report(writer, &expected.proof.change)?
                        .ok_or_else(|| {
                            recovery("import result lacks its committed terminal outcome")
                        })?;
                    if report.status != ChangeStatus::Committed
                        || result != group_result(pending, report.change)
                    {
                        return Err(conflict(
                            "import result differs from actual committed group",
                        ));
                    }
                    acknowledge_commit(p, result, tail.next_offset, frame.checksum)?;
                    committed = true;
                }
                ImportResultEvent::AttemptClosed { prepared, .. } => {
                    let actual = self.prove_import_closable(pending, &expected)?;
                    let input_intent = self.import_input_anchor(writer, pending, &expected)?;
                    if actual != prepared {
                        return Err(conflict("closed result differs from retained attempt"));
                    }
                    if let Some(change) = prepared {
                        let report = self.engine()?.abort(writer, &change)?;
                        if report.status != ChangeStatus::Aborted {
                            return Err(recovery("closed attempt is not aborted"));
                        }
                    }
                    p.results_offset = tail.next_offset;
                    p.results_hash = frame.checksum;
                    let pending = p.pending.as_mut().expect("pending group");
                    pending.input_intent = input_intent;
                    pending.change = identity()?;
                    pending.intent = None;
                    *closed = true;
                }
            }
            save(store, writer, p, hash)?;
        }
        if tail.torn_tail {
            let length = fs::symlink_metadata(self.fs.root().resolve(&store.results_path())?)
                .map_err(io)?
                .len();
            store.truncate_tail(writer, p.results_offset, length)?;
        }
        Ok(committed)
    }
    fn import_input_anchor(
        &self,
        writer: &WriterPermit,
        pending: &ImportPendingGroup,
        expected: &NamedIndexedIntent,
    ) -> Result<Option<ImportIntentRef>> {
        // Prefer a fully verified current attempt; a newer partial attempt must
        // not displace an older complete anchor. All reads are known and bounded.
        let engine = self.engine()?;
        for capture in &pending.captures {
            let Some(bytes) = retained_original(&engine, expected, capture)? else {
                return Ok(pending.input_intent.clone());
            };
            let target = VaultRelativePath::new(format!(
                "sources/{}/revisions/{}/original.bin",
                capture.allocation.source_id, capture.allocation.revision_id
            ))?;
            let operation = expected
                .manifest
                .operations
                .iter()
                .find(|op| op.target == target)
                .expect("verified original operation");
            let reference = operation.after_payload.as_ref().expect("verified payload");
            journal::require_sync(self.fs.sync_target(&reference.path, writer)?)?;
            if retained_original(&engine, expected, capture)?.as_ref() != Some(&bytes) {
                return Err(conflict(
                    "captured original changed while establishing input anchor",
                ));
            }
        }
        Ok(pending.intent.clone())
    }
}

enum GroupPublication {
    Committed(PreparedChange),
    Close(ImportAttemptCloseReason),
}
fn has_applying(state: &crate::changes::JournalState) -> bool {
    state
        .frames
        .iter()
        .any(|frame| matches!(frame.event, ChangeEvent::Applying))
}
fn group_result(pending: &ImportPendingGroup, change: PreparedChange) -> ImportGroupResult {
    ImportGroupResult {
        group: pending.group,
        change,
        items: pending
            .captures
            .iter()
            .map(|capture| ImportedItem {
                ordinal: capture.item.ordinal,
                source_id: capture.allocation.source_id.clone(),
                revision_id: capture.allocation.revision_id.clone(),
            })
            .collect(),
    }
}
fn acknowledge_commit(
    p: &mut ImportProgress,
    result: ImportGroupResult,
    offset: u64,
    checksum: Blake3Hash,
) -> Result<()> {
    let pending = p
        .pending
        .as_ref()
        .ok_or_else(|| conflict("commit result lacks a pending group"))?;
    if result != group_result(pending, result.change.clone())
        || result.change.change_id != pending.change.change_id
    {
        return Err(conflict("commit result differs from pending mapping"));
    }
    p.next_ordinal += pending.captures.len() as u64;
    p.manifest_offset = pending.next_manifest_offset;
    p.groups_committed += 1;
    p.results_offset = offset;
    p.results_hash = checksum;
    p.last_group = Some(result);
    p.pending = None;
    p.completed = p.next_ordinal == p.manifest.items;
    Ok(())
}
fn bind_intent(pending: &ImportPendingGroup, expected: &NamedIndexedIntent) -> Result<()> {
    bind_input_intent(pending, expected)?;
    if expected.manifest.change_id != pending.change.change_id
        || expected.manifest.created_at != pending.change.created_at
    {
        return Err(conflict(
            "known intent differs from pending Change identity or time",
        ));
    }
    Ok(())
}
fn bind_input_intent(pending: &ImportPendingGroup, expected: &NamedIndexedIntent) -> Result<()> {
    let mut pairs = pending
        .captures
        .iter()
        .map(
            |capture| crate::changes::indexed_refresh::IndexedCaptureTarget {
                source_id: capture.allocation.source_id.clone(),
                revision_id: capture.allocation.revision_id.clone(),
            },
        )
        .collect::<Vec<_>>();
    pairs.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    if expected.proof.operation.as_ref()
        != Some(
            &crate::changes::indexed_refresh::IndexedWriteOperation::SourceCaptureBatch {
                captures: pairs,
            },
        )
    {
        return Err(conflict(
            "known intent differs from pending identities or group membership",
        ));
    }
    for capture in &pending.captures {
        let target = format!(
            "sources/{}/revisions/{}/original.bin",
            capture.allocation.source_id, capture.allocation.revision_id
        );
        let operation = expected
            .manifest
            .operations
            .iter()
            .find(|op| op.target.as_str() == target)
            .ok_or_else(|| conflict("input anchor omits its declared original"))?;
        if operation.before != ExpectedState::Absent
            || operation.after != ExpectedState::Hash(capture.item.original_hash.clone())
            || operation.after_payload.as_ref().is_none_or(|payload| {
                payload.hash != capture.item.original_hash
                    || payload.byte_len != capture.item.byte_len
            })
        {
            return Err(conflict(
                "input anchor differs from frozen original hash or length",
            ));
        }
    }
    Ok(())
}
fn check_control_links(engine: &ChangeEngine, expected: &NamedIndexedIntent) -> Result<()> {
    for path in [
        manifest_path(&expected.manifest.change_id)?,
        journal::journal_path(&expected.manifest.change_id)?,
        crate::changes::indexed_refresh::baseline_path(&expected.proof.change)?,
        crate::catalog::source_refresh::delta_path(&expected.proof.change)?,
        VaultRelativePath::new(format!(
            "changes/{}/outcome.json",
            expected.manifest.change_id
        ))?,
        VaultRelativePath::new(format!(
            "changes/{}/revision-trees.json",
            expected.manifest.change_id
        ))?,
    ] {
        engine.fs().validate_paths(std::slice::from_ref(&path))?;
        engine.require_named_single_link(&path)?;
    }
    for reference in expected
        .manifest
        .operations
        .iter()
        .flat_map(|op| [&op.before_payload, &op.after_payload])
        .flatten()
    {
        engine.require_named_single_link(&reference.path)?;
    }
    Ok(())
}
fn require_retention_order(engine: &ChangeEngine, expected: &NamedIndexedIntent) -> Result<()> {
    let note = manifest_path(&expected.manifest.change_id)?;
    match fs::symlink_metadata(engine.fs().root().resolve(&note)?) {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io(error)),
    }
    for path in [
        journal::journal_path(&expected.manifest.change_id)?,
        crate::changes::indexed_refresh::baseline_path(&expected.proof.change)?,
        crate::catalog::source_refresh::delta_path(&expected.proof.change)?,
        VaultRelativePath::new(format!(
            "changes/{}/outcome.json",
            expected.manifest.change_id
        ))?,
        VaultRelativePath::new(format!(
            "changes/{}/revision-trees.json",
            expected.manifest.change_id
        ))?,
    ] {
        match fs::symlink_metadata(engine.fs().root().resolve(&path)?) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(error)),
            Ok(_) => {
                return Err(recovery(
                    "later retained authority exists without its named manifest; preserve the attempt",
                ));
            }
        }
    }
    Ok(())
}
fn next_items(path: &Path, p: &ImportProgress) -> Result<(Vec<ImportManifestItem>, u64)> {
    let mut items = Vec::new();
    let mut offset = p.manifest_offset;
    let mut total = 0u64;
    while items.len() < p.group_size && p.next_ordinal + (items.len() as u64) < p.manifest.items {
        let batch = read_manifest_batch(path, offset, p.next_ordinal + items.len() as u64, 1)?;
        if batch.eof {
            return Err(conflict("manifest footer precedes its frozen item count"));
        }
        let item = batch
            .items
            .into_iter()
            .next()
            .ok_or_else(|| conflict("manifest ends before acknowledged item count"))?;
        if !items.is_empty() && total + item.byte_len > GROUP_BYTES {
            break;
        }
        total += item.byte_len;
        offset = batch.next_offset;
        items.push(item);
    }
    Ok((items, offset))
}
fn request(item: &ImportManifestItem, original: Vec<u8>) -> CaptureRequest {
    CaptureRequest {
        title: item.title.clone(),
        origin_kind: SourceOrigin::LocalFile,
        origin: item.path.clone(),
        original,
        media_type: item.media_type.clone(),
        extraction: match item.extraction {
            ImportExtraction::Utf8Preserve => ExtractionInput::Utf8Preserve,
            ImportExtraction::Unsupported => ExtractionInput::Unsupported {
                extractor: "unsupported-local-format-v1".into(),
                fingerprint: Blake3Hash::digest(b"unsupported-local-format-v1"),
            },
        },
    }
}
fn original(item: &ImportManifestItem) -> Result<Vec<u8>> {
    original_bytes(item).map_err(|mut error| {
        if !error.details.is_object() { error.details = serde_json::json!({"cause":error.details}); }
        error.details.as_object_mut().expect("object details").insert("import_item".into(), serde_json::json!({"ordinal":item.ordinal,"path":item.path,"original_hash":item.original_hash}));
        error
    })
}
fn original_bytes(item: &ImportManifestItem) -> Result<Vec<u8>> {
    let path = Path::new(&item.path);
    let before = fs::symlink_metadata(path).map_err(io)?;
    if !before.is_file()
        || before.file_type().is_symlink()
        || before.len() != item.byte_len
        || before.len() > MAX_IMPORT_ITEM_BYTES
    {
        return Err(conflict("import original type or length changed"));
    }
    // Parent symlinks are also refused: frozen paths name actual regular files.
    let canonical = fs::canonicalize(path).map_err(io)?;
    if canonical != PathBuf::from(path) {
        return Err(conflict("import original path changed or uses an alias"));
    }
    let mut file = File::open(path).map_err(io)?;
    let opened = file.metadata().map_err(io)?;
    if !same_identity(&before, &opened) {
        return Err(conflict("import original identity changed"));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(item.byte_len + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    unchanged(path, &before)?;
    if bytes.len() as u64 != item.byte_len || Blake3Hash::digest(&bytes) != item.original_hash {
        return Err(conflict("import original bytes differ from manifest"));
    }
    if matches!(item.extraction, ImportExtraction::Utf8Preserve)
        && std::str::from_utf8(&bytes).is_err()
    {
        return Err(conflict("frozen text extraction is no longer UTF-8"));
    }
    Ok(bytes)
}
fn same_identity(a: &Metadata, b: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}
fn unchanged(path: &Path, before: &Metadata) -> Result<()> {
    let after = fs::symlink_metadata(path).map_err(io)?;
    if !after.is_file()
        || after.file_type().is_symlink()
        || !same_identity(before, &after)
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(conflict("import pinned file changed"));
    }
    Ok(())
}
fn retained_original(
    engine: &ChangeEngine,
    expected: &NamedIndexedIntent,
    capture: &ImportPendingCapture,
) -> Result<Option<Vec<u8>>> {
    let target = VaultRelativePath::new(format!(
        "sources/{}/revisions/{}/original.bin",
        capture.allocation.source_id, capture.allocation.revision_id
    ))?;
    let (index, operation) = expected
        .manifest
        .operations
        .iter()
        .enumerate()
        .find(|(_, operation)| operation.target == target)
        .ok_or_else(|| conflict("import intent omits its original payload"))?;
    let reference = operation
        .after_payload
        .as_ref()
        .ok_or_else(|| conflict("original intent has no retained payload"))?;
    engine.require_named_single_link(&reference.path)?;
    match fs::symlink_metadata(engine.fs().root().resolve(&reference.path)?) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io(error)),
        Ok(_) => {}
    }
    // Missing external files are irrelevant once the exact retained original
    // exists. The normal payload verifier checks its layout, hash and length.
    let bytes = engine
        .verify_payload_with_limit(
            &expected.manifest.change_id,
            index,
            "proposed",
            &target,
            (&operation.after, &operation.after_payload),
            MAX_IMPORT_ITEM_BYTES as usize,
        )?
        .ok_or_else(|| conflict("retained original disappeared"))?;
    if bytes.len() as u64 != capture.item.byte_len
        || Blake3Hash::digest(&bytes) != capture.item.original_hash
    {
        return Err(conflict(
            "retained original differs from frozen import item",
        ));
    }
    Ok(Some(bytes))
}
