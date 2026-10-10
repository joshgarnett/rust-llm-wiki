//! First selected admission of one never-started legacy Page inverse.
//! This is a child of source_refresh; no caller-provided rows gain authority.
use super::*;
use crate::{
    changes::{
        ChangeEvent, ChangeManifest, OperationRole,
        indexed_refresh::{LegacyPageAdmission, MAX_LEGACY_PAGE_ENVELOPE_BYTES, baseline_path},
        outcome,
    },
    records::parse_note,
};

const PAGE_BYTES: usize = 16 * 1024 * 1024;
const LINEAGE_BYTES: usize = 4 * PAGE_BYTES;
const ENVELOPE_BYTES: usize = MAX_LEGACY_PAGE_ENVELOPE_BYTES;

struct ValidatedLegacyPageAdmission {
    manifest: ChangeManifest,
    binding: LegacyPageAdmission,
    page_id: RecordId,
    proposed: Vec<u8>,
}

fn replacement_shape(manifest: &ChangeManifest) -> Result<()> {
    if manifest.operations.len() != 1
        || manifest.origin.is_some()
        || !manifest.allocated_ids.is_empty()
    {
        return Err(recovery(
            "legacy Page admission requires one ordinary replacement",
        ));
    }
    let op = &manifest.operations[0];
    if op.role != OperationRole::MutableRecord
        || !op.apply_after.is_empty()
        || !crate::sources::revision::canonical_path(&op.target)
        || !matches!(op.before, ExpectedState::Hash(_))
        || !matches!(op.after, ExpectedState::Hash(_))
        || op.before == op.after
        || [&op.before_payload, &op.after_payload]
            .into_iter()
            .any(|payload| {
                payload
                    .as_ref()
                    .is_none_or(|p| p.byte_len > PAGE_BYTES as u64)
            })
    {
        return Err(recovery(
            "legacy Page admission excludes creation, deletion, assets and ordering",
        ));
    }
    Ok(())
}

fn payload_pair(
    engine: &ChangeEngine,
    manifest: &ChangeManifest,
    remaining: &mut usize,
) -> Result<[Vec<u8>; 2]> {
    let op = &manifest.operations[0];
    let mut payload = |side, expected, reference| -> Result<Vec<u8>> {
        let bytes = engine
            .verify_payload_with_limit(
                &manifest.change_id,
                0,
                side,
                &op.target,
                (expected, reference),
                (*remaining).min(PAGE_BYTES),
            )?
            .ok_or_else(|| recovery("legacy Page admission requires both retained payloads"))?;
        *remaining = remaining
            .checked_sub(bytes.len())
            .ok_or_else(|| budget("legacy Page lineage exceeds byte ceiling"))?;
        Ok(bytes)
    };
    Ok([
        payload("before", &op.before, &op.before_payload)?,
        payload("proposed", &op.after, &op.after_payload)?,
    ])
}

