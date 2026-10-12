//! Typed admission of exact retained graph intent into one selected publication.
use super::{
    eligibility_facts::{EligibilityEdge, EligibilityFact, EligibilityRole},
    normalized_delta::CatalogDelta,
    query::QuerySnapshot,
    query_types::QueryCatalog,
    source_projection::{self, RefreshProjectionLimits, Work, baseline, entry},
    types::RecordRow,
    write_projection::{self, ProjectedWrite},
};
use crate::{
    changes::{
        ChangeDraft, ChangeEngine, ChangeManifest, OperationRole, PreparedChange, ProposedTarget,
        ValidationInput,
        indexed_refresh::{
            GraphWriteAction, IndexedGraphTarget, IndexedPageTarget, IndexedWriteOperation,
        },
    },
    domain::*,
    records::parse_note,
    sources::SourceView,
    vault::{ExpectedState, VaultFs},
};
use std::collections::{BTreeMap, BTreeSet};

/// One command/session graph verification allowance. Verified immutable payloads
/// may be reused, while every read/decode/projection shares the original clock.
#[derive(Clone)]
pub(crate) struct GraphBudget {
    pub(super) started: std::time::Instant,
    bytes: std::rc::Rc<std::cell::Cell<usize>>,
    cache: std::rc::Rc<std::cell::RefCell<BTreeMap<VaultRelativePath, Option<Vec<u8>>>>>,
    pub(super) projection_bytes: std::rc::Rc<std::cell::Cell<usize>>,
    pub(super) projection_visits: std::rc::Rc<std::cell::Cell<usize>>,
}
impl GraphBudget {
    pub(crate) fn new() -> Self {
        Self {
            started: std::time::Instant::now(),
            bytes: Default::default(),
            cache: Default::default(),
            projection_bytes: Default::default(),
            projection_visits: Default::default(),
        }
    }
    pub(crate) fn tick(&self) -> Result<()> {
        if self.started.elapsed() > std::time::Duration::from_secs(30) {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "graph verification exceeded its 30 second allowance; use a smaller extraction or review batch",
            ));
        }
        Ok(())
    }
    pub(crate) fn read(
        &self,
        fs: &VaultFs,
        path: &VaultRelativePath,
        maximum: usize,
    ) -> Result<Option<Vec<u8>>> {
        self.tick()?;
        if let Some(bytes) = self.cache.borrow().get(path) {
            if bytes.as_ref().is_some_and(|bytes| bytes.len() > maximum) {
                return Err(actionable(source_projection::budget()));
            }
            return Ok(bytes.clone());
        }
        let remaining = crate::changes::indexed_refresh::MAX_GRAPH_ENVELOPE_BYTES
            .checked_sub(self.bytes.get())
            .ok_or_else(source_projection::budget)?;
        let limit = maximum.min(remaining);
        let bytes = match crate::vault::fs::read_regular_bounded(
            fs.root(),
            path,
            false,
            limit,
            "graph-verification",
        )
        .map_err(actionable)?
        {
            Some(bytes) => Some(bytes),
            None => crate::storage::layout::legacy_payload(fs, path, limit)?,
        };
        let size = bytes.as_ref().map_or(0, Vec::len);
        self.bytes.set(
            self.bytes
                .get()
                .checked_add(size)
                .filter(|n| *n <= crate::changes::indexed_refresh::MAX_GRAPH_ENVELOPE_BYTES)
                .ok_or_else(source_projection::budget)?,
        );
        self.tick()?;
        if path.as_str().starts_with(".wiki/retained/objects/blake3/")
            || (path.as_str().starts_with("changes/")
                && (path.as_str().contains("/proposed/") || path.as_str().contains("/before/")))
        {
            self.cache.borrow_mut().insert(path.clone(), bytes.clone());
        }
        Ok(bytes)
    }
}

