//! Strict durable resolution receipts and source-local binding state.
use super::{extraction_types::*, import, packet::*, resolution_types::*};
use crate::{domain::*, records::ParsedNote, sources::SourceView, vault::ExpectedState};
use pulldown_cmark::{CodeBlockKind, Event, Tag};
use serde_json::json;
use std::collections::BTreeSet;

pub const RESOLUTION_FENCE: &str = "lwiki-graph-resolution-v1";

pub(crate) fn mention(mapping: &ResolutionMapping) -> &PacketLocalId {
    match mapping {
        ResolutionMapping::BindMention { mention_id, .. }
        | ResolutionMapping::CreateEntity { mention_id, .. }
        | ResolutionMapping::RejectMention { mention_id, .. } => mention_id,
    }
}
pub(crate) fn reason(mapping: &ResolutionMapping) -> &str {
    match mapping {
        ResolutionMapping::BindMention { reason, .. }
        | ResolutionMapping::CreateEntity { reason, .. }
        | ResolutionMapping::RejectMention { reason, .. } => reason,
    }
}
pub(crate) fn action(mapping: &ResolutionMapping) -> &'static str {
    match mapping {
        ResolutionMapping::BindMention { .. } => "bind_mention",
        ResolutionMapping::CreateEntity { .. } => "create_entity",
        ResolutionMapping::RejectMention { .. } => "reject_mention",
    }
}
pub(crate) fn normalize(mut request: ResolutionRequest) -> Result<ResolutionRequest> {
    if request.schema != RESOLUTION_SCHEMA
        || request.mappings.is_empty()
        || request.mappings.len() > MAX_RESOLUTION_MAPPINGS
    {
        return Err(invalid("resolution schema or mapping count is invalid"));
    }
    request.mappings.sort_by(|a, b| mention(a).cmp(mention(b)));
    let mut previous = None;
    for mapping in &request.mappings {
        if previous == Some(mention(mapping)) {
            return Err(invalid("duplicate resolution mention"));
        }
        previous = Some(mention(mapping));
        string_bound(reason(mapping), MAX_RESOLUTION_REASON_BYTES, true)?;
        if let ResolutionMapping::CreateEntity {
            title,
            entity_type: kind,
            ..
        } = mapping
        {
            string_bound(title, 1024, true)?;
            string_bound(kind, 64, true)?;
            entity_type(kind)?;
        }
    }
    if canonical_json(&request)?.len() > MAX_RESOLUTION_BYTES {
        return Err(invalid("normalized resolution request exceeds ceiling"));
    }
    Ok(request)
}
pub(crate) fn identity(request: &ResolutionRequest) -> Result<(RecordId, Blake3Hash)> {
    let scope = Blake3Hash::digest(&canonical_json(&json!({
        "extraction_id":request.extraction_id,"expected_hash":request.expected_hash
    }))?);
    Ok((
        RecordId::new(format!("resolution_{}", scope.hex()))?,
        Blake3Hash::digest(&canonical_json(request)?),
    ))
}
pub(crate) fn immutable_hash(artifact: &ExtractionArtifactV1) -> Result<Blake3Hash> {
    let mut value = serde_json::to_value(artifact).map_err(|e| invalid(e.to_string()))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| invalid("invalid extraction artifact"))?;
    object.remove("bindings");
    object.remove("materialized_assertions");
    Ok(Blake3Hash::digest(&canonical_json(&value)?))
}
pub(crate) fn receipt(note: &ParsedNote) -> Result<ResolutionReceiptV1> {
    decode(
        fenced_json(note, RESOLUTION_FENCE, MAX_RESOLUTION_RECEIPT_BYTES)?,
        MAX_RESOLUTION_RECEIPT_BYTES,
    )
}
pub(crate) fn expected_binding(
    mapping: &ResolutionMapping,
    ids: &ResolutionAllocations,
) -> Result<MentionBinding> {
    let local = mention(mapping);
    let decision_id = ids
        .decisions
        .get(local)
        .ok_or_else(|| invalid("missing decision allocation"))?
        .clone();
    Ok(match mapping {
        ResolutionMapping::BindMention { entity_id, .. } => MentionBinding::Resolved {
            entity_id: entity_id.clone(),
            decision_id,
        },
        ResolutionMapping::CreateEntity { .. } => MentionBinding::Resolved {
            entity_id: ids
                .entities
                .get(local)
                .ok_or_else(|| invalid("missing created entity allocation"))?
                .clone(),
            decision_id,
        },
        ResolutionMapping::RejectMention { .. } => MentionBinding::Rejected { decision_id },
    })
}
pub(crate) fn validate_receipt_shape(
    receipt: &ResolutionReceiptV1,
    artifact: &ExtractionArtifactV1,
) -> Result<()> {
    let request = normalize(receipt.request.clone())?;
    let (task, hash) = identity(&request)?;
    if request != receipt.request
        || receipt.schema != RESOLUTION_RECEIPT_SCHEMA
        || task != receipt.task_id
        || hash != receipt.request_hash
        || request.extraction_id != artifact.extraction_id
        || immutable_hash(artifact)? != receipt.immutable_extraction_hash
    {
        return Err(invalid(
            "resolution receipt identity/immutable proof mismatch",
        ));
    }
    let keys: BTreeSet<_> = request
        .mappings
        .iter()
        .map(|m| mention(m).clone())
        .collect();
    let creates: BTreeSet<_> = request
        .mappings
        .iter()
        .filter(|m| matches!(m, ResolutionMapping::CreateEntity { .. }))
        .map(|m| mention(m).clone())
        .collect();
    if receipt
        .allocations
        .decisions
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        != keys
        || receipt
            .allocations
            .entities
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
            != creates
        || receipt.transitions.keys().cloned().collect::<BTreeSet<_>>() != keys
        || keys.iter().any(|k| !artifact.bindings.contains_key(k))
    {
        return Err(invalid(
            "resolution receipt has incomplete allocation/transition maps",
        ));
    }
    historical_state(receipt, artifact)?;
    let mut ids: BTreeSet<_> = std::iter::once(&artifact.extraction_id)
        .chain(artifact.allocations.assertions.values())
        .chain(artifact.allocations.evidence.values().flatten())
        .cloned()
        .collect();
    for id in receipt
        .allocations
        .decisions
        .values()
        .chain(receipt.allocations.entities.values())
    {
        if !ids.insert(id.clone()) {
            return Err(invalid("resolution allocation IDs overlap"));
        }
    }
    for mapping in &request.mappings {
        let transition = &receipt.transitions[mention(mapping)];
        if transition.before != MentionBinding::Pending
            || transition.after != expected_binding(mapping, &receipt.allocations)?
            || artifact.bindings[mention(mapping)] != transition.after
        {
            return Err(invalid(
                "receipt disagrees with explicit current mention binding",
            ));
        }
    }
    let set: BTreeSet<_> = receipt.materialized_assertions.iter().collect();
    if set.len() != receipt.materialized_assertions.len()
        || set.len() > 128
        || receipt
            .materialized_assertions
            .windows(2)
            .any(|w| w[0] >= w[1])
        || set
            .iter()
            .any(|id| !artifact.materialized_assertions.contains(id))
        || receipt.operations.is_empty()
        || receipt.operations.len() > 705
        || receipt
            .operations
            .windows(2)
            .any(|w| w[0].target >= w[1].target)
        || receipt
            .operations
            .iter()
            .any(|op| !matches!(op.after, ExpectedState::Hash(_)))
    {
        return Err(invalid(
            "receipt materialization/operation proof is invalid",
        ));
    }
    Ok(())
}