/// Validate only retained history here. Current before/mixed/after state belongs
/// to the existing session checkpoints, including publication recovery.
fn retained_lineage(
    catalog: &Catalog,
    engine: &ChangeEngine,
    change: &PreparedChange,
) -> Result<ValidatedLegacyPageAdmission> {
    engine.require_binding()?;
    if engine.vault_id() != &catalog.vault_id {
        return Err(recovery("legacy Page admission belongs to another vault"));
    }
    let (manifest, hash) = engine.load_manifest_structure(&change.change_id)?;
    if hash != change.manifest_hash || !manifest.read_preconditions.is_empty() {
        return Err(recovery(
            "legacy Page admission requires exact identity and no original read guards",
        ));
    }
    replacement_shape(&manifest)?;
    let parent_id = manifest
        .inverse_of
        .as_ref()
        .ok_or_else(|| recovery("legacy Page admission requires inverse ancestry"))?;
    let (parent, parent_hash) = engine.load_manifest_structure(parent_id)?;
    replacement_shape(&parent)?;
    if parent.inverse_of.is_some() {
        return Err(recovery(
            "legacy Page admission requires an ordinary parent",
        ));
    }
    let committed = match outcome::terminal_report(engine.fs(), &parent, &parent_hash)? {
        Some(report) => report.status == ChangeStatus::Committed,
        None => {
            journal::load_journal(engine.fs(), &parent, &parent_hash)?.status
                == Some(ChangeStatus::Committed)
        }
    };
    if !committed {
        return Err(recovery(
            "legacy Page admission requires authenticated committed parent",
        ));
    }
    let op = &manifest.operations[0];
    let original = &parent.operations[0];
    if op.target != original.target || op.before != original.after || op.after != original.before {
        return Err(recovery(
            "legacy Page inverse differs from exact parent reversal",
        ));
    }
    let mut remaining = LINEAGE_BYTES;
    let inverse = payload_pair(engine, &manifest, &mut remaining)?;
    let parent_bytes = payload_pair(engine, &parent, &mut remaining)?;
    if inverse[0] != parent_bytes[1] || inverse[1] != parent_bytes[0] {
        return Err(recovery(
            "legacy Page retained payloads differ from parent reversal",
        ));
    }
    let before = parse_note(&inverse[0]);
    let after = parse_note(&inverse[1]);
    let (before_record, after_record) = before
        .canonical
        .as_ref()
        .zip(after.canonical.as_ref())
        .filter(|(before, after)| {
            before.kind() == RecordKind::Page
                && after.kind() == RecordKind::Page
                && before.id() == after.id()
        })
        .ok_or_else(|| recovery("legacy Page inverse changes record identity or kind"))?;
    let page_id = before_record.id().clone();
    let _ = after_record;
    let state = journal::load_journal(engine.fs(), &manifest, &hash)?;
    let first = state
        .frames
        .first()
        .filter(|frame| matches!(frame.event, ChangeEvent::Prepared))
        .ok_or_else(|| {
            recovery("legacy Page admission requires original Prepared journal prefix")
        })?;
    let binding = LegacyPageAdmission {
        parent: PreparedChange {
            change_id: parent.change_id,
            manifest_hash: parent_hash,
        },
        prepared_journal_hash: Blake3Hash::digest(journal::encode_frame(first)?),
    };
    let [_, proposed] = inverse;
    Ok(ValidatedLegacyPageAdmission {
        manifest,
        binding,
        page_id,
        proposed,
    })
}

fn require_never_started(
    catalog: &Catalog,
    engine: &ChangeEngine,
    admission: &ValidatedLegacyPageAdmission,
    change: &PreparedChange,
) -> Result<()> {
    let state = journal::load_journal(engine.fs(), &admission.manifest, &change.manifest_hash)?;
    if state.torn_tail
        || state.frames.len() != 1
        || state.status != Some(ChangeStatus::Prepared)
        || outcome::terminal_report(engine.fs(), &admission.manifest, &change.manifest_hash)?
            .is_some()
    {
        return Err(recovery(
            "legacy Page admission requires never-started Prepared history",
        ));
    }
    let frame = journal::encode_frame(&state.frames[0])?;
    let raw = read_bounded(
        engine.fs(),
        &journal::journal_path(&change.change_id)?,
        ENVELOPE_BYTES,
    )?
    .ok_or_else(|| recovery("legacy Prepared journal disappeared"))?;
    if raw != frame || Blake3Hash::digest(&raw) != admission.binding.prepared_journal_hash {
        return Err(recovery("legacy Prepared journal prefix changed"));
    }
    for path in [baseline_path(change)?, delta_path(change)?] {
        if read_bounded(engine.fs(), &path, ENVELOPE_BYTES)?.is_some() {
            return Err(recovery(
                "legacy Page admission refuses an existing indexed receipt or delta",
            ));
        }
    }
    let authority = operation_authority::load(engine.fs(), &catalog.vault_id, Presence::Required)?
        .ok_or_else(|| recovery("legacy Page admission requires operation authority"))?;
    if authority.active().is_some() {
        return Err(recovery(
            "legacy Page admission requires idle operation authority",
        ));
    }
    Ok(())
}