// Existing resolution permits at most 705 non-Decision records plus 64 mention
// decisions. Review has its own stricter request/receipt bounds. Closure rows and
// encoded retention remain independently metered before any canonical write.
pub(super) const MAX_GRAPH_WRITES: usize = 769;
/// Only the authenticated graph projector constructs this exact replacement
/// boundary. Shared navigation/policy work must not add unrelated exclusions.
#[derive(Clone)]
pub(super) struct GraphReplacementScope {
    paths: BTreeSet<VaultRelativePath>,
}
impl GraphReplacementScope {
    fn new(records: &[IndexedGraphTarget]) -> Self {
        Self {
            paths: records.iter().map(|record| record.path.clone()).collect(),
        }
    }
    pub(super) fn require(&self, paths: &BTreeSet<VaultRelativePath>) -> Result<()> {
        if paths != &self.paths || paths.len() > MAX_GRAPH_WRITES {
            return Err(invalid(
                "graph registry/policy exclusions differ from admitted graph targets",
            ));
        }
        Ok(())
    }
}
fn actionable(mut error: WikiError) -> WikiError {
    if error.code == ErrorCode::BudgetExceeded {
        error
            .message
            .push_str("; use fewer extraction mentions or a smaller assertion review batch");
    }
    error
}
fn invalid(message: &str) -> WikiError {
    WikiError::invalid(message)
}
fn allowed(action: GraphWriteAction, kind: RecordKind) -> bool {
    match action {
        GraphWriteAction::Packet => kind == RecordKind::ExtractionPacket,
        GraphWriteAction::Import => kind == RecordKind::Extraction,
        GraphWriteAction::Resolve => matches!(
            kind,
            RecordKind::Extraction
                | RecordKind::Entity
                | RecordKind::Decision
                | RecordKind::Assertion
                | RecordKind::Evidence
        ),
        GraphWriteAction::Review => matches!(
            kind,
            RecordKind::Assertion | RecordKind::Evidence | RecordKind::Decision
        ),
    }
}
pub(crate) fn validate_descriptor(
    action: GraphWriteAction,
    records: &[IndexedGraphTarget],
) -> Result<()> {
    if records.is_empty()
        || records.len() > MAX_GRAPH_WRITES
        || matches!(action, GraphWriteAction::Packet | GraphWriteAction::Import)
            && records.len() != 1
        || !records.windows(2).all(|p| p[0].path < p[1].path)
        || records.iter().map(|r| &r.id).collect::<BTreeSet<_>>().len() != records.len()
        || records
            .iter()
            .any(|r| !allowed(action, r.kind) || !crate::sources::revision::canonical_path(&r.path))
    {
        return Err(invalid(
            "graph operation has an invalid action, target kind, identity or bounded scope",
        ));
    }
    Ok(())
}
pub(crate) fn validate_manifest_shape(
    manifest: &ChangeManifest,
    action: GraphWriteAction,
    records: &[IndexedGraphTarget],
) -> Result<()> {
    use crate::changes::OriginOperation;
    validate_descriptor(action, records)?;
    let origin_ok = match (action, manifest.origin.as_ref()) {
        (GraphWriteAction::Packet, None) => manifest.allocated_ids.is_empty(),
        (GraphWriteAction::Import, Some(o)) => o.operation == OriginOperation::GraphImport,
        (GraphWriteAction::Resolve, Some(o)) => o.operation == OriginOperation::GraphResolve,
        (GraphWriteAction::Review, Some(o)) => o.operation == OriginOperation::GraphReview,
        _ => false,
    };
    if !origin_ok
        || manifest.inverse_of.is_some()
        || manifest.operations.len() != records.len()
        || manifest.operations.iter().zip(records).any(|(op, r)| {
            op.target != r.path
                || op.role
                    != if action == GraphWriteAction::Packet {
                        OperationRole::ImmutableAsset
                    } else {
                        OperationRole::MutableRecord
                    }
                || !op.apply_after.is_empty()
                || !matches!(op.after, ExpectedState::Hash(_))
                || matches!(action, GraphWriteAction::Packet | GraphWriteAction::Import)
                    && op.before != ExpectedState::Absent
                || action == GraphWriteAction::Resolve
                    && r.kind != RecordKind::Extraction
                    && op.before != ExpectedState::Absent
        })
    {
        return Err(invalid(
            "graph manifest differs from its exact action and record boundary",
        ));
    }
    Ok(())
}
/// Classification grants no authority. Every selected candidate still receives
/// full retained payload, intent, identity, semantic and publication admission.
pub(crate) fn candidate(manifest: &ChangeManifest) -> bool {
    use crate::changes::OriginOperation;
    manifest.inverse_of.is_none()
        && match manifest.origin.as_ref() {
            Some(o) => matches!(
                o.operation,
                OriginOperation::GraphImport
                    | OriginOperation::GraphResolve
                    | OriginOperation::GraphReview
            ),
            None => {
                manifest.operations.len() == 1
                    && manifest.operations[0].before == ExpectedState::Absent
                    && manifest.operations[0]
                        .target
                        .as_str()
                        .starts_with("knowledge/extractions/packets/")
            }
        }
}
#[cfg(test)]
pub(crate) fn retained_draft(
    engine: &ChangeEngine,
    change: &PreparedChange,
) -> Result<(ChangeManifest, ChangeDraft, IndexedWriteOperation)> {
    retained_draft_with_budget(engine, change, &GraphBudget::new())
}
pub(crate) fn retained_draft_with_budget(
    engine: &ChangeEngine,
    change: &PreparedChange,
    budget: &GraphBudget,
) -> Result<(ChangeManifest, ChangeDraft, IndexedWriteOperation)> {
    use crate::changes::{ExpectedWrite, OriginOperation};
    budget.tick()?;
    let (manifest, hash) = engine
        .load_manifest_structure_with_reader(&change.change_id, &mut |p, m| {
            budget.read(engine.fs(), p, m)
        })?;
    if hash != change.manifest_hash || !candidate(&manifest) {
        return Err(invalid(
            "retained Change is not an exact supported graph proposal",
        ));
    }
    // Reject graph count/role/ordering and declared aggregate sizes BEFORE the
    // generic verifier can visit either payload side.
    if manifest.operations.is_empty()
        || manifest.operations.len() > MAX_GRAPH_WRITES
        || manifest
            .operations
            .iter()
            .any(|op| !op.apply_after.is_empty() || !matches!(op.after, ExpectedState::Hash(_)))
    {
        return Err(invalid(
            "graph retained operation shape exceeds its admitted boundary",
        ));
    }
    let declared = manifest
        .operations
        .iter()
        .flat_map(|op| [&op.before_payload, &op.after_payload])
        .flatten()
        .try_fold(0u64, |n, p| n.checked_add(p.byte_len))
        .ok_or_else(source_projection::budget)?;
    if declared > crate::changes::indexed_refresh::MAX_GRAPH_ENVELOPE_BYTES as u64 {
        return Err(actionable(source_projection::budget()));
    }
    let action = match manifest.origin.as_ref().map(|o| o.operation) {
        None => GraphWriteAction::Packet,
        Some(OriginOperation::GraphImport) => GraphWriteAction::Import,
        Some(OriginOperation::GraphResolve) => GraphWriteAction::Resolve,
        Some(OriginOperation::GraphReview) => GraphWriteAction::Review,
        _ => return Err(invalid("unsupported graph operation")),
    };
    if matches!(action, GraphWriteAction::Packet | GraphWriteAction::Import)
        && (manifest.operations.len() != 1
            || manifest.operations[0].before != ExpectedState::Absent)
    {
        return Err(invalid("graph creation requires one absent target"));
    }
    if manifest.operations.iter().any(|op| {
        op.role
            != if action == GraphWriteAction::Packet {
                OperationRole::ImmutableAsset
            } else {
                OperationRole::MutableRecord
            }
    }) {
        return Err(invalid(
            "graph retained operation role differs from its action",
        ));
    }
    engine.validate_manifest_with_reader(&manifest, &change.change_id, &mut |p, m| {
        budget.read(engine.fs(), p, m)
    })?;
    budget.tick()?;
    let mut remaining = crate::changes::indexed_refresh::MAX_GRAPH_ENVELOPE_BYTES;
    let mut records = Vec::new();
    let mut operations = Vec::new();
    for (index, op) in manifest.operations.iter().enumerate() {
        let bytes = engine
            .verify_payload_with_reader(
                &manifest.change_id,
                index,
                "proposed",
                &op.target,
                (&op.after, &op.after_payload),
                remaining,
                &mut |p, m| budget.read(engine.fs(), p, m),
            )?
            .ok_or_else(|| invalid("graph proposal cannot delete a record"))?;
        remaining = remaining
            .checked_sub(bytes.len())
            .ok_or_else(source_projection::budget)?;
        let parsed = parse_note(&bytes);
        let record = parsed
            .canonical
            .as_ref()
            .ok_or_else(|| invalid("graph proposal requires a valid canonical record"))?;
        records.push(IndexedGraphTarget {
            path: op.target.clone(),
            id: record.id().clone(),
            kind: record.kind(),
        });
        operations.push(ExpectedWrite {
            target: op.target.clone(),
            expected: op.before.clone(),
            proposed: Some(bytes),
            apply_after: vec![],
        });
    }
    validate_manifest_shape(&manifest, action, &records)?;
    budget.tick()?;
    let draft = ChangeDraft {
        title: manifest.title.clone(),
        origin: manifest.origin.clone(),
        inverse_of: None,
        allocated_ids: manifest.allocated_ids.clone(),
        read_preconditions: manifest.read_preconditions.clone(),
        operations,
    };
    Ok((
        manifest,
        draft,
        IndexedWriteOperation::GraphBatch { action, records },
    ))
}