pub(crate) fn historical_state(
    receipt: &ResolutionReceiptV1,
    current: &ExtractionArtifactV1,
) -> Result<ExtractionArtifactV1> {
    if receipt.prior_bindings.keys().ne(current.bindings.keys())
        || receipt.prior_materialized_assertions.len() > 128
        || receipt
            .prior_materialized_assertions
            .windows(2)
            .any(|w| w[0] >= w[1])
    {
        return Err(invalid(
            "receipt prior semantic map is incomplete or noncanonical",
        ));
    }
    let mut before = current.clone();
    before.bindings = receipt.prior_bindings.clone();
    before.materialized_assertions = receipt.prior_materialized_assertions.clone();
    let mut after = before.clone();
    for (local, transition) in &receipt.transitions {
        if before.bindings.get(local) != Some(&transition.before)
            || transition.before != MentionBinding::Pending
        {
            return Err(invalid(
                "receipt transition disagrees with complete prior semantic map",
            ));
        }
        after
            .bindings
            .insert(local.clone(), transition.after.clone());
    }
    after
        .materialized_assertions
        .extend(receipt.materialized_assertions.clone());
    after.materialized_assertions.sort();
    import::verify_resolution_transition(&before, &after)?;
    // Already decided IDs/materializations cannot change; later independent
    // Pending transitions remain valid and are proven by the current loader.
    import::verify_resolution_transition(&after, current)?;
    let mut ids: BTreeSet<_> = [
        &after.extraction_id,
        &after.packet_id,
        &after.source_id,
        &after.source_revision,
    ]
    .into_iter()
    .cloned()
    .collect();
    ids.extend(after.allocations.assertions.values().cloned());
    ids.extend(after.allocations.evidence.values().flatten().cloned());
    for binding in after.bindings.values() {
        match binding {
            MentionBinding::Resolved {
                entity_id,
                decision_id,
            } => {
                ids.insert(entity_id.clone());
                ids.insert(decision_id.clone());
            }
            MentionBinding::Rejected { decision_id } => {
                ids.insert(decision_id.clone());
            }
            MentionBinding::Pending => {}
        }
    }
    if receipt.record_paths.len() > 772
        || receipt
            .record_paths
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
            != ids
        || receipt.record_paths.values().collect::<BTreeSet<_>>().len() != ids.len()
        || receipt
            .record_paths
            .values()
            .any(|path| !crate::sources::revision::canonical_path(path))
    {
        return Err(invalid(
            "receipt historical record paths have incomplete or overlapping membership",
        ));
    }
    Ok(after)
}

