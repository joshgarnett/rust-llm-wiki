//! Complete selected structural components for canonical Page overlays.
//! Outgoing reference witnesses are boundary rows, not permission to scan their
//! unrelated reverse fanout. Only affected consumers and whole cycle/Decision
//! components enter the recomputation workset.
use super::{
    decision_rules::{self, DecisionPolicy},
    eligibility,
    eligibility_facts::{EligibilityEdge, EligibilityRole},
    eligibility_rules,
    link_facts::{self, MatchKeyKind},
    navigation_resolution::RegistryProbe,
    normalized_delta::CatalogDelta,
    policy_delta::PolicyDelta,
    policy_facts::{PolicyKind, PolicyRow, PolicyState},
    query_types::QueryCatalog,
    scan,
    source_projection::{Work, corrupt, entry},
    structural_rules::{self, ReferenceBoundary, StructuralRecorder, StructuralStage},
};
use crate::{
    domain::{RecordId, RecordKind, Result},
    graph::policy_inputs::PolicyInputKey,
    records::links::IndexedRegistry,
    vault::ExpectedState,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

fn roles() -> Vec<EligibilityRole> {
    let mut roles = vec![
        EligibilityRole::DeclaredSupport,
        EligibilityRole::SourceInventory,
        EligibilityRole::ExtractionSource,
        EligibilityRole::ExtractionRevision,
        EligibilityRole::DecisionInput,
        EligibilityRole::DecisionOutput,
    ];
    for field in [
        "wiki_superseded_by_id",
        "wiki_subject_id",
        "wiki_object_id",
        "wiki_assertion_id",
        "wiki_source_id",
        "wiki_source_revision",
        "wiki_supersedes_id",
        "wiki_extraction_id",
        "wiki_current_revision",
        "wiki_packet_id",
        "wiki_checkpoint_event_id",
        "wiki_run_id",
    ] {
        roles.push(EligibilityRole::TypedReference {
            field: field.into(),
        });
    }
    roles
}
fn cycle_roles() -> Vec<EligibilityRole> {
    vec![
        EligibilityRole::DeclaredSupport,
        EligibilityRole::PolicySupersession,
        EligibilityRole::TypedReference {
            field: "wiki_supersedes_id".into(),
        },
        EligibilityRole::TypedReference {
            field: "wiki_superseded_by_id".into(),
        },
    ]
}

struct PolicyBoundary {
    states: BTreeMap<PolicyKind, PolicyState>,
    aliases: BTreeSet<RecordId>,
    families: Vec<BTreeSet<RecordId>>,
    review_edges: BTreeSet<(RecordId, RecordId)>,
}
impl DecisionPolicy for PolicyBoundary {
    fn historical_alias_authority(&self, id: &RecordId) -> bool {
        self.aliases.contains(id)
    }
    fn decisions_compatible(&self, ids: &[RecordId]) -> bool {
        ids.len() > 1
            && self
                .families
                .iter()
                .any(|family| ids.iter().all(|id| family.contains(id)))
    }
}
fn replacement_edges(
    policy: &PolicyDelta,
    kind: PolicyKind,
) -> Option<BTreeSet<(RecordId, RecordId)>> {
    policy
        .replacements
        .iter()
        .find(|r| r.kind == kind)
        .map(|replacement| {
            replacement
                .rows
                .iter()
                .filter_map(|row| match row {
                    PolicyRow::Edge { before, after, .. } => Some((before.clone(), after.clone())),
                    _ => None,
                })
                .collect()
        })
}
fn policy_edges(
    work: &mut Work<'_>,
    policy: &PolicyDelta,
    id: &RecordId,
    reverse: bool,
) -> Result<BTreeSet<(RecordId, RecordId)>> {
    let mut edges = BTreeSet::new();
    for kind in [PolicyKind::Remap, PolicyKind::Review] {
        if let Some(replacement) = replacement_edges(policy, kind) {
            edges.extend(
                replacement
                    .into_iter()
                    .filter(|(before, after)| if reverse { before == id } else { after == id }),
            );
        } else {
            // Stored policy edges are predecessor -> successor. Structural edges
            // run in the opposite direction.
            edges.extend(work.reader.policy_edges(kind, id, !reverse)?);
        }
    }
    Ok(edges)
}
fn boundary(
    work: &mut Work<'_>,
    policy: &PolicyDelta,
    active: &BTreeSet<RecordId>,
) -> Result<PolicyBoundary> {
    let mut result = PolicyBoundary {
        states: BTreeMap::new(),
        aliases: BTreeSet::new(),
        families: vec![],
        review_edges: BTreeSet::new(),
    };
    for kind in [PolicyKind::Remap, PolicyKind::Review] {
        if let Some(replacement) = policy.replacements.iter().find(|r| r.kind == kind) {
            let mut families: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
            for row in &replacement.rows {
                work.tick()?;
                match row {
                    PolicyRow::State { state, .. } => {
                        result.states.insert(kind, state.clone());
                    }
                    PolicyRow::Alias(id) => {
                        result.aliases.insert(id.clone());
                    }
                    PolicyRow::FamilyMember { family, id } => {
                        families
                            .entry(family.clone())
                            .or_default()
                            .insert(id.clone());
                    }
                    PolicyRow::Edge {
                        kind: PolicyKind::Review,
                        before,
                        after,
                    } => {
                        result.review_edges.insert((before.clone(), after.clone()));
                    }
                    _ => {}
                }
            }
            result.families.extend(families.into_values());
        } else {
            result.states.insert(kind, work.reader.policy_state(kind)?);
            match kind {
                PolicyKind::Remap => {
                    result.aliases = work.reader.policy_alias_authority(active)?;
                    result.families = work
                        .reader
                        .policy_family_members(active)?
                        .into_values()
                        .collect();
                }
                PolicyKind::Review => {
                    for id in active {
                        result
                            .review_edges
                            .extend(work.reader.policy_edges(kind, id, false)?);
                        result
                            .review_edges
                            .extend(work.reader.policy_edges(kind, id, true)?);
                    }
                }
            }
        }
    }
    if result.states.len() != 2 {
        return Err(corrupt("policy replacement lacks complete state"));
    }
    Ok(result)
}

pub(super) fn recompute(
    work: &mut Work<'_>,
    mut seeds: BTreeSet<RecordId>,
    policy: &PolicyDelta,
    delta: &mut CatalogDelta,
) -> Result<BTreeSet<RecordId>> {
    let raw_roles = roles();
    let cycle_roles = cycle_roles();
    let mut missing = BTreeSet::new();
    // Page replacements own only declared structural references. Install the
    // overlay relation before discovering either additions or removed edges.
    for id in &seeds {
        let row = &work.now[id];
        if row.record.kind() != RecordKind::Page || !work.overlay.contains_key(&row.path) {
            continue;
        }
        let edges = scan::list(&row.record, "wiki_depends_on_ids")
            .into_iter()
            .map(|target| {
                Ok(EligibilityEdge {
                    owner_id: id.clone(),
                    target_id: RecordId::new(target)?,
                    role: EligibilityRole::DeclaredSupport,
                })
            })
            .collect::<Result<_>>()?;
        work.edge_overrides.insert(id.clone(), edges);
    }
    // A global receipt result can invalidate every relevant producer, including
    // a Page whose raw fence contributes to review policy. Membership queries
    // are complete indexed certificates, not a scan of all canonical notes.
    for replacement in &policy.replacements {
        let key = match replacement.kind {
            PolicyKind::Remap => PolicyInputKey::RemapReceiptCandidates,
            PolicyKind::Review => PolicyInputKey::ReviewReceiptCandidates,
        };
        let mut notes = BTreeMap::new();
        for (path, hash) in work.reader.policy_members(&key, &work.replaced_registry)? {
            work.capture(&path, &ExpectedState::Hash(hash))?;
            notes.insert(path.clone(), work.note(&path)?);
        }
        for path in work.overlay.keys().cloned().collect::<Vec<_>>() {
            notes.insert(path.clone(), work.note(&path)?);
        }
        let relevant = match replacement.kind {
            PolicyKind::Remap => crate::graph::remap::relevant_decision_ids(&notes),
            PolicyKind::Review => crate::graph::review::relevant_decision_ids(&notes),
        };
        seeds.extend(relevant);
        let prior_outputs = work.reader.policy_output_rows(replacement.kind)?;
        for row in prior_outputs.iter().chain(&replacement.rows) {
            work.tick()?;
            match row {
                PolicyRow::Alias(id) | PolicyRow::FamilyMember { id, .. } => {
                    seeds.insert(id.clone());
                }
                PolicyRow::Edge { before, after, .. } => {
                    seeds.extend([before.clone(), after.clone()]);
                }
                _ => {}
            }
        }
    }
    let mut queue: VecDeque<_> = seeds.into_iter().collect();
    let mut active = BTreeSet::new();
    let mut visited = BTreeSet::new();
    while let Some(id) = queue.pop_front() {
        work.tick()?;
        if !visited.insert(id.clone()) {
            continue;
        }
        // Every structural consumer can change on either validity restoration
        // or invalidation. Reverse discovery includes dangling indexed targets.
        for edge in work.edges(&id, &raw_roles, true)? {
            queue.push_back(edge.owner_id);
        }
        if !work.load(&id)? {
            missing.insert(id);
            continue;
        }
        active.insert(id.clone());
        let row = work.now[&id].clone();
        for target in structural_rules::reference_ids(&row)? {
            if !work.load(&target)? {
                missing.insert(target.clone());
            }
            if row.record.kind() == RecordKind::Decision {
                queue.push_back(target);
            }
        }
        // Cycles are nonlocal: obtain the complete weak component of each
        // cycle graph, including removed edges and old/new policy edges.
        for reverse in [false, true] {
            let prior = if reverse {
                work.reader.dependent_edges(&id, &cycle_roles)?
            } else {
                work.reader.outgoing_edges(&id, &cycle_roles)?
            };
            for edge in prior {
                queue.push_back(if reverse {
                    edge.owner_id
                } else {
                    edge.target_id
                });
            }
            for edge in work.edges(&id, &cycle_roles, reverse)? {
                queue.push_back(if reverse {
                    edge.owner_id
                } else {
                    edge.target_id
                });
            }
            for (before, after) in policy_edges(work, policy, &id, reverse)? {
                queue.push_back(if reverse { after } else { before });
            }
        }
        // Companion paths can name an unrelated existing record. Include exact
        // path witnesses so a partial registry cannot turn conflict into stale.
        for (field, kind, companion) in eligibility::references(&row.record) {
            if let (Some(target), Some(destination)) = (
                row.record.string(field),
                companion.and_then(|c| row.record.string(c)),
            ) {
                let fact = link_facts::typed_fact(
                    &row.path,
                    0,
                    destination,
                    &RecordId::new(target)?,
                    kind,
                )?;
                for key in fact
                    .keys
                    .iter()
                    .filter(|key| key.kind == MatchKeyKind::Path)
                {
                    match work.registry_probe(key, true)? {
                        RegistryProbe::Zero => {}
                        RegistryProbe::One(candidate) => {
                            work.require(&candidate.id)?;
                        }
                        RegistryProbe::Many(candidates) => {
                            for candidate in candidates {
                                work.require(&candidate.id)?;
                            }
                        }
                    }
                }
            }
        }
    }
    // A missing seed still has reverse consumers (notably an absent Decision
    // target). Changed Page seeds are present; missing cycle endpoints may have
    // reverse edges, already discovered from their complete component members.
    missing.retain(|id| !work.now.contains_key(id));
    let policy_boundary = boundary(work, policy, &active)?;
    let registry = IndexedRegistry::new(work.now.values().map(entry).collect());
    let reference_snapshot = work.now.clone();
    let reference_boundary = ReferenceBoundary::selected(&reference_snapshot, &missing)?;
    let mut rows: BTreeMap<_, _> = active
        .iter()
        .map(|id| (id.clone(), work.now[id].clone()))
        .collect();
    let mut recorder = StructuralRecorder::new(true);
    let mut diagnostics = Vec::new();
    let mut raw = BTreeMap::new();
    let mut declared = BTreeMap::new();
    let mut supersession = BTreeMap::new();
    let mut semantics = BTreeMap::new();
    for (id, row) in &mut rows {
        work.tick()?;
        eligibility_rules::restore_baseline(row, &super::source_projection::baseline());
        let refs = structural_rules::evaluate_references(
            row,
            &registry,
            &reference_boundary,
            &mut diagnostics,
            &mut recorder,
        )?;
        raw.insert(id.clone(), refs.targets);
        declared.insert(id.clone(), refs.declared);
        supersession.insert(id.clone(), refs.supersession);
        semantics.insert(id.clone(), refs.semantic);
    }
    recorder.phase(StructuralStage::Invariant);
    let notes: BTreeMap<_, _> = work
        .now
        .values()
        .map(|row| Ok((row.path.clone(), work.note(&row.path)?)))
        .collect::<Result<_>>()?;
    for kind in [PolicyKind::Remap, PolicyKind::Review] {
        if let PolicyState::Failed(error) = &policy_boundary.states[&kind] {
            let relevant = match kind {
                PolicyKind::Remap => crate::graph::remap::relevant_decision_ids(&notes),
                PolicyKind::Review => crate::graph::review::relevant_decision_ids(&notes),
            };
            for id in relevant {
                if let Some(row) = rows.get_mut(&id) {
                    structural_rules::invalidate(
                        row,
                        match kind {
                            PolicyKind::Remap => "entity_decision_receipt_invalid",
                            PolicyKind::Review => "review_receipt_invalid",
                        },
                        &mut diagnostics,
                        serde_json::json!({"error":error}),
                        &mut recorder,
                    );
                }
            }
        }
    }
    for id in &active {
        for (before, after) in policy_edges(work, policy, id, false)? {
            supersession
                .entry(after.clone())
                .or_default()
                .insert(before.clone());
            semantics
                .entry(after.clone())
                .or_default()
                .insert(EligibilityEdge {
                    owner_id: after,
                    target_id: before,
                    role: EligibilityRole::PolicySupersession,
                });
        }
    }
    for (ids, reason) in [
        (decision_rules::cycle_members(&declared), "dependency_cycle"),
        (
            decision_rules::cycle_members(&supersession),
            "supersession_cycle",
        ),
        (
            decision_rules::overlong_supersession_roots(&supersession),
            "supersession_chain_over_limit",
        ),
    ] {
        for id in ids {
            if let Some(row) = rows.get_mut(&id) {
                structural_rules::invalidate(
                    row,
                    reason,
                    &mut diagnostics,
                    serde_json::Value::Null,
                    &mut recorder,
                );
            }
        }
    }
    // Only the complete selected Decision component may enter the three passes.
    // Non-Decision reference witnesses provide kind/status and extraction bytes.
    let boundary_ids: Vec<_> = work
        .now
        .iter()
        .filter(|(id, row)| !active.contains(*id) && row.record.kind() != RecordKind::Decision)
        .map(|(id, _)| id.clone())
        .collect();
    for id in &boundary_ids {
        rows.insert(id.clone(), work.now[id].clone());
    }
    recorder.phase(StructuralStage::Decision);
    decision_rules::apply_decisions(
        &notes,
        &mut rows,
        &mut diagnostics,
        matches!(
            policy_boundary.states[&PolicyKind::Remap],
            PolicyState::Verified
        )
        .then_some(&policy_boundary as &dyn DecisionPolicy),
        &policy_boundary.review_edges,
        &mut recorder,
    )?;
    for id in boundary_ids {
        rows.remove(&id);
    }
    let mut emitted = recorder.finish();
    let adopted = work
        .overlay
        .keys()
        .any(|path| !work.old.values().any(|row| &row.path == path));
    let mut integrity = if adopted {
        adoption_integrity(work, &active)?
    } else {
        BTreeMap::new()
    };

    // Existing Page body/status/alias edits preserve integrity input identity,
    // paths and bytes. New identity/path adoption rechecks exact diagnostics.
    for id in &active {
        let kind = work.now[id].record.kind();
        let fact = &mut work.facts.get_mut(id).unwrap().structural;
        if adopted && matches!(kind, RecordKind::Evidence | RecordKind::Revision) {
            let effects = integrity.remove(id).unwrap_or_default();
            for stage in [
                StructuralStage::RevisionIntegrity,
                StructuralStage::EvidenceIntegrity,
            ] {
                fact.replace_stage_effects(
                    stage,
                    effects
                        .effects
                        .iter()
                        .filter(|e| e.stage == stage)
                        .cloned()
                        .collect(),
                )?;
            }
        }
        let replacement = emitted.remove(id).unwrap_or_default();
        for stage in [
            StructuralStage::Reference,
            StructuralStage::Invariant,
            StructuralStage::Decision,
        ] {
            fact.replace_stage_effects(
                stage,
                replacement
                    .effects
                    .iter()
                    .filter(|e| e.stage == stage)
                    .cloned()
                    .collect(),
            )?;
        }
        fact.replace_stage_effects(StructuralStage::Propagation, vec![])?;
        eligibility_rules::restore_baseline(
            rows.get_mut(id).unwrap(),
            &fact.baseline_through(StructuralStage::RevisionIntegrity)?,
        );
    }
    if !emitted.is_empty() {
        return Err(corrupt(
            "Decision closure emitted effects outside its admitted component",
        ));
    }
    let mut outside = BTreeMap::new();
    for target in raw.values().flatten() {
        if !active.contains(target) {
            let invalid = if let Some(row) = work.now.get(target) {
                let mut row = row.clone();
                eligibility_rules::restore_baseline(
                    &mut row,
                    &work.facts[target]
                        .structural
                        .baseline_through(StructuralStage::Propagation)?,
                );
                structural_rules::structurally_invalid(&row)
            } else if missing.contains(target) {
                false
            } else {
                return Err(corrupt("propagation target lacks an absence certificate"));
            };
            outside.insert(target.clone(), invalid);
        }
    }
    let mut propagation = StructuralRecorder::new(true);
    propagation.phase(StructuralStage::Propagation);
    structural_rules::propagate_invalid(
        &mut rows,
        &raw,
        &mut diagnostics,
        &outside,
        &mut propagation,
    )?;
    let mut propagated = propagation.finish();
    for id in &active {
        work.tick()?;
        let fact = work.facts.get_mut(id).unwrap();
        fact.structural.replace_stage_effects(
            StructuralStage::Propagation,
            propagated.remove(id).unwrap_or_default().effects,
        )?;
        fact.baseline = fact.structural.baseline();
        let after = semantics.remove(id).unwrap_or_default();
        let mut selected_roles = raw_roles.clone();
        selected_roles.push(EligibilityRole::PolicySupersession);
        let before: BTreeSet<_> = work
            .reader
            .outgoing_edges(id, &selected_roles)?
            .into_iter()
            .collect();
        let facts = delta.facts.as_mut().unwrap();
        facts
            .edge_deletes
            .extend(before.difference(&after).cloned());
        facts
            .edge_inserts
            .extend(after.difference(&before).cloned());
        // Retain nonstructural support/generation edges for dynamic discovery.
        let mut complete = after;
        complete.extend(work.reader.outgoing_edges(
            id,
            &[
                EligibilityRole::AssertionEvidence,
                EligibilityRole::GenerationPacket,
            ],
        )?);
        work.edge_overrides.insert(id.clone(), complete);
        work.now.insert(id.clone(), rows.remove(id).unwrap());
    }
    Ok(active)
}

/// A newly named Page can change missing/duplicate/kind/companion diagnostics
/// even though it cannot turn into a Source, Revision, Assertion or Evidence.
/// Recheck those immutable predicates with complete selected identity and exact
/// path witnesses; ordinary body/alias edits retain their unchanged predicates.
fn adoption_integrity(
    work: &mut Work<'_>,
    active: &BTreeSet<RecordId>,
) -> Result<BTreeMap<RecordId, structural_rules::StructuralFact>> {
    use crate::{
        changes::{ProposedTarget, ValidationInput},
        domain::{Blake3Hash, CitationRef, ErrorCode, WikiError},
        sources::{CitationScope, SourceView, evidence::evidence_reference},
    };
    let mut identities = BTreeSet::new();
    let mut revisions = BTreeSet::new();
    let mut targets = Vec::new();
    for id in active {
        let row = &work.now[id];
        match row.record.kind() {
            RecordKind::Evidence => {
                let reference = evidence_reference(&row.record)?;
                identities.extend([
                    reference.source_id,
                    reference.source_revision.clone(),
                    reference.assertion_id,
                    reference.evidence_id,
                ]);
                revisions.insert(reference.source_revision);
                targets.push(id.clone());
            }
            RecordKind::Revision => {
                identities.extend([
                    id.clone(),
                    super::source_projection::record_id(&row.record, "wiki_source_id")?,
                ]);
                revisions.insert(id.clone());
                targets.push(id.clone());
            }
            _ => {}
        }
    }
    let mut pending: VecDeque<_> = identities.into_iter().collect();
    let mut visited = BTreeSet::new();
    let mut paths = BTreeSet::new();
    while let Some(id) = pending.pop_front() {
        work.tick()?;
        if !visited.insert(id.clone()) {
            continue;
        }
        for claim in work.reader.identity_claims_for_id(&id)? {
            work.capture(&claim.path, &ExpectedState::Hash(claim.hash))?;
            paths.insert(claim.path);
        }
        // Overlay identities have no old claim yet; they are already captured
        // as an explicit absence and supplied through the sealed overlay.
        let notes: Vec<_> = paths.iter().map(|p| work.note(p)).collect::<Result<_>>()?;
        for note in notes {
            let Some(record) = note.canonical.as_ref().filter(|r| r.id() == &id) else {
                continue;
            };
            if record.kind() == RecordKind::Source {
                if let Some(head) = record.string("wiki_current_revision") {
                    pending.push_back(RecordId::new(head)?);
                }
            }
            if record.kind() == RecordKind::Revision {
                pending.push_back(super::source_projection::record_id(
                    record,
                    "wiki_source_id",
                )?);
            }
            for (_, _, companion) in eligibility::references(record) {
                if let Some(destination) = companion.and_then(|field| record.string(field)) {
                    let companions = crate::records::links::companion_paths(destination);
                    for companion in
                        std::iter::once(companions.direct.to_owned()).chain(companions.fallback)
                    {
                        if let Ok(path) = crate::domain::VaultRelativePath::new(companion)
                            && work.reader.document_metadata(&path)?.is_some()
                        {
                            work.capture_path(&path)?;
                        }
                    }
                }
            }
        }
    }
    // Revision facts contain its own original/content observations, including
    // genuine absent assets. Fetch exact published expectations, never replace
    // a cached absence with a new unindexed physical file.
    for id in revisions {
        if work.load(&id)? && work.now[&id].record.kind() == RecordKind::Revision {
            for path in work.facts[&id].direct_paths.clone() {
                work.capture_path(&path)?;
            }
        }
    }
    let input = ValidationInput {
        vault_id: QueryCatalog::vault_id(work.reader).clone(),
        documents: work.captured.values().cloned().collect(),
        overlay: work
            .before
            .iter()
            .filter(|(_, expected)| **expected == ExpectedState::Absent)
            .filter(|(path, _)| {
                !work.overlay.contains_key(*path) && !work.removed_paths.contains(*path)
            })
            .map(|(path, _)| ProposedTarget {
                path: path.clone(),
                bytes: None,
            })
            .chain(work.overlay.iter().map(|(path, bytes)| ProposedTarget {
                path: path.clone(),
                bytes: Some(bytes.clone()),
            }))
            .chain(work.removed_paths.iter().map(|path| ProposedTarget {
                path: path.clone(),
                bytes: None,
            }))
            .collect(),
    };
    let view = SourceView::from_closed_input(work.fs, &input)?;
    let mut recorder = StructuralRecorder::new(true);
    let mut diagnostics = Vec::new();
    for stage in [
        StructuralStage::RevisionIntegrity,
        StructuralStage::EvidenceIntegrity,
    ] {
        recorder.phase(stage);
        for id in &targets {
            work.tick()?;
            let mut row = work.now[id].clone();
            let result = match (stage, row.record.kind()) {
                (StructuralStage::RevisionIntegrity, RecordKind::Revision) => {
                    let source =
                        super::source_projection::record_id(&row.record, "wiki_source_id")?;
                    let mut observed = BTreeMap::new();
                    (|| -> Result<()> {
                        if row.record.string("wiki_extraction_status") == Some("complete") {
                            view.revision_content_bounded(
                                &source,
                                id,
                                &mut observed,
                                work.limits.max_file_bytes,
                                work.limits.max_file_bytes,
                            )?;
                        } else {
                            let parent = row
                                .path
                                .as_str()
                                .rsplit_once('/')
                                .ok_or_else(|| WikiError::invalid("revision lacks directory"))?
                                .0;
                            let original = crate::domain::VaultRelativePath::new(format!(
                                "{parent}/{}",
                                row.record
                                    .string("wiki_original_path")
                                    .expect("validated path")
                            ))?;
                            let bytes = view.read_bounded(
                                &original,
                                &mut observed,
                                work.limits.max_file_bytes,
                            )?;
                            if Some(Blake3Hash::digest(bytes).as_str())
                                != row.record.string("wiki_original_hash")
                            {
                                return Err(WikiError::new(
                                    ErrorCode::SourceIntegrity,
                                    "original snapshot hash mismatch",
                                ));
                            }
                            let source = work
                                .now
                                .get(&source)
                                .ok_or_else(|| WikiError::invalid("missing source"))?;
                            if !scan::list(&source.record, "wiki_revisions")
                                .iter()
                                .any(|value| value == id.as_str())
                            {
                                return Err(WikiError::invalid("revision not retained by source"));
                            }
                        }
                        Ok(())
                    })()
                }
                (StructuralStage::EvidenceIntegrity, RecordKind::Evidence) => view
                    .verify(
                        &CitationRef::Assertion(evidence_reference(&row.record)?),
                        CitationScope::Historical,
                    )
                    .map(|_| ()),
                _ => continue,
            };
            if let Err(error) = result {
                if error.code == ErrorCode::BudgetExceeded {
                    return Err(error);
                }
                structural_rules::invalidate(
                    &mut row,
                    if stage == StructuralStage::RevisionIntegrity {
                        "revision_integrity"
                    } else {
                        "evidence_integrity"
                    },
                    &mut diagnostics,
                    serde_json::json!({"error":error}),
                    &mut recorder,
                );
            }
        }
    }
    Ok(recorder.finish())
}