pub(crate) fn project_graph(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    change: &PreparedChange,
    limits: &RefreshProjectionLimits,
    budget: &GraphBudget,
) -> Result<ProjectedWrite> {
    project_graph_inner(fs, reader, change, limits, budget).map_err(actionable)
}
fn project_graph_inner(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    change: &PreparedChange,
    limits: &RefreshProjectionLimits,
    budget: &GraphBudget,
) -> Result<ProjectedWrite> {
    reader.require_policy_layout()?;
    let engine = ChangeEngine::new(fs.clone())?;
    let (_, draft, operation) = retained_draft_with_budget(&engine, change, budget)?;
    let IndexedWriteOperation::GraphBatch { records, .. } = &operation else {
        unreachable!()
    };
    let mut work = Work::new(fs, reader, limits)?;
    work.started = budget.started;
    work.bytes = budget.projection_bytes.get();
    work.visits = budget.projection_visits.get();
    work.capture_path(&VaultRelativePath::new("WIKI.md")?)?;
    for dependency in &draft.read_preconditions {
        work.capture(&dependency.path, &dependency.expected)?;
    }
    let mut targets = Vec::new();
    for (op, target) in draft.operations.iter().zip(records) {
        let bytes = op.proposed.as_ref().unwrap();
        work.charge(bytes.len())?;
        work.capture(&op.target, &op.expected)?;
        let claim = reader.unique_identity_claim(&target.id)?;
        let existing = work.load(&target.id)?;
        match &op.expected {
            ExpectedState::Absent => {
                if existing || claim.is_some() || reader.document_metadata(&op.target)?.is_some() {
                    return Err(source_projection::conflict(
                        "new graph identity or path is already claimed",
                    ));
                }
            }
            ExpectedState::Hash(hash) => {
                let old = work.old.get(&target.id).ok_or_else(|| {
                    source_projection::conflict("graph replacement lacks an adopted prior identity")
                })?;
                if old.path != op.target
                    || old.hash != *hash
                    || old.record.kind() != target.kind
                    || claim.as_ref().is_none_or(|c| {
                        c.path != op.target || c.hash != *hash || c.kind != Some(target.kind)
                    })
                {
                    return Err(source_projection::conflict(
                        "graph replacement must preserve authenticated identity, kind and path",
                    ));
                }
            }
        }
        let note = parse_note(bytes);
        let record = note.canonical.unwrap();
        let row = RecordRow {
            authored_status: record.string("wiki_status").map(str::to_owned),
            record,
            path: op.target.clone(),
            hash: note.source_hash,
            eligibility: Eligibility::Current,
            reasons: vec![],
            identity_eligibility: None,
            description_eligibility: None,
            disputed: false,
            dependencies: vec![],
        };
        work.overlay.insert(op.target.clone(), bytes.clone());
        work.replaced_registry.insert(op.target.clone());
        work.new_registry.push(entry(&row));
        work.now.insert(target.id.clone(), row);
        work.facts
            .entry(target.id.clone())
            .or_insert_with(|| EligibilityFact {
                baseline: baseline(),
                structural: Default::default(),
                direct_paths: BTreeSet::from([op.target.clone()]),
            });
        targets.push(IndexedPageTarget {
            path: target.path.clone(),
            id: target.id.clone(),
        });
    }
    work.graph_replacements = Some(GraphReplacementScope::new(records));
    install_edges(&mut work)?;
    write_projection::finish_graph(work, draft, targets, operation, change, budget)
}

