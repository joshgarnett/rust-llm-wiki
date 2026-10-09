//! Explicit identity decisions, staged as one guarded recoverable changeset.
use super::{decision_types::*, extraction_types::*, import, mention_state, packet, remap::*};
use crate::{
    changes::*,
    domain::*,
    records::{edit_note, parse_note},
    sources::{
        SourceView,
        revision::{common, record_bytes, timestamp},
    },
    vault::WriterPermit,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

// Leaf implementation follows strict preflight and immutable captured inputs.

fn require_entity(view: &SourceView<'_>, id: &RecordId) -> Result<()> {
    let (_, note) = view.resolve(id, RecordKind::Entity, None)?;
    if note
        .canonical
        .as_ref()
        .and_then(|r| r.string("wiki_status"))
        != Some("active")
    {
        return Err(bad("entity operation requires explicit active identity"));
    }
    Ok(())
}
pub(crate) fn predecessors(
    view: &SourceView<'_>,
    op: &EntityDecision,
) -> Result<BTreeSet<RecordId>> {
    let changing = sources(op);
    let assertions = remaps(op)
        .iter()
        .filter_map(|r| match r {
            EntityRemap::Assertion { assertion_id, .. } => Some(assertion_id.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut result = BTreeSet::new();
    for note in view.notes.values() {
        let Some(r) = &note.canonical else {
            continue;
        };
        if r.kind() != RecordKind::Decision || r.string("wiki_status") != Some("active") {
            continue;
        }
        match r.string("wiki_action") {
            Some("merge" | "split") => {
                if id_list(r, "wiki_output_ids")?
                    .iter()
                    .any(|id| changing.contains(id))
                {
                    result.insert(r.id().clone());
                }
            }
            Some("accept") => {
                let ids = id_list(r, "wiki_input_ids")?
                    .into_iter()
                    .chain(id_list(r, "wiki_output_ids")?)
                    .collect::<BTreeSet<_>>();
                if ids.iter().any(|id| assertions.contains(id)) {
                    if ids.is_empty() || !ids.is_subset(&assertions) {
                        return Err(conflict(
                            "accept predecessor includes an untouched assertion",
                        ));
                    }
                    for id in &ids {
                        let (_, n) = view.resolve(id, RecordKind::Assertion, None)?;
                        if n.canonical.as_ref().and_then(|r| r.string("wiki_status"))
                            != Some("accepted")
                        {
                            return Err(bad(
                                "accept predecessor has nonaccepted/missing assertion",
                            ));
                        }
                    }
                    result.insert(r.id().clone());
                }
            }
            _ => {}
        }
    }
    Ok(result)
}
fn verify_fresh(
    view: &SourceView<'_>,
    request: &EntityDecisionRequest,
) -> Result<BTreeSet<RecordId>> {
    let mut selected = BTreeSet::new();
    for op in &request.decisions {
        let mut required = sources(op);
        for id in &required {
            require_entity(view, id)?;
        }
        match op {
            EntityDecision::MergeEntities { target_id, .. } => {
                require_entity(view, target_id)?;
                required.insert(target_id.clone());
            }
            EntityDecision::AddAlias { entity_id, .. } => {
                require_entity(view, entity_id)?;
                required.insert(entity_id.clone());
            }
            _ => {}
        }
        let refs = reference_set(&view.notes, &sources(op))?;
        let requested = remaps(op)
            .iter()
            .filter(|r| sources(op).contains(old(r)))
            .map(remap_key)
            .collect::<BTreeSet<_>>();
        if refs.keys().cloned().collect::<BTreeSet<_>>() != requested {
            return Err(conflict(
                "merge/split needs exhaustive assertion and resolved mention remaps",
            ));
        }
        required.extend(refs.into_values());
        let mut replaced = BTreeSet::new();
        for r in remaps(op) {
            match r {
                EntityRemap::Assertion {
                    assertion_id,
                    field: f,
                    old_entity_id,
                    ..
                } => {
                    if !sources(op).contains(old_entity_id) {
                        return Err(bad("unrelated assertion remap"));
                    }
                    let (_, n) = view.resolve(assertion_id, RecordKind::Assertion, None)?;
                    if n.canonical.as_ref().and_then(|r| r.string(field(f)))
                        != Some(old_entity_id.as_str())
                    {
                        return Err(conflict("assertion old endpoint differs"));
                    }
                }
                EntityRemap::Mention {
                    extraction_id,
                    mention_id,
                    old_entity_id,
                    target,
                } => {
                    let loaded = import::load_extraction(view, extraction_id)?;
                    selected.insert(extraction_id.clone());
                    let Some(MentionBinding::Resolved {
                        entity_id,
                        decision_id,
                    }) = loaded.artifact.bindings.get(mention_id)
                    else {
                        return Err(bad("mention remap must name existing Resolved endpoint"));
                    };
                    if entity_id != old_entity_id {
                        return Err(conflict("mention old identity differs"));
                    }
                    if !sources(op).contains(entity_id)
                        && !matches!(target,RemapTarget::ExistingEntity{entity_id:id} if id==entity_id)
                    {
                        return Err(bad("unrelated mention carry-forward"));
                    }
                    required.extend([
                        extraction_id.clone(),
                        decision_id.clone(),
                        entity_id.clone(),
                    ]);
                    replaced.insert(decision_id.clone());
                }
            }
        }
        for note in view.notes.values() {
            if note
                .canonical
                .as_ref()
                .is_some_and(|r| r.kind() == RecordKind::Extraction)
            {
                let a = artifact(note)?;
                for (local, binding) in &a.bindings {
                    if let MentionBinding::Resolved{decision_id,..}=binding && replaced.contains(decision_id) && !remaps(op).iter().any(|r|matches!(r,EntityRemap::Mention{extraction_id,mention_id,..} if extraction_id==&a.extraction_id && mention_id==local)){return Err(conflict("shared predecessor requires explicit complete carry-forward"));}
                }
            }
        }
        required.extend(predecessors(view, op)?);
        let hashes = expected(op)
            .iter()
            .map(|g| g.record_id.clone())
            .collect::<BTreeSet<_>>();
        if !required.is_subset(&hashes) {
            return Err(conflict("expected hashes omit affected record/decision"));
        }
        for guard in expected(op) {
            if find(&view.notes, &guard.record_id)?.1.source_hash != guard.hash {
                return Err(conflict("entity decision expected record hash differs"));
            }
        }
    }
    Ok(selected)
}
fn capture(
    view: &SourceView<'_>,
    selected: &BTreeSet<RecordId>,
) -> Result<(ValidationInput, Vec<ReadDependency>)> {
    let mut total = view
        .notes
        .values()
        .try_fold(0usize, |n, note| n.checked_add(note.raw.len()))
        .ok_or_else(|| bad("capture size overflow"))?;
    if total > packet::SOURCE_CAP || view.notes.len() > 4096 {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "entity decision canonical capture exceeds ceiling",
        ));
    }
    let mut documents = view
        .notes
        .iter()
        .map(|(p, n)| {
            (
                p.clone(),
                ScanDocument {
                    path: p.clone(),
                    bytes: n.raw.clone(),
                    hash: n.source_hash.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut deps = view
        .notes
        .iter()
        .map(|(p, n)| {
            (
                p.clone(),
                crate::vault::ExpectedState::Hash(n.source_hash.clone()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for id in selected {
        let verified = import::load_extraction(view, id)?;
        for dep in &verified.dependencies {
            if !documents.contains_key(&dep.path) {
                let bytes = view.read_bounded(
                    &dep.path,
                    &mut deps,
                    packet::SOURCE_CAP
                        .checked_sub(total)
                        .filter(|n| *n > 0)
                        .ok_or_else(|| bad("source asset ceiling exhausted"))?,
                )?;
                total = total
                    .checked_add(bytes.len())
                    .ok_or_else(|| bad("capture size overflow"))?;
                let hash = Blake3Hash::digest(&bytes);
                if dep.expected != crate::vault::ExpectedState::Hash(hash.clone()) {
                    return Err(conflict("source changed during verified capture"));
                }
                documents.insert(
                    dep.path.clone(),
                    ScanDocument {
                        path: dep.path.clone(),
                        bytes,
                        hash,
                    },
                );
            }
            if deps
                .insert(dep.path.clone(), dep.expected.clone())
                .is_some_and(|old| old != dep.expected)
            {
                return Err(conflict("dependency capture hash changed"));
            }
        }
    }
    Ok((
        ValidationInput {
            vault_id: packet::vault_id(view)?,
            documents: documents.into_values().collect(),
            overlay: vec![],
        },
        packet::dependencies(deps),
    ))
}
pub fn validate_entity_decisions(
    view: &SourceView<'_>,
    bytes: &[u8],
) -> Result<ValidatedEntityDecisions> {
    let request = normalize(packet::decode::<EntityDecisionRequest>(
        bytes,
        MAX_ENTITY_DECISION_BYTES,
    )?)?;
    let (task_id, request_hash) = identity(&request)?;
    if let Some(restored) = super::remap::load_entity_decision_receipt(view, &task_id)? {
        if restored.receipt.request != request || restored.receipt.request_hash != request_hash {
            return Err(conflict("different canonical decision for guarded scope"));
        }
        let selected = restored
            .receipt
            .extraction_proofs
            .iter()
            .map(|p| p.extraction_id.clone())
            .collect();
        let (input, dependencies) = capture(view, &selected)?;
        return Ok(ValidatedEntityDecisions {
            request,
            request_hash,
            task_id,
            dependencies,
            restored_receipt: Some(restored.receipt),
            input,
        });
    }
    let selected = verify_fresh(view, &request)?;
    let (input, dependencies) = capture(view, &selected)?;
    let closed = SourceView::from_closed_input(view.fs, &input)?;
    verify_fresh(&closed, &request)?;
    Ok(ValidatedEntityDecisions {
        request,
        request_hash,
        task_id,
        dependencies,
        restored_receipt: None,
        input,
    })
}
fn summary(v: &ValidatedEntityDecisions) -> EntityDecisionSummary {
    EntityDecisionSummary {
        merge_entities: v
            .request
            .decisions
            .iter()
            .filter(|d| matches!(d, EntityDecision::MergeEntities { .. }))
            .count(),
        split_entities: v
            .request
            .decisions
            .iter()
            .filter(|d| matches!(d, EntityDecision::SplitEntity { .. }))
            .count(),
        add_aliases: v
            .request
            .decisions
            .iter()
            .filter(|d| matches!(d, EntityDecision::AddAlias { .. }))
            .count(),
        create_entities: v
            .request
            .decisions
            .iter()
            .map(|d| match d {
                EntityDecision::SplitEntity { new_entities, .. } => new_entities.len(),
                _ => 0,
            })
            .sum(),
        create_decisions: v.request.decisions.len()
            + v.request
                .decisions
                .iter()
                .flat_map(remaps)
                .filter(|r| matches!(r, EntityRemap::Mention { .. }))
                .count(),
        remap_assertion_fields: v
            .request
            .decisions
            .iter()
            .flat_map(remaps)
            .filter(|r| matches!(r, EntityRemap::Assertion { .. }))
            .count(),
        remap_mentions: v
            .request
            .decisions
            .iter()
            .flat_map(remaps)
            .filter(|r| matches!(r, EntityRemap::Mention { .. }))
            .count(),
    }
}
pub fn plan_entity_decisions(v: &ValidatedEntityDecisions) -> Result<EntityDecisionPlan> {
    Ok(EntityDecisionPlan {
        validated: v.clone(),
        summary: summary(v),
    })
}
pub(crate) fn path(kind: RecordKind, id: &RecordId) -> Result<VaultRelativePath> {
    let dir = match kind {
        RecordKind::Entity => "entities",
        RecordKind::Decision => "decisions",
        _ => return Err(bad("unsupported allocated kind")),
    };
    VaultRelativePath::new(format!("knowledge/{dir}/{id}.md"))
}
pub(crate) fn entity_bytes(id: &RecordId, title: &str, kind: &str) -> Result<Vec<u8>> {
    let mut f = common(id, RecordKind::Entity, title);
    f.insert("wiki_status".into(), "active".into());
    f.insert("wiki_entity_type".into(), kind.into());
    record_bytes(
        CanonicalRecord::new(f)?,
        b"\nIdentity created by an explicit exhaustive split decision.\n",
    )
}
pub(crate) fn allocation_map(
    allocations: &[EntityDecisionAllocation],
) -> BTreeMap<String, RecordId> {
    let mut map = BTreeMap::new();
    for (i, a) in allocations.iter().enumerate() {
        map.insert(format!("main:{i}"), a.decision_id.clone());
        for (key, id) in &a.entities {
            map.insert(format!("entity:{i}:{key}"), id.clone());
        }
        for m in &a.mention_decisions {
            map.insert(
                format!("mention:{i}:{}:{}", m.extraction_id, m.mention_id),
                m.decision_id.clone(),
            );
        }
    }
    map
}

fn decision_bytes(
    op: &EntityDecision,
    a: &EntityDecisionAllocation,
    receipt: &EntityDecisionReceiptV1,
    mention: Option<&MentionDecisionAllocation>,
) -> Result<Vec<u8>> {
    decision_bytes_at(op, a, receipt, mention, &timestamp()?)
}
pub(crate) fn decision_bytes_at(
    op: &EntityDecision,
    a: &EntityDecisionAllocation,
    receipt: &EntityDecisionReceiptV1,
    mention: Option<&MentionDecisionAllocation>,
    created_at: &str,
) -> Result<Vec<u8>> {
    let id = mention.map_or(&a.decision_id, |m| &m.decision_id);
    let mut fields = common(
        id,
        RecordKind::Decision,
        "Explicit exhaustive entity decision",
    );
    fields.insert("wiki_status".into(), "active".into());
    fields.insert(
        "wiki_action".into(),
        mention.map_or(action(op), |_| "bind_mention").into(),
    );
    fields.insert("wiki_created_at".into(), created_at.into());
    if let Some(m) = mention {
        let transition = receipt
            .extraction_proofs
            .iter()
            .find(|p| p.extraction_id == m.extraction_id)
            .and_then(|p| p.transitions.iter().find(|t| t.mention_id == m.mention_id))
            .ok_or_else(|| bad("missing replacement transition"))?;
        let MentionBinding::Resolved { entity_id, .. } = &transition.after else {
            return Err(bad("replacement is not resolved"));
        };
        fields.insert("wiki_extraction_id".into(), json!(m.extraction_id));
        fields.insert("wiki_mention_ids".into(), json!([m.mention_id]));
        fields.insert("wiki_input_ids".into(), json!([m.extraction_id]));
        fields.insert("wiki_output_ids".into(), json!([entity_id]));
        fields.insert("wiki_supersedes_id".into(), json!(m.predecessor_id));
        fields.insert(
            "wiki_supersedes".into(),
            json!(format!(
                "[[{}]]",
                receipt
                    .record_paths
                    .get(&m.predecessor_id)
                    .ok_or_else(|| bad("missing predecessor path"))?
            )),
        );
    } else {
        let inputs = match op {
            EntityDecision::AddAlias { entity_id, .. } => vec![entity_id.clone()],
            _ => sources(op).into_iter().collect(),
        };
        fields.insert("wiki_input_ids".into(), json!(inputs));
        fields.insert("wiki_output_ids".into(), json!(output_ids(op, a)));
        let prior = receipt
            .main_supersessions
            .iter()
            .filter(|e| e.successor_id == a.decision_id)
            .collect::<Vec<_>>();
        if prior.len() == 1 {
            fields.insert("wiki_supersedes_id".into(), json!(prior[0].predecessor_id));
            fields.insert(
                "wiki_supersedes".into(),
                json!(format!(
                    "[[{}]]",
                    receipt
                        .record_paths
                        .get(&prior[0].predecessor_id)
                        .ok_or_else(|| bad("missing main predecessor path"))?
                )),
            );
        }
    }
    let mut body = format!(
        "\nExplicit {} identity operation.\n\nRationale: {}\n\n",
        action(op),
        serde_json::to_string(reason(op)).map_err(|e| bad(e.to_string()))?
    )
    .into_bytes();
    body.extend(packet::render_fence(
        receipt,
        ENTITY_DECISION_FENCE,
        MAX_ENTITY_DECISION_RECEIPT_BYTES,
    )?);
    record_bytes(CanonicalRecord::new(fields)?, &body)
}
fn edit(
    view: &SourceView<'_>,
    updates: &mut BTreeMap<RecordId, BTreeMap<String, Value>>,
    id: &RecordId,
    key: &str,
    value: Value,
) -> Result<()> {
    find(&view.notes, id)?;
    let fields = updates.entry(id.clone()).or_default();
    if fields
        .insert(key.into(), value.clone())
        .is_some_and(|old| old != value)
    {
        return Err(bad("conflicting batched edits"));
    }
    Ok(())
}
fn build_draft(view: &SourceView<'_>, v: &ValidatedEntityDecisions) -> Result<ChangeDraft> {
    let count = summary(v);
    if count.create_entities
        + count.create_decisions
        + v.request.decisions.iter().flat_map(expected).count()
        > crate::changes::prepare::MAX_OPS
    {
        return Err(bad("operation ceiling exceeded"));
    }
    let mut allocations = vec![];
    let mut writes = BTreeMap::new();
    let mut record_paths = BTreeMap::new();
    let mut edits: BTreeMap<RecordId, BTreeMap<String, Value>> = BTreeMap::new();
    let mut artifacts: BTreeMap<RecordId, ExtractionArtifactV1> = BTreeMap::new();
    let mut extraction_proofs: BTreeMap<RecordId, ExtractionRemapProof> = BTreeMap::new();
    let mut assertion_proofs = vec![];
    let mut main_supersessions = vec![];
    for op in &v.request.decisions {
        let decision_id = RecordId::generate(RecordKind::Decision)?;
        record_paths.insert(
            decision_id.clone(),
            path(RecordKind::Decision, &decision_id)?,
        );
        let mut a = EntityDecisionAllocation {
            decision_id,
            entities: BTreeMap::new(),
            mention_decisions: vec![],
        };
        if let EntityDecision::SplitEntity { new_entities, .. } = op {
            for e in new_entities {
                let id = RecordId::generate(RecordKind::Entity)?;
                let p = path(RecordKind::Entity, &id)?;
                writes.insert(
                    p.clone(),
                    ExpectedWrite {
                        target: p.clone(),
                        expected: crate::vault::ExpectedState::Absent,
                        proposed: Some(entity_bytes(&id, &e.title, &e.entity_type)?),
                        apply_after: vec![],
                    },
                );
                record_paths.insert(id.clone(), p);
                a.entities.insert(e.key.clone(), id);
            }
        }
        for guard in expected(op) {
            record_paths.insert(
                guard.record_id.clone(),
                find(&view.notes, &guard.record_id)?.0.clone(),
            );
        }
        for r in remaps(op) {
            if let EntityRemap::Mention {
                extraction_id,
                mention_id,
                ..
            } = r
            {
                let (_, n) = find(&view.notes, extraction_id)?;
                let original = artifact(n)?;
                let Some(MentionBinding::Resolved { decision_id, .. }) =
                    original.bindings.get(mention_id)
                else {
                    return Err(bad("mention lost binding"));
                };
                let id = RecordId::generate(RecordKind::Decision)?;
                record_paths.insert(id.clone(), path(RecordKind::Decision, &id)?);
                a.mention_decisions.push(MentionDecisionAllocation {
                    extraction_id: extraction_id.clone(),
                    mention_id: mention_id.clone(),
                    predecessor_id: decision_id.clone(),
                    decision_id: id,
                });
            }
        }
        for source in sources(op) {
            edit(
                view,
                &mut edits,
                &source,
                "wiki_status",
                json!("superseded"),
            )?;
            if let EntityDecision::MergeEntities { target_id, .. } = op {
                edit(
                    view,
                    &mut edits,
                    &source,
                    "wiki_superseded_by_id",
                    json!(target_id),
                )?;
                edit(
                    view,
                    &mut edits,
                    &source,
                    "wiki_superseded_by",
                    json!(format!("[[{}]]", find(&view.notes, target_id)?.0)),
                )?;
            }
        }
        if let EntityDecision::AddAlias {
            entity_id, alias, ..
        } = op
        {
            let (_, n) = find(&view.notes, entity_id)?;
            let mut aliases = n
                .canonical
                .as_ref()
                .and_then(|r| r.field("aliases"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if !aliases.contains(&json!(alias)) {
                aliases.push(json!(alias));
            }
            edit(view, &mut edits, entity_id, "aliases", json!(aliases))?;
        }
        for predecessor in predecessors(view, op)? {
            edit(
                view,
                &mut edits,
                &predecessor,
                "wiki_status",
                json!("superseded"),
            )?;
            main_supersessions.push(MainDecisionSupersession {
                predecessor_id: predecessor,
                successor_id: a.decision_id.clone(),
            });
        }
        for r in remaps(op) {
            let target = target_id(r, &a)?;
            let target_path = record_paths
                .get(&target)
                .cloned()
                .or_else(|| find(&view.notes, &target).ok().map(|(p, _)| p.clone()))
                .ok_or_else(|| bad("missing target path"))?;
            record_paths.insert(target.clone(), target_path.clone());
            match r {
                EntityRemap::Assertion {
                    assertion_id,
                    field: f,
                    old_entity_id,
                    ..
                } => {
                    let (_, n) = find(&view.notes, assertion_id)?;
                    let record = n
                        .canonical
                        .as_ref()
                        .ok_or_else(|| bad("assertion invalid"))?;
                    edit(view, &mut edits, assertion_id, field(f), json!(target))?;
                    edit(
                        view,
                        &mut edits,
                        assertion_id,
                        companion(f),
                        json!(format!("[[{target_path}]]")),
                    )?;
                    if record.string("wiki_status") == Some("accepted") {
                        edit(
                            view,
                            &mut edits,
                            assertion_id,
                            "wiki_status",
                            json!("proposed"),
                        )?;
                    }
                    assertion_proofs.push(AssertionRemapProof {
                        assertion_id: assertion_id.clone(),
                        field: f.clone(),
                        before_entity_id: old_entity_id.clone(),
                        after_entity_id: target,
                        invariant_proposition_hash: invariant(record)?,
                        governing_decision_id: a.decision_id.clone(),
                    });
                }
                EntityRemap::Mention {
                    extraction_id,
                    mention_id,
                    ..
                } => {
                    let (_, n) = find(&view.notes, extraction_id)?;
                    let original = artifact(n)?;
                    let updated = artifacts
                        .entry(extraction_id.clone())
                        .or_insert_with(|| original.clone());
                    let m = a
                        .mention_decisions
                        .iter()
                        .find(|m| &m.extraction_id == extraction_id && &m.mention_id == mention_id)
                        .ok_or_else(|| bad("replacement allocation absent"))?;
                    let after = MentionBinding::Resolved {
                        entity_id: target,
                        decision_id: m.decision_id.clone(),
                    };
                    let before = original
                        .bindings
                        .get(mention_id)
                        .cloned()
                        .ok_or_else(|| bad("prior mention missing"))?;
                    updated.bindings.insert(mention_id.clone(), after.clone());
                    extraction_proofs
                        .entry(extraction_id.clone())
                        .or_insert(ExtractionRemapProof {
                            extraction_id: extraction_id.clone(),
                            immutable_extraction_hash: mention_state::immutable_hash(&original)?,
                            prior_bindings: original.bindings.clone(),
                            prior_materialized_assertions: original.materialized_assertions.clone(),
                            transitions: vec![],
                        })
                        .transitions
                        .push(MentionRemapProof {
                            mention_id: mention_id.clone(),
                            before,
                            after,
                            governing_decision_id: a.decision_id.clone(),
                        });
                    edit(
                        view,
                        &mut edits,
                        &m.predecessor_id,
                        "wiki_status",
                        json!("superseded"),
                    )?;
                }
            }
        }
        allocations.push(a);
    }
    for (id, fields) in edits {
        let (p, n) = find(&view.notes, &id)?;
        let bytes = edit_note(n, &fields, None, &n.source_hash)?;
        if bytes != n.raw {
            writes.insert(
                p.clone(),
                ExpectedWrite {
                    target: p.clone(),
                    expected: crate::vault::ExpectedState::Hash(n.source_hash.clone()),
                    proposed: Some(bytes),
                    apply_after: vec![],
                },
            );
        }
    }
    for (id, updated) in &artifacts {
        let (p, n) = find(&view.notes, id)?;
        writes.insert(
            p.clone(),
            ExpectedWrite {
                target: p.clone(),
                expected: crate::vault::ExpectedState::Hash(n.source_hash.clone()),
                proposed: Some(import::edit_extraction_artifact(n, updated)?),
                apply_after: vec![],
            },
        );
    }
    assertion_proofs.sort_by(|a, b| {
        (&a.assertion_id, field(&a.field)).cmp(&(&b.assertion_id, field(&b.field)))
    });
    for p in extraction_proofs.values_mut() {
        p.transitions
            .sort_by(|a, b| a.mention_id.cmp(&b.mention_id));
    }
    main_supersessions.sort_by(|a, b| {
        (&a.predecessor_id, &a.successor_id).cmp(&(&b.predecessor_id, &b.successor_id))
    });
    let operations = writes
        .values()
        .filter(|w| {
            w.proposed.as_ref().is_some_and(|b| {
                parse_note(b)
                    .canonical
                    .as_ref()
                    .is_some_and(|r| r.kind() != RecordKind::Decision)
            })
        })
        .map(|w| EntityDecisionWriteProof {
            target: w.target.clone(),
            before: w.expected.clone(),
            after: crate::vault::ExpectedState::Hash(Blake3Hash::digest(
                w.proposed.as_ref().expect("proof payload"),
            )),
        })
        .collect();
    let receipt = EntityDecisionReceiptV1 {
        schema: ENTITY_DECISION_RECEIPT_SCHEMA.into(),
        task_id: v.task_id.clone(),
        request: v.request.clone(),
        request_hash: v.request_hash.clone(),
        allocations,
        record_paths,
        extraction_proofs: extraction_proofs.into_values().collect(),
        assertion_proofs,
        main_supersessions,
        operations,
    };
    shape(&receipt)?;
    for (op, a) in receipt.request.decisions.iter().zip(&receipt.allocations) {
        let p = path(RecordKind::Decision, &a.decision_id)?;
        writes.insert(
            p.clone(),
            ExpectedWrite {
                target: p,
                expected: crate::vault::ExpectedState::Absent,
                proposed: Some(decision_bytes(op, a, &receipt, None)?),
                apply_after: vec![],
            },
        );
        for m in &a.mention_decisions {
            let p = path(RecordKind::Decision, &m.decision_id)?;
            writes.insert(
                p.clone(),
                ExpectedWrite {
                    target: p,
                    expected: crate::vault::ExpectedState::Absent,
                    proposed: Some(decision_bytes(op, a, &receipt, Some(m))?),
                    apply_after: vec![],
                },
            );
        }
    }
    let input = ValidationInput {
        vault_id: v.input.vault_id.clone(),
        documents: v.input.documents.clone(),
        overlay: writes
            .values()
            .map(|w| ProposedTarget {
                path: w.target.clone(),
                bytes: w.proposed.clone(),
            })
            .collect(),
    };
    super::remap::verify_remap_overlay(view.fs, &input, None)?
        .ok_or_else(|| bad("new decision lacks remap authority"))?;
    let closed = SourceView::from_closed_input(view.fs, &input)?;
    for id in artifacts.keys() {
        import::load_extraction(&closed, id)?;
    }
    crate::catalog::CatalogGraphValidator.validate_closed(view.fs, &input)?;
    Ok(ChangeDraft {
        title: "Explicit exhaustive entity decisions".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: allocation_map(&receipt.allocations),
        read_preconditions: v.dependencies.clone(),
        operations: writes.into_values().collect(),
    })
}

/// Construct an actual identity-remap history for disposable DEVELOPMENT probes.
/// The caller installs exact before-image checked bytes; no publication authority.
#[cfg(test)]
pub(crate) fn build_draft_for_remap_probe(
    view: &SourceView<'_>,
    v: &ValidatedEntityDecisions,
) -> Result<ChangeDraft> {
    build_draft(view, v)
}

fn retained_proposal(
    engine: &ChangeEngine,
    v: &ValidatedEntityDecisions,
    change: &ChangeInspection,
) -> Result<VerifiedEntityDecisionReceipt> {
    let mut input = v.input.clone();
    input.overlay.clear();
    for (i, op) in change.manifest.operations.iter().enumerate() {
        input.overlay.push(ProposedTarget {
            path: op.target.clone(),
            bytes: engine.verify_payload(
                &change.manifest.change_id,
                i,
                "proposed",
                &op.target,
                &op.after,
                &op.after_payload,
            )?,
        });
    }
    let view = SourceView::from_closed_input(engine.fs(), &input)?;
    let receipt = super::remap::load_entity_decision_receipt(&view, &v.task_id)?
        .ok_or_else(|| bad("retained entity decision receipt missing"))?;
    verify_retained(engine, change, &receipt.receipt)?;
    Ok(receipt)
}
fn verify_retained(
    engine: &ChangeEngine,
    change: &ChangeInspection,
    r: &EntityDecisionReceiptV1,
) -> Result<()> {
    if change.manifest.allocated_ids != allocation_map(&r.allocations) {
        return Err(bad("retained/canonical allocation maps differ"));
    }
    let mut operations = vec![];
    let mut new_decisions = BTreeSet::new();
    let mut old_decisions = BTreeSet::new();
    let required_predecessors = r
        .main_supersessions
        .iter()
        .map(|e| e.predecessor_id.clone())
        .chain(
            r.allocations
                .iter()
                .flat_map(|a| a.mention_decisions.iter().map(|m| m.predecessor_id.clone())),
        )
        .collect::<BTreeSet<_>>();
    let allocated = r
        .allocations
        .iter()
        .flat_map(|a| {
            std::iter::once(&a.decision_id)
                .chain(a.mention_decisions.iter().map(|m| &m.decision_id))
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    for (i, op) in change.manifest.operations.iter().enumerate() {
        let bytes = engine
            .verify_payload(
                &change.manifest.change_id,
                i,
                "proposed",
                &op.target,
                &op.after,
                &op.after_payload,
            )?
            .ok_or_else(|| bad("entity decision cannot retain deletions"))?;
        let note = parse_note(&bytes);
        let record = note
            .canonical
            .as_ref()
            .ok_or_else(|| bad("retained entity decision adopted envelope invalid"))?;
        if record.kind() == RecordKind::Decision {
            if allocated.contains(record.id()) {
                if op.before != crate::vault::ExpectedState::Absent
                    || super::remap::receipt(&note)? != *r
                    || r.record_paths.get(record.id()) != Some(&op.target)
                    || !new_decisions.insert(record.id().clone())
                {
                    return Err(bad("retained new decision identity/proof differs"));
                }
                let (requested, allocation) = r
                    .request
                    .decisions
                    .iter()
                    .zip(&r.allocations)
                    .find(|(_, a)| {
                        a.decision_id == *record.id()
                            || a.mention_decisions
                                .iter()
                                .any(|m| m.decision_id == *record.id())
                    })
                    .ok_or_else(|| bad("retained allocated decision not requested"))?;
                let mention = allocation
                    .mention_decisions
                    .iter()
                    .find(|m| m.decision_id == *record.id());
                let stamp = record
                    .string("wiki_created_at")
                    .ok_or_else(|| bad("retained decision creation time absent"))?;
                if decision_bytes_at(requested, allocation, r, mention, stamp)? != bytes {
                    return Err(bad("retained original decision envelope/body differs"));
                }
            } else if !required_predecessors.contains(record.id())
                || !old_decisions.insert(record.id().clone())
                || record.string("wiki_status") != Some("superseded")
                || !r.request.decisions.iter().flat_map(expected).any(|g| {
                    g.record_id == *record.id()
                        && op.before == crate::vault::ExpectedState::Hash(g.hash.clone())
                })
            {
                return Err(bad("retained predecessor guard/status differs"));
            } else {
                let before = engine
                    .verify_payload(
                        &change.manifest.change_id,
                        i,
                        "before",
                        &op.target,
                        &op.before,
                        &op.before_payload,
                    )?
                    .ok_or_else(|| bad("retained predecessor before-image absent"))?;
                let before = parse_note(&before);
                if before.canonical.as_ref().is_none_or(|old| {
                    old.kind() != RecordKind::Decision
                        || old.id() != record.id()
                        || old.string("wiki_status") != Some("active")
                }) || edit_note(
                    &before,
                    &BTreeMap::from([("wiki_status".into(), json!("superseded"))]),
                    None,
                    &before.source_hash,
                )? != bytes
                {
                    return Err(bad("retained predecessor is not exact status-only edit"));
                }
            }
        } else {
            operations.push(EntityDecisionWriteProof {
                target: op.target.clone(),
                before: op.before.clone(),
                after: op.after.clone(),
            });
        }
    }
    if new_decisions != allocated
        || old_decisions != required_predecessors
        || operations != r.operations
    {
        return Err(bad("retained exact operation membership differs"));
    }
    Ok(())
}
pub fn stage_entity_decisions(
    engine: &ChangeEngine,
    writer: &WriterPermit,
    v: &ValidatedEntityDecisions,
) -> Result<EntityDecisionOutcome> {
    writer.require_root(engine.fs().root())?;
    let physical = SourceView::from_fs_bounded(engine.fs(), packet::SOURCE_CAP, 4096)?;
    let current = validate_entity_decisions(&physical, &packet::canonical_json(&v.request)?)?;
    let origin = ChangeOrigin {
        operation: OriginOperation::GraphDecide,
        packet_id: v.task_id.clone(),
        response_hash: v.request_hash.clone(),
    };
    if let Some(r) = &current.restored_receipt {
        let mut retained = None;
        for id in engine.change_ids()? {
            let c = engine.inspect_history(&id)?;
            if c.manifest
                .origin
                .as_ref()
                .is_some_and(|o| o.operation == origin.operation && o.packet_id == origin.packet_id)
            {
                if c.manifest.origin.as_ref() != Some(&origin) {
                    return Err(conflict("different retained request for canonical scope"));
                }
                verify_retained(engine, &c, r)?;
                if retained.replace(c).is_some() {
                    return Err(bad("duplicate retained entity decision origins"));
                }
            }
        }
        let verified = super::remap::load_entity_decision_receipt(&physical, &v.task_id)?
            .ok_or_else(|| bad("canonical receipt disappeared"))?;
        return Ok(EntityDecisionOutcome {
            decisions: verified.decision_locators,
            prepared: retained.as_ref().map(|c| c.prepared.clone()),
            status: retained.as_ref().map(|c| c.status),
            disposition: if retained.is_some() {
                EntityDecisionDisposition::RetainedChange
            } else {
                EntityDecisionDisposition::CanonicalRestored
            },
            allocations: r.allocations.clone(),
            summary: summary(&current),
            reused: true,
        });
    }
    engine.plan(&ChangeDraft {
        title: "Recheck entity decision authority".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: v.dependencies.clone(),
        operations: vec![],
    })?;
    let closed = SourceView::from_closed_input(engine.fs(), &current.input)?;
    let prepared =
        engine.prepare_or_reuse(writer, origin, OriginPolicy::ReuseOrConflict, || {
            build_draft(&closed, &current)
        })?;
    let proposal = retained_proposal(engine, &current, &prepared.change)?;
    if proposal.receipt.request != current.request
        || proposal.receipt.request_hash != current.request_hash
    {
        return Err(bad("retained normalized request differs"));
    }
    Ok(EntityDecisionOutcome {
        decisions: proposal.decision_locators,
        prepared: Some(prepared.change.prepared),
        status: Some(prepared.change.status),
        disposition: EntityDecisionDisposition::RetainedChange,
        allocations: proposal.receipt.allocations,
        summary: summary(&current),
        reused: prepared.reused,
    })
}