fn has_receipt_fence(note: &ParsedNote) -> Result<bool> {
    if note.raw.len() > MAX_RESOLUTION_RECEIPT_BYTES + 262144 {
        return Err(invalid("resolution decision exceeds note ceiling"));
    }
    let text = std::str::from_utf8(note.body())
        .map_err(|_| invalid("resolution decision body is not UTF-8"))?;
    Ok(pulldown_cmark::Parser::new(text).any(|event|matches!(event,Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(ref info))) if info.as_ref()==RESOLUTION_FENCE)))
}

pub(crate) fn verify_decision_note(
    note: &ParsedNote,
    mapping: &ResolutionMapping,
    receipt: &ResolutionReceiptV1,
) -> Result<()> {
    let local = mention(mapping);
    let record = note
        .canonical
        .as_ref()
        .ok_or_else(|| invalid("invalid decision envelope"))?;
    let outputs = match &receipt.transitions[local].after {
        MentionBinding::Resolved { entity_id, .. } => vec![entity_id.as_str()],
        MentionBinding::Rejected { .. } => vec![],
        MentionBinding::Pending => return Err(invalid("pending receipt outcome")),
    };
    if record.kind() != RecordKind::Decision
        || record.id() != &receipt.allocations.decisions[local]
        || record.string("wiki_status") != Some("active")
        || record.string("wiki_action") != Some(action(mapping))
        || record.string("wiki_extraction_id") != Some(receipt.request.extraction_id.as_str())
        || record.field("wiki_mention_ids") != Some(&json!([local]))
        || record.field("wiki_input_ids") != Some(&json!([receipt.request.extraction_id]))
        || record.field("wiki_output_ids") != Some(&json!(outputs))
        || self::receipt(note)? != *receipt
    {
        return Err(invalid(
            "decision envelope disagrees with resolution receipt",
        ));
    }
    Ok(())
}