/// Install the complete raw new relation boundary before closure discovery.
/// Structural evaluation later replaces these with validated semantic edges.
fn install_edges(work: &mut Work<'_>) -> Result<()> {
    for row in work
        .now
        .values()
        .filter(|r| work.overlay.contains_key(&r.path))
        .cloned()
        .collect::<Vec<_>>()
    {
        let id = row.record.id();
        let mut edges = BTreeSet::new();
        for (field, _, _) in super::eligibility::references(&row.record) {
            if let Some(target) = row.record.string(field) {
                edges.insert(EligibilityEdge {
                    owner_id: id.clone(),
                    target_id: RecordId::new(target)?,
                    role: EligibilityRole::TypedReference {
                        field: field.into(),
                    },
                });
            }
        }
        for (field, role) in [
            ("wiki_depends_on_ids", EligibilityRole::DeclaredSupport),
            ("wiki_source_ids", EligibilityRole::ExtractionSource),
            (
                "wiki_source_revision_ids",
                EligibilityRole::ExtractionRevision,
            ),
            ("wiki_input_ids", EligibilityRole::DecisionInput),
            ("wiki_output_ids", EligibilityRole::DecisionOutput),
        ] {
            for target in super::scan::list(&row.record, field) {
                edges.insert(EligibilityEdge {
                    owner_id: id.clone(),
                    target_id: RecordId::new(target)?,
                    role: role.clone(),
                });
            }
        }
        edges.extend(work.reader.outgoing_edges(
            id,
            &[
                EligibilityRole::AssertionEvidence,
                EligibilityRole::GenerationPacket,
            ],
        )?);
        work.edge_overrides.insert(id.clone(), edges);
    }
    // Evidence membership is by canonical assertion ID, not display links.
    let evidence = work
        .now
        .values()
        .filter(|r| r.record.kind() == RecordKind::Evidence && work.overlay.contains_key(&r.path))
        .cloned()
        .collect::<Vec<_>>();
    for row in evidence {
        let assertion = source_projection::record_id(&row.record, "wiki_assertion_id")?;
        work.require(&assertion)?;
        if !work.edge_overrides.contains_key(&assertion) {
            let roles = super::structural_projection::roles();
            let mut all = work
                .reader
                .outgoing_edges(&assertion, &roles)?
                .into_iter()
                .collect::<BTreeSet<_>>();
            all.extend(work.reader.outgoing_edges(
                &assertion,
                &[
                    EligibilityRole::AssertionEvidence,
                    EligibilityRole::GenerationPacket,
                ],
            )?);
            work.edge_overrides.insert(assertion.clone(), all);
        }
        work.edge_overrides
            .get_mut(&assertion)
            .unwrap()
            .insert(EligibilityEdge {
                owner_id: assertion,
                target_id: row.record.id().clone(),
                role: EligibilityRole::AssertionEvidence,
            });
    }
    Ok(())
}