pub(super) fn validate_retained_admission(
    catalog: &Catalog,
    engine: &ChangeEngine,
    proof: &IndexedRefreshProof,
    delta: &RetainedDelta,
) -> Result<()> {
    let validated = retained_lineage(catalog, engine, &proof.change)?;
    if proof.version != 4
        || delta.version != 4
        || delta.rows.version != 3
        || delta.legacy_page_admission.as_ref() != Some(&validated.binding)
    {
        return Err(recovery(
            "legacy Page admission retained descriptor differs",
        ));
    }
    let op = &validated.manifest.operations[0];
    let Some(IndexedWriteOperation::PageBatch { pages }) = &proof.operation else {
        return Err(recovery("legacy Page admission permits only PageBatch"));
    };
    if pages.len() != 1 || pages[0].path != op.target || pages[0].id != validated.page_id {
        return Err(recovery(
            "legacy Page admission differs from its single target",
        ));
    }
    // Proof validation supplies sorted, unique and identical before/after paths.
    // Recheck that the sole mutable target is exact and all supplemental paths
    // are equal constraints; v4 may never introduce an extra canonical write.
    let mut target_count = 0;
    if proof.before.len() != proof.after.len() {
        return Err(recovery("legacy Page admission boundary lengths differ"));
    }
    for (before, after) in proof.before.iter().zip(&proof.after) {
        if before.path != after.path {
            return Err(recovery("legacy Page admission boundary paths differ"));
        }
        if before.path == op.target {
            target_count += 1;
            if before.expected != op.before || after.expected != op.after {
                return Err(recovery("legacy Page admission target hashes differ"));
            }
        } else if before.expected != after.expected {
            return Err(recovery(
                "legacy Page admission supplemental guard mutates a path",
            ));
        }
    }
    if target_count != 1 {
        return Err(recovery(
            "legacy Page admission requires exactly one boundary target",
        ));
    }
    Ok(())
}