/// Restore an explicit resolution batch from canonical Markdown without inventing a journal.
pub fn load_resolution_receipt(
    view: &SourceView<'_>,
    task_id: &RecordId,
) -> Result<Option<VerifiedResolutionReceipt>> {
    load_resolution_receipt_scoped(view, task_id, None)
}
pub(crate) fn load_resolution_receipt_scoped(
    view: &SourceView<'_>,
    task_id: &RecordId,
    extraction_scope: Option<&RecordId>,
) -> Result<Option<VerifiedResolutionReceipt>> {
    let mut candidate = None;
    for note in view.notes.values() {
        let relevant = note.canonical.as_ref().is_some_and(|r| {
            r.kind() == RecordKind::Decision
                && matches!(
                    r.string("wiki_action"),
                    Some("bind_mention" | "create_entity" | "reject_mention")
                )
                && r.string("wiki_extraction_id").is_some()
                && extraction_scope
                    .is_none_or(|id| r.string("wiki_extraction_id") == Some(id.as_str()))
        });
        if relevant && has_receipt_fence(note)? {
            let parsed = receipt(note)?;
            if &parsed.task_id == task_id {
                if candidate.as_ref().is_some_and(|old| old != &parsed) {
                    return Err(invalid("contradictory canonical resolution receipts"));
                }
                candidate = Some(parsed);
            }
        }
    }
    let Some(receipt) = candidate else {
        return Ok(None);
    };
    let extraction = import::load_extraction(view, &receipt.request.extraction_id)?;
    validate_receipt_shape(&receipt, &extraction.artifact)?;
    let mut deps = dependency_map(&extraction.dependencies)?;
    let mut locators = vec![];
    for mapping in &receipt.request.mappings {
        let local = mention(mapping);
        let decision_id = &receipt.allocations.decisions[local];
        let (path, note) = view.resolve(decision_id, RecordKind::Decision, None)?;
        verify_decision_note(note, mapping, &receipt)?;
        SourceView::note_dependency(path, note, &mut deps);
        locators.push(locator(view, path, note)?);
        if let MentionBinding::Resolved { entity_id, .. } = &receipt.transitions[local].after {
            let (path, note) = view.resolve(entity_id, RecordKind::Entity, None)?;
            if note
                .canonical
                .as_ref()
                .and_then(|r| r.string("wiki_status"))
                != Some("active")
            {
                return Err(invalid(
                    "resolved identity is no longer active; explicit remap required",
                ));
            }
            SourceView::note_dependency(path, note, &mut deps);
        }
        if let ResolutionMapping::CreateEntity { entity_type, .. } = mapping {
            let (path, note) = view.resolve(
                &receipt.allocations.entities[local],
                RecordKind::Entity,
                None,
            )?;
            let record = note
                .canonical
                .as_ref()
                .ok_or_else(|| invalid("invalid created entity"))?;
            if record.string("wiki_status") != Some("active")
                || record.string("wiki_entity_type") != Some(entity_type)
            {
                return Err(invalid(
                    "created identity type/status disagrees with explicit decision",
                ));
            }
            SourceView::note_dependency(path, note, &mut deps);
        }
    }
    super::resolution::verify_operation_proofs(view, &receipt, &extraction)?;
    Ok(Some(VerifiedResolutionReceipt {
        receipt,
        extraction,
        dependencies: dependencies(deps),
        decision_locators: locators,
    }))
}