pub(super) fn verify_overlay(
    work: &mut Work<'_>,
    draft: &ChangeDraft,
    change: &PreparedChange,
    action: GraphWriteAction,
    budget: &GraphBudget,
) -> Result<()> {
    budget.tick()?;
    let input = ValidationInput {
        vault_id: QueryCatalog::vault_id(work.reader).clone(),
        documents: work.captured.values().cloned().collect(),
        overlay: draft
            .operations
            .iter()
            .map(|op| ProposedTarget {
                path: op.target.clone(),
                bytes: op.proposed.clone(),
            })
            .collect(),
    };
    let view = SourceView::from_closed_input(work.fs, &input)?;
    let engine = ChangeEngine::new(work.fs.clone())?;
    let (manifest, hash) = engine
        .load_manifest_structure_with_reader(&change.change_id, &mut |p, m| {
            budget.read(engine.fs(), p, m)
        })?;
    if hash != change.manifest_hash {
        return Err(invalid("graph manifest changed during projection"));
    }
    match action {
        GraphWriteAction::Packet => {
            let id = parse_note(draft.operations[0].proposed.as_ref().unwrap())
                .canonical
                .unwrap()
                .id()
                .clone();
            let packet = crate::graph::packet::load_packet(&view, &id)?;
            for dep in packet.dependencies() {
                if !work.before.contains_key(&dep.path) {
                    return Err(invalid("packet authorization dependency omitted"));
                }
            }
        }
        GraphWriteAction::Import => {
            let id = parse_note(draft.operations[0].proposed.as_ref().unwrap())
                .canonical
                .unwrap()
                .id()
                .clone();
            let extracted = crate::graph::import::load_extraction(&view, &id)?;
            let artifact = &extracted.artifact;
            let origin = manifest.origin.as_ref().unwrap();
            let mut allocated = BTreeMap::from([("extraction".to_owned(), id)]);
            for (local, id) in &artifact.allocations.assertions {
                allocated.insert(format!("assertion:{local}"), id.clone());
            }
            for (local, ids) in &artifact.allocations.evidence {
                for (i, id) in ids.iter().enumerate() {
                    allocated.insert(format!("evidence:{local}:{i}"), id.clone());
                }
            }
            if origin.packet_id != artifact.packet_id
                || origin.response_hash != artifact.response_hash
                || allocated != manifest.allocated_ids
                || !artifact.materialized_assertions.is_empty()
                || artifact
                    .bindings
                    .values()
                    .any(|b| !matches!(b, crate::graph::MentionBinding::Pending))
            {
                return Err(invalid(
                    "graph import origin, allocations or proposal-only state differs",
                ));
            }
        }
        GraphWriteAction::Resolve => {
            let before = SourceView::from_closed_input(
                work.fs,
                &ValidationInput {
                    overlay: vec![],
                    ..input.clone()
                },
            )?;
            let inspection = engine.inspect_with_reader(&change.change_id, &mut |p, m| {
                budget.read(engine.fs(), p, m)
            })?;
            let verified = crate::graph::resolution::retained_receipt_with_reader(
                &engine,
                &before,
                &inspection,
                &manifest.origin.as_ref().unwrap().packet_id,
                &mut |p, m| budget.read(engine.fs(), p, m),
            )?;
            if verified.receipt().request_hash != manifest.origin.as_ref().unwrap().response_hash {
                return Err(invalid("resolution origin request differs"));
            }
        }
        GraphWriteAction::Review => {
            let mut remaining = crate::changes::indexed_refresh::MAX_GRAPH_ENVELOPE_BYTES;
            let retained = engine.retained_graph_input_with_reader(
                &manifest,
                &hash,
                &mut remaining,
                &mut |p, m| budget.read(engine.fs(), p, m),
            )?;
            crate::graph::review::verify_review_overlay(work.fs, &input, Some(&retained))?
                .ok_or_else(|| invalid("graph review lacks exact retained authorization"))?;
        }
    }
    Ok(())
}