impl<'a> IndexedRefreshSession<'a> {
    /// Preview authenticates the explicit legacy intent and current target only.
    /// It deliberately obtains no selected SQL projection or durable admission.
    pub(crate) fn preview_legacy_page(catalog: &Catalog, change: &PreparedChange) -> Result<()> {
        let engine = ChangeEngine::new(catalog.fs.clone())?;
        let validated = retained_lineage(catalog, &engine, change)?;
        require_never_started(catalog, &engine, &validated, change)?;
        let authority = catalog.operation_state()?;
        let inverse = engine.page_inverse_plan(&validated.binding.parent)?;
        let draft = inverse.draft();
        let original = &validated.manifest.operations[0];
        if draft.operations.len() != 1
            || draft.inverse_of != validated.manifest.inverse_of
            || draft.origin.is_some()
            || !draft.allocated_ids.is_empty()
            || !draft.read_preconditions.is_empty()
        {
            return Err(recovery(
                "legacy Page preview differs from original write intent",
            ));
        }
        let write = &draft.operations[0];
        if write.target != original.target
            || write.expected != original.before
            || !write.apply_after.is_empty()
            || write.proposed.as_ref() != Some(&validated.proposed)
        {
            return Err(recovery(
                "legacy Page preview changes target, payload or order",
            ));
        }
        let fresh = retained_lineage(catalog, &engine, change)?;
        if fresh.binding != validated.binding || fresh.manifest != validated.manifest {
            return Err(recovery(
                "legacy Page preview history changed during observation",
            ));
        }
        require_never_started(catalog, &engine, &fresh, change)?;
        let current = read_bounded(engine.fs(), &original.target, PAGE_BYTES)?;
        if current.as_ref().map(Blake3Hash::digest)
            != match &original.before {
                ExpectedState::Hash(hash) => Some(hash.clone()),
                ExpectedState::Absent => None,
            }
        {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "legacy Page preview target changed during observation",
            ));
        }
        if catalog.operation_state()? != authority {
            return Err(recovery(
                "legacy Page preview authority changed during observation",
            ));
        }
        Ok(())
    }

    pub(crate) fn adopt_legacy_page(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        change: &PreparedChange,
    ) -> Result<Self> {
        writer.require_root(catalog.fs.root())?;
        let engine = ChangeEngine::new(catalog.fs.clone())?;
        let validated = retained_lineage(catalog, &engine, change)?;
        require_never_started(catalog, &engine, &validated, change)?;
        let inverse = engine.page_inverse_plan(&validated.binding.parent)?;
        let query = catalog.query_snapshot(QueryReadLimits::default())?;
        let projected = crate::catalog::write_projection::project_page_inverse(
            &catalog.fs,
            &query,
            inverse,
            &crate::catalog::source_projection::RefreshProjectionLimits::default(),
        )?
        .ok_or_else(|| {
            recovery("legacy Page admission projection unexpectedly produced no write")
        })?;
        let parts = projected.into_parts();
        drop(query);
        let original = &validated.manifest.operations[0];
        if parts.draft.operations.len() != 1
            || parts.draft.inverse_of != validated.manifest.inverse_of
            || parts.draft.origin.is_some()
            || !parts.draft.allocated_ids.is_empty()
        {
            return Err(recovery(
                "legacy Page projection differs from original write intent",
            ));
        }
        let write = &parts.draft.operations[0];
        if write.target != original.target
            || write.expected != original.before
            || !write.apply_after.is_empty()
            || write.proposed.as_ref() != Some(&validated.proposed)
        {
            return Err(recovery(
                "legacy Page projection changes target, payload or order",
            ));
        }
        let _admitted = Self::admitted_engine(catalog, writer, &parts)?;
        _admitted.plan(&parts.draft)?;
        let delta = RetainedDelta {
            version: 4,
            vault_id: catalog.vault_id.clone(),
            source_id: None,
            operation: Some(parts.operation),
            change: change.clone(),
            base: parts.base,
            before: parts.before,
            after: parts.after,
            rows: parts.delta,
            legacy_page_admission: Some(validated.binding),
        };
        super::super::normalized_delta::counted(&delta, ENVELOPE_BYTES)?;
        let bytes =
            serde_json::to_vec(&delta).map_err(|error| WikiError::invalid(error.to_string()))?;
        if bytes.len() > ENVELOPE_BYTES {
            return Err(budget("legacy Page delta exceeds envelope ceiling"));
        }
        let delta_hash = Blake3Hash::digest(bytes);
        let proof = IndexedRefreshProof {
            version: 4,
            vault_id: delta.vault_id.clone(),
            source_id: None,
            operation: delta.operation.clone(),
            change: change.clone(),
            base: delta.base.clone(),
            intended: intended(&delta.base, change, &delta_hash, 4)?,
            delta_hash,
            before: delta.before.clone(),
            after: delta.after.clone(),
        };
        delta.require_bound(catalog, &engine, &proof)?;
        // Reauthenticate history and the never-started predicate immediately
        // before the sole durable admission; no new change is allocated.
        let fresh = retained_lineage(catalog, &engine, change)?;
        if delta.legacy_page_admission.as_ref() != Some(&fresh.binding) {
            return Err(recovery(
                "legacy Page admission history changed before retention",
            ));
        }
        require_never_started(catalog, &engine, &fresh, change)?;
        let remaining = Cell::new(MAX_SELECTED_BYTES);
        verify_dependencies(catalog, &proof.before, None, &remaining)?;
        let query = catalog.query_snapshot(QueryReadLimits::default())?;
        if QueryCatalog::snapshot(&query) != &proof.base {
            return Err(recovery("legacy Page admission base changed"));
        }
        drop(query);
        let authority =
            operation_authority::load(engine.fs(), &catalog.vault_id, Presence::Required)?
                .ok_or_else(|| recovery("legacy Page admission authority disappeared"))?;
        if authority.active().is_some() || authority.publication() != &publication(&proof.base)? {
            return Err(recovery(
                "legacy Page admission no longer has idle base authority",
            ));
        }
        retain_legacy_page_envelope(catalog, writer, &proof, &delta)?;
        Self::open(catalog, writer, proof, delta).map_err(|error| retained_error(error, change))
    }
}
