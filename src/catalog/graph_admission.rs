//! First selected admission preserves an exact never-started graph Change.
use super::super::graph_projection::GraphBudget;
use super::*;
use crate::changes::{
    ChangeEvent,
    indexed_refresh::{GraphAdmission, MAX_GRAPH_ENVELOPE_BYTES, baseline_path},
    outcome,
};

fn prefix(
    engine: &ChangeEngine,
    change: &PreparedChange,
    budget: &GraphBudget,
) -> Result<GraphAdmission> {
    let (manifest, hash) = engine
        .load_manifest_structure_with_reader(&change.change_id, &mut |p, m| {
            budget.read(engine.fs(), p, m)
        })?;
    if hash != change.manifest_hash {
        return Err(recovery("graph admission manifest changed"));
    }
    let state = journal::load_journal_with_reader(&manifest, &hash, &mut |p, m| {
        budget.read(engine.fs(), p, m)
    })?;
    let first = state
        .frames
        .first()
        .filter(|frame| matches!(frame.event, ChangeEvent::Prepared))
        .ok_or_else(|| recovery("graph admission requires original Prepared journal prefix"))?;
    Ok(GraphAdmission {
        prepared_journal_hash: Blake3Hash::digest(journal::encode_frame(first)?),
    })
}
fn never_started(
    catalog: &Catalog,
    engine: &ChangeEngine,
    change: &PreparedChange,
    budget: &GraphBudget,
) -> Result<GraphAdmission> {
    let (manifest, hash) = engine
        .load_manifest_structure_with_reader(&change.change_id, &mut |p, m| {
            budget.read(engine.fs(), p, m)
        })?;
    if hash != change.manifest_hash {
        return Err(recovery("graph admission manifest changed"));
    }
    let state = journal::load_journal_with_reader(&manifest, &hash, &mut |p, m| {
        budget.read(engine.fs(), p, m)
    })?;
    if state.torn_tail
        || state.frames.len() != 1
        || state.status != Some(ChangeStatus::Prepared)
        || outcome::terminal_report_with_reader(&manifest, &hash, &mut |p, m| {
            budget.read(engine.fs(), p, m)
        })?
        .is_some()
    {
        return Err(recovery(
            "graph first admission requires a never-started Prepared proposal",
        ));
    }
    let bytes = budget
        .read(
            engine.fs(),
            &journal::journal_path(&change.change_id)?,
            MAX_GRAPH_ENVELOPE_BYTES,
        )?
        .ok_or_else(|| recovery("graph Prepared journal missing"))?;
    if bytes != journal::encode_frame(&state.frames[0])? {
        return Err(recovery("graph Prepared journal prefix changed"));
    }
    for path in [baseline_path(change)?, delta_path(change)?] {
        if budget
            .read(engine.fs(), &path, MAX_GRAPH_ENVELOPE_BYTES)?
            .is_some()
        {
            return Err(recovery(
                "graph first admission refuses existing validation authority",
            ));
        }
    }
    let authority = catalog
        .operation_state()?
        .ok_or_else(|| recovery("graph first admission requires normalized authority"))?;
    if authority.active().is_some() {
        return Err(recovery(
            "recover the active operation before admitting another graph proposal",
        ));
    }
    prefix(engine, change, budget)
}
pub(super) fn validate_embedded(delta: &RetainedDelta, proof: &IndexedRefreshProof) -> Result<()> {
    if delta.version != 5
        || proof.version != 5
        || delta.rows.version != 3
        || delta.graph_admission.is_none()
        || delta.legacy_page_admission.is_some()
        || delta.source_id.is_some()
        || delta.vault_id != proof.vault_id
        || delta.change != proof.change
        || delta.operation != proof.operation
        || delta.base != proof.base
        || delta.before != proof.before
        || delta.after != proof.after
    {
        return Err(recovery("embedded graph admission identity differs"));
    }
    super::super::normalized_delta::counted(delta, MAX_GRAPH_ENVELOPE_BYTES)?;
    let bytes = serde_json::to_vec(delta).map_err(|e| WikiError::invalid(e.to_string()))?;
    if Blake3Hash::digest(bytes) != proof.delta_hash
        || intended(&proof.base, &proof.change, &proof.delta_hash, 5)? != proof.intended
    {
        return Err(recovery(
            "embedded graph admission hash or publication differs",
        ));
    }
    delta.rows.validate()
}
pub(super) fn validate_retained(
    engine: &ChangeEngine,
    proof: &IndexedRefreshProof,
    delta: &RetainedDelta,
    budget: &GraphBudget,
) -> Result<()> {
    budget.tick()?;
    if delta.graph_admission.as_ref() != Some(&prefix(engine, &proof.change, budget)?) {
        return Err(recovery(
            "graph admission original Prepared journal differs",
        ));
    }
    let (manifest, _, operation) =
        super::super::graph_projection::retained_draft_with_budget(engine, &proof.change, budget)?;
    if proof.operation.as_ref() != Some(&operation) {
        return Err(recovery(
            "graph admission operation differs from retained intent",
        ));
    }
    proof.validate_manifest(&manifest)
}
impl<'a> IndexedRefreshSession<'a> {
    pub(super) fn reprove_graph_before_activation(&self) -> Result<()> {
        let budget = self
            .graph_budget
            .as_ref()
            .ok_or_else(|| recovery("graph verification budget missing"))?;
        budget.tick()?;
        let engine = ChangeEngine::new(self.catalog.fs.clone())?;
        let (manifest, hash) = engine
            .load_manifest_structure_with_reader(&self.proof.change.change_id, &mut |p, m| {
                budget.read(engine.fs(), p, m)
            })?;
        let state = journal::load_journal_with_reader(&manifest, &hash, &mut |p, m| {
            budget.read(engine.fs(), p, m)
        })?;
        if hash != self.proof.change.manifest_hash
            || state.torn_tail
            || state.frames.len() != 1
            || state.status != Some(ChangeStatus::Prepared)
            || outcome::terminal_report_with_reader(&manifest, &hash, &mut |p, m| {
                budget.read(engine.fs(), p, m)
            })?
            .is_some()
        {
            return Err(recovery(
                "graph first activation requires exact never-started Prepared history",
            ));
        }
        self.verify_selected(false, false)?;
        let reader = self
            .starting
            .as_ref()
            .ok_or_else(|| recovery("graph first activation requires exact base reader"))?;
        if QueryCatalog::snapshot(reader) != &self.proof.base {
            return Err(recovery("graph first activation base changed"));
        }
        let parts = super::super::graph_projection::project_graph(
            &self.catalog.fs,
            reader,
            &self.proof.change,
            &super::super::source_projection::RefreshProjectionLimits::default(),
            budget,
        )?
        .into_parts();
        if Some(&parts.operation) != self.delta.operation.as_ref()
            || parts.base != self.delta.base
            || parts.before != self.delta.before
            || parts.after != self.delta.after
            || parts.delta != self.delta.rows
        {
            return Err(recovery(
                "retained graph projection differs from the exact authorized canonical operation; retain the proposal for inspection",
            ));
        }
        self.retained()?;
        self.verify_selected(false, false)?;
        budget.tick()
    }
    /// Recovery inventory verifies staged intent and unchanged write targets.
    /// A stale publication or another active Change does not activate this
    /// proposal and must not prevent recovery of that other Change.
    pub(crate) fn check_staged_graph(catalog: &Catalog, proof: &IndexedRefreshProof) -> Result<()> {
        if proof.version != 5 {
            return Err(recovery("staged graph verification requires version5"));
        }
        let budget = GraphBudget::new();
        let engine = ChangeEngine::new_with_reader(catalog.fs.clone(), &mut |p, m| {
            budget.read(&catalog.fs, p, m)
        })?;
        let delta = load_delta(catalog, proof, &budget)?;
        delta.require_bound_with_budget(catalog, &engine, proof, Some(&budget))?;
        let (manifest, hash) = engine
            .load_manifest_structure_with_reader(&proof.change.change_id, &mut |p, m| {
                budget.read(engine.fs(), p, m)
            })?;
        let state = journal::load_journal_with_reader(&manifest, &hash, &mut |p, m| {
            budget.read(engine.fs(), p, m)
        })?;
        if hash != proof.change.manifest_hash
            || state.status != Some(ChangeStatus::Prepared)
            || outcome::terminal_report_with_reader(&manifest, &hash, &mut |p, m| {
                budget.read(engine.fs(), p, m)
            })?
            .is_some()
        {
            return Err(recovery(
                "staged graph verification requires Prepared history without a terminal outcome",
            ));
        }
        for operation in &manifest.operations {
            let bytes = budget.read(engine.fs(), &operation.target, MAX_PAYLOAD_BYTES)?;
            if super::super::source_projection::state(bytes.as_deref()) != operation.before {
                return Err(recovery(
                    "staged indexed proposal has changed targets without applying authority",
                ));
            }
        }
        budget.tick()
    }
    /// Filesystem-only preview: no selected SQL admission or publication claim.
    pub(crate) fn preview_graph(catalog: &Catalog, change: &PreparedChange) -> Result<()> {
        let budget = GraphBudget::new();
        let engine = ChangeEngine::new(catalog.fs.clone())?;
        let binding = never_started(catalog, &engine, change, &budget)?;
        let (_, draft, _) =
            super::super::graph_projection::retained_draft_with_budget(&engine, change, &budget)?;
        engine.plan_with_reader(&draft, &mut |p, m| budget.read(engine.fs(), p, m))?;
        if never_started(catalog, &engine, change, &budget)? != binding {
            return Err(recovery("graph preview history changed"));
        }
        Ok(())
    }
    pub(crate) fn adopt_graph(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        change: &PreparedChange,
    ) -> Result<Self> {
        writer.require_root(catalog.fs.root())?;
        let budget = GraphBudget::new();
        let engine = ChangeEngine::new(catalog.fs.clone())?;
        let binding = never_started(catalog, &engine, change, &budget)?;
        let (_, original, _) =
            super::super::graph_projection::retained_draft_with_budget(&engine, change, &budget)?;
        engine.plan_with_reader(&original, &mut |p, m| budget.read(engine.fs(), p, m))?;
        let query = catalog.query_snapshot(QueryReadLimits::default())?;
        let projected = super::super::graph_projection::project_graph(
            &catalog.fs,
            &query,
            change,
            &super::super::source_projection::RefreshProjectionLimits::default(),
            &budget,
        )?;
        let parts = projected.into_parts();
        drop(query);
        // Projection may add only read dependencies; all original canonical
        // intent, allocation and operation ordering remain byte-for-byte bound.
        if parts.draft.operations.len() != original.operations.len()
            || parts
                .draft
                .operations
                .iter()
                .zip(&original.operations)
                .any(|(a, b)| {
                    a.target != b.target
                        || a.expected != b.expected
                        || a.proposed != b.proposed
                        || a.apply_after != b.apply_after
                })
            || parts.draft.origin != original.origin
            || parts.draft.allocated_ids != original.allocated_ids
            || parts.draft.inverse_of != original.inverse_of
        {
            return Err(recovery(
                "graph projection changed retained canonical intent",
            ));
        }
        let admitted = Self::admitted_engine(catalog, writer, &parts)?;
        admitted.plan_with_reader(&parts.draft, &mut |p, m| budget.read(admitted.fs(), p, m))?;
        let delta = RetainedDelta {
            version: 5,
            vault_id: catalog.vault_id.clone(),
            source_id: None,
            operation: Some(parts.operation),
            change: change.clone(),
            base: parts.base,
            before: parts.before,
            after: parts.after,
            rows: parts.delta,
            legacy_page_admission: None,
            graph_admission: Some(binding.clone()),
        };
        super::super::normalized_delta::counted(&delta, MAX_GRAPH_ENVELOPE_BYTES)?;
        let delta_hash = Blake3Hash::digest(
            serde_json::to_vec(&delta).map_err(|e| WikiError::invalid(e.to_string()))?,
        );
        let proof = IndexedRefreshProof {
            version: 5,
            vault_id: delta.vault_id.clone(),
            source_id: None,
            operation: delta.operation.clone(),
            change: change.clone(),
            base: delta.base.clone(),
            intended: intended(&delta.base, change, &delta_hash, 5)?,
            delta_hash,
            before: delta.before.clone(),
            after: delta.after.clone(),
        };
        delta.require_bound_with_budget(catalog, &engine, &proof, Some(&budget))?;
        if never_started(catalog, &engine, change, &budget)? != binding {
            return Err(recovery("graph history changed before admission"));
        }
        verify_dependencies(catalog, &proof.before, None, &Cell::new(MAX_SELECTED_BYTES))?;
        let query = catalog.query_snapshot(QueryReadLimits::default())?;
        if QueryCatalog::snapshot(&query) != &proof.base {
            return Err(recovery("graph base changed before admission"));
        }
        drop(query);
        budget.tick()?;
        engine.stage_graph_envelope(writer, &proof, &delta)?;
        Self::open_with_budget(catalog, writer, proof, delta, Some(budget))
            .map_err(|e| retained_error(e, change))
    }
}

/// Every v5 envelope decode in an admission/recovery session uses the same meter.
pub(super) fn load_delta(
    catalog: &Catalog,
    proof: &IndexedRefreshProof,
    budget: &GraphBudget,
) -> Result<RetainedDelta> {
    let engine = ChangeEngine::new(catalog.fs.clone())?;
    let (retained, delta) = engine
        .load_indexed_refresh_retention_with_reader(&proof.change, &mut |p, m| {
            budget.read(engine.fs(), p, m)
        })?
        .ok_or_else(|| recovery("graph admission envelope missing"))?;
    budget.tick()?;
    if retained != *proof {
        return Err(recovery("graph admission proof changed"));
    }
    delta.ok_or_else(|| recovery("graph embedded delta missing"))
}