pub(crate) fn validate_retained_rows(
    engine: &ChangeEngine,
    manifest: &ChangeManifest,
    action: GraphWriteAction,
    records: &[IndexedGraphTarget],
    delta: &CatalogDelta,
    budget: &GraphBudget,
) -> Result<()> {
    validate_manifest_shape(manifest, action, records)?;
    if !delta.owners.is_empty() {
        return Err(invalid(
            "graph publication cannot own captured revision trees",
        ));
    }
    if delta.facts.is_none() {
        return Err(invalid(
            "graph publication requires complete normalized facts",
        ));
    }
    // Complete inserts AND deletes (including policy/generation and unchanged
    // evidence membership) are certified by exact selected reprojection before
    // activation. Active replay requires the durable hash of that whole proof.
    for (index, (op, target)) in manifest.operations.iter().zip(records).enumerate() {
        let bytes = engine
            .verify_payload_with_reader(
                &manifest.change_id,
                index,
                "proposed",
                &op.target,
                (&op.after, &op.after_payload),
                crate::changes::indexed_refresh::MAX_GRAPH_ENVELOPE_BYTES,
                &mut |p, m| budget.read(engine.fs(), p, m),
            )?
            .ok_or_else(|| invalid("graph retained payload absent"))?;
        let note = parse_note(&bytes);
        let record = note
            .canonical
            .as_ref()
            .ok_or_else(|| invalid("graph retained payload malformed"))?;
        let row = delta
            .records
            .iter()
            .find(|r| r.record.id() == &target.id)
            .ok_or_else(|| invalid("graph publication omitted a written record"))?;
        if record.id() != &target.id
            || record.kind() != target.kind
            || row.path != op.target
            || &row.record != record
            || row.hash != note.source_hash
        {
            return Err(invalid(
                "graph publication row differs from exact retained record",
            ));
        }
        if !delta.documents.iter().any(|m|matches!(m,super::normalized_delta::DocumentMutation::Put{row} if row.path==op.target && row.raw_text.as_bytes()==bytes && row.hash==note.source_hash)) {
            return Err(invalid("graph publication document differs from retained bytes"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn graph_replacement_scope_rejects_an_added_page_or_omitted_graph_owner() {
        let target = IndexedGraphTarget {
            path: VaultRelativePath::new("knowledge/entities/one.md").unwrap(),
            id: RecordId::new("entity_one").unwrap(),
            kind: RecordKind::Entity,
        };
        let scope = GraphReplacementScope::new(std::slice::from_ref(&target));
        let mut paths = BTreeSet::from([target.path]);
        scope.require(&paths).unwrap();
        paths.insert(VaultRelativePath::new("notes/unrelated-page.md").unwrap());
        assert!(scope.require(&paths).is_err());
        assert!(scope.require(&BTreeSet::new()).is_err());
    }
    #[test]
    fn graph_reads_share_actual_bytes_and_the_original_deadline() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), b"eight123").unwrap();
        let fs = VaultFs::new(crate::vault::VaultRoot::explicit(temp.path()).unwrap());
        let path = VaultRelativePath::new("WIKI.md").unwrap();
        let budget = GraphBudget::new();
        budget.read(&fs, &path, 8).unwrap();
        let cloned = budget.clone();
        cloned.read(&fs, &path, 8).unwrap();
        assert_eq!(budget.bytes.get(), 16);
        // The declared budget cannot hide actual bytes. Clamp the next read
        // before touching a file larger than the remaining allowance.
        budget
            .bytes
            .set(crate::changes::indexed_refresh::MAX_GRAPH_ENVELOPE_BYTES - 7);
        assert!(cloned.read(&fs, &path, 8).is_err());
        assert_eq!(
            budget.bytes.get(),
            crate::changes::indexed_refresh::MAX_GRAPH_ENVELOPE_BYTES - 7
        );
        let mut expired = GraphBudget::new();
        expired.started = std::time::Instant::now() - std::time::Duration::from_secs(31);
        assert_eq!(
            expired.read(&fs, &path, 8).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(expired.bytes.get(), 0);
    }
}
