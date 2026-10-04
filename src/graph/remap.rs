//! Strict exhaustive identity-remap proof. No redirect traversal or inferred identity.
use super::receipt_budget::ReceiptBudget;
use super::{decision_types::*, extraction_types::*, import, mention_state, packet, wire};
use crate::{
    changes::{OriginOperation, RetainedGraphInput, RetainedGraphInverseInput, ValidationInput},
    domain::*,
    records::ParsedNote,
    sources::SourceView,
    vault::VaultFs,
};
use pulldown_cmark::{CodeBlockKind, Event, Tag};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
type EnvelopePolicy = (BTreeSet<RecordId>, BTreeSet<(RecordId, RecordId)>);

pub(crate) fn bad(message: impl Into<String>) -> WikiError {
    WikiError::invalid(message)
}
pub(crate) fn conflict(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
pub(crate) fn reason(op: &EntityDecision) -> &str {
    match op {
        EntityDecision::MergeEntities { reason, .. }
        | EntityDecision::SplitEntity { reason, .. }
        | EntityDecision::AddAlias { reason, .. } => reason,
    }
}
pub(crate) fn expected(op: &EntityDecision) -> &[ExpectedRecord] {
    match op {
        EntityDecision::MergeEntities {
            expected_records, ..
        }
        | EntityDecision::SplitEntity {
            expected_records, ..
        }
        | EntityDecision::AddAlias {
            expected_records, ..
        } => expected_records,
    }
}
pub(crate) fn remaps(op: &EntityDecision) -> &[EntityRemap] {
    match op {
        EntityDecision::MergeEntities { remaps, .. }
        | EntityDecision::SplitEntity { remaps, .. }
        | EntityDecision::AddAlias { remaps, .. } => remaps,
    }
}
pub(crate) fn sources(op: &EntityDecision) -> BTreeSet<RecordId> {
    match op {
        EntityDecision::MergeEntities { source_ids, .. } => source_ids.iter().cloned().collect(),
        EntityDecision::SplitEntity { source_id, .. } => BTreeSet::from([source_id.clone()]),
        EntityDecision::AddAlias { .. } => BTreeSet::new(),
    }
}
pub(crate) fn action(op: &EntityDecision) -> &str {
    match op {
        EntityDecision::MergeEntities { .. } => "merge",
        EntityDecision::SplitEntity { .. } => "split",
        EntityDecision::AddAlias { .. } => "add_alias",
    }
}
pub(crate) fn field(field: &AssertionEntityField) -> &str {
    match field {
        AssertionEntityField::SubjectId => "wiki_subject_id",
        AssertionEntityField::ObjectId => "wiki_object_id",
    }
}
pub(crate) fn companion(field: &AssertionEntityField) -> &str {
    match field {
        AssertionEntityField::SubjectId => "wiki_subject",
        AssertionEntityField::ObjectId => "wiki_object",
    }
}
pub(crate) fn remap_key(r: &EntityRemap) -> String {
    match r {
        EntityRemap::Assertion {
            assertion_id,
            field: f,
            ..
        } => format!("a:{assertion_id}:{}", field(f)),
        EntityRemap::Mention {
            extraction_id,
            mention_id,
            ..
        } => format!("m:{extraction_id}:{mention_id}"),
    }
}
fn target(r: &EntityRemap) -> &RemapTarget {
    match r {
        EntityRemap::Assertion { target, .. } | EntityRemap::Mention { target, .. } => target,
    }
}
pub(crate) fn old(r: &EntityRemap) -> &RecordId {
    match r {
        EntityRemap::Assertion { old_entity_id, .. }
        | EntityRemap::Mention { old_entity_id, .. } => old_entity_id,
    }
}

/// Root preflight: strict parser still supplies duplicate/null/depth/node checks.
pub(crate) fn normalize(mut request: EntityDecisionRequest) -> Result<EntityDecisionRequest> {
    if request.schema != ENTITY_DECISIONS_SCHEMA
        || request.decisions.is_empty()
        || request.decisions.len() > MAX_ENTITY_DECISIONS
    {
        return Err(bad("unsupported entity decision schema/count"));
    }
    let mut total_remaps = 0usize;
    let mut total_records = 0usize;
    let mut references = BTreeSet::new();
    let mut entity_scopes = BTreeSet::new();
    for op in &mut request.decisions {
        if reason(op).trim().is_empty() || reason(op).len() > 4096 {
            return Err(bad("decision reason empty/oversized"));
        }
        let (guards, mappings) = match op {
            EntityDecision::MergeEntities {
                expected_records,
                remaps,
                ..
            }
            | EntityDecision::SplitEntity {
                expected_records,
                remaps,
                ..
            }
            | EntityDecision::AddAlias {
                expected_records,
                remaps,
                ..
            } => (expected_records, remaps),
        };
        total_remaps = total_remaps
            .checked_add(mappings.len())
            .ok_or_else(|| bad("remap count overflow"))?;
        total_records = total_records
            .checked_add(guards.len())
            .ok_or_else(|| bad("guard count overflow"))?;
        if total_remaps > MAX_ENTITY_REMAPS || total_records > MAX_ENTITY_EXPECTED_RECORDS {
            return Err(bad("decision remap/guard ceiling exceeded"));
        }
        guards.sort_by(|a, b| a.record_id.cmp(&b.record_id));
        if guards.is_empty() || guards.windows(2).any(|w| w[0].record_id == w[1].record_id) {
            return Err(bad("missing/duplicate expected record"));
        }
        mappings.sort_by_key(remap_key);
        for r in mappings.iter() {
            if !references.insert(remap_key(r)) {
                return Err(bad("duplicate remap or nonentity old endpoint"));
            }
        }
        let mut scope = sources(op);
        match op {
            EntityDecision::MergeEntities {
                source_ids,
                target_id,
                ..
            } => {
                if source_ids.is_empty() || source_ids.len() > 16 {
                    return Err(bad("invalid merge sources/target"));
                }
                source_ids.sort();
                if source_ids.windows(2).any(|w| w[0] == w[1]) || source_ids.contains(target_id) {
                    return Err(bad(
                        "merge sources must be absorbed distinct entity IDs excluding target",
                    ));
                }
                scope.insert(target_id.clone());
            }
            EntityDecision::SplitEntity {
                source_id: _,
                new_entities,
                ..
            } => {
                if !(2..=16).contains(&new_entities.len()) {
                    return Err(bad("invalid split source/new entity count"));
                }
                new_entities.sort_by(|a, b| a.key.cmp(&b.key));
                if new_entities.windows(2).any(|w| w[0].key == w[1].key) {
                    return Err(bad("duplicate split key"));
                }
                for e in new_entities {
                    if e.title.trim().is_empty()
                        || e.title.len() > 1024
                        || !matches!(
                            e.entity_type.as_str(),
                            "person"
                                | "organization"
                                | "project"
                                | "component"
                                | "concept"
                                | "place"
                                | "event"
                                | "other"
                        )
                    {
                        return Err(bad("invalid split title/entity type"));
                    }
                }
            }
            EntityDecision::AddAlias {
                entity_id,
                alias,
                remaps,
                ..
            } => {
                if alias.trim().is_empty() || alias.len() > 1024 || !remaps.is_empty() {
                    return Err(bad("alias requires entity, bounded alias and empty remaps"));
                }
                scope.insert(entity_id.clone());
            }
        }
        for id in scope {
            if !entity_scopes.insert(id) {
                return Err(bad("overlapping entity operation scopes"));
            }
        }
        for r in remaps(op) {
            match (&*op, target(r)) {
                (
                    EntityDecision::MergeEntities { target_id, .. },
                    RemapTarget::ExistingEntity { entity_id },
                ) if entity_id == target_id
                    || (matches!(r, EntityRemap::Mention { .. }) && entity_id == old(r)) => {}
                (
                    EntityDecision::SplitEntity { new_entities, .. },
                    RemapTarget::NewEntity { key },
                ) if new_entities.iter().any(|e| &e.key == key) => {}
                (EntityDecision::SplitEntity { .. }, RemapTarget::ExistingEntity { entity_id })
                    if matches!(r, EntityRemap::Mention { .. }) && entity_id == old(r) => {}
                _ => return Err(bad("remap target not authorized by operation")),
            }
        }
    }
    let mut keyed = request
        .decisions
        .into_iter()
        .map(|op| Ok((packet::canonical_json(&op)?, op)))
        .collect::<Result<Vec<_>>>()?;
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    request.decisions = keyed.into_iter().map(|(_, op)| op).collect();
    if packet::canonical_json(&request)?.len() > MAX_ENTITY_DECISION_BYTES {
        return Err(bad("normalized decision request exceeds ceiling"));
    }
    Ok(request)
}
pub(crate) fn identity(request: &EntityDecisionRequest) -> Result<(RecordId, Blake3Hash)> {
    let hash = Blake3Hash::digest(&packet::canonical_json(request)?);
    let scopes = request
        .decisions
        .iter()
        .map(
            |op| json!({"action":action(op),"sources":sources(op),"expected_records":expected(op)}),
        )
        .collect::<Vec<_>>();
    let task = Blake3Hash::digest(&packet::canonical_json(&scopes)?);
    Ok((
        RecordId::new(format!(
            "entity_decisions_{}",
            task.as_str()
                .strip_prefix("blake3:")
                .ok_or_else(|| bad("hash prefix"))?
        ))?,
        hash,
    ))
}
pub(crate) fn id_list(record: &CanonicalRecord, key: &str) -> Result<Vec<RecordId>> {
    record
        .field(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| bad(format!("missing ID list {key}")))?
        .iter()
        .map(|v| {
            RecordId::new(
                v.as_str()
                    .ok_or_else(|| bad("ID list item is not string"))?,
            )
        })
        .collect()
}
pub(crate) fn find<'a>(
    notes: &'a BTreeMap<VaultRelativePath, ParsedNote>,
    id: &RecordId,
) -> Result<(&'a VaultRelativePath, &'a ParsedNote)> {
    let mut found = None;
    for (path, note) in notes {
        if note.canonical.as_ref().is_some_and(|r| r.id() == id)
            && found.replace((path, note)).is_some()
        {
            return Err(bad("duplicate canonical ID"));
        }
    }
    found.ok_or_else(|| bad(format!("missing canonical record {id}")))
}
pub(crate) fn artifact(note: &ParsedNote) -> Result<ExtractionArtifactV1> {
    let a: ExtractionArtifactV1 = packet::decode(
        packet::fenced_json(note, import::ARTIFACT_FENCE, MAX_ARTIFACT_BYTES)?,
        MAX_ARTIFACT_BYTES,
    )?;
    let record = note
        .canonical
        .as_ref()
        .ok_or_else(|| bad("invalid extraction envelope"))?;
    let response: ExtractionResponse =
        packet::decode(a.raw_response.as_bytes(), MAX_RESPONSE_BYTES)?;
    wire::artifact_membership(&a, &response)?;
    if a.schema != EXTRACTION_STATE_SCHEMA
        || record.kind() != RecordKind::Extraction
        || record.id() != &a.extraction_id
        || record.string("wiki_packet_id") != Some(a.packet_id.as_str())
        || record.string("wiki_input_hash") != Some(a.packet_fingerprint.as_str())
        || record.field("wiki_source_ids") != Some(&json!([a.source_id]))
        || record.field("wiki_source_revision_ids") != Some(&json!([a.source_revision]))
    {
        return Err(bad("extraction envelope/raw membership differs"));
    }
    Ok(a)
}
pub(crate) fn invariant(record: &CanonicalRecord) -> Result<Blake3Hash> {
    let fields = record
        .fields()
        .iter()
        .filter(|(k, _)| {
            matches!(
                k.as_str(),
                "wiki_predicate"
                    | "wiki_object_kind"
                    | "wiki_literal_type"
                    | "wiki_literal_value"
                    | "wiki_property"
                    | "wiki_negated"
                    | "wiki_modality"
                    | "wiki_unit"
                    | "wiki_valid_from"
                    | "wiki_valid_until"
            )
        })
        .collect::<BTreeMap<_, _>>();
    Ok(Blake3Hash::digest(&packet::canonical_json(&fields)?))
}
pub(crate) fn has_fence(note: &ParsedNote) -> Result<bool> {
    if note.raw.len() > MAX_ENTITY_DECISION_RECEIPT_BYTES + 262144 {
        return Err(bad("entity decision note exceeds ceiling"));
    }
    fence_present(note)
}
fn fence_present(note: &ParsedNote) -> Result<bool> {
    let text = std::str::from_utf8(note.body()).map_err(|_| bad("decision body not UTF8"))?;
    Ok(pulldown_cmark::Parser::new(text).any(|e|matches!(e,Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) if info.as_ref()==ENTITY_DECISION_FENCE)))
}
pub(crate) fn receipt(note: &ParsedNote) -> Result<EntityDecisionReceiptV1> {
    packet::decode(
        packet::fenced_json(
            note,
            ENTITY_DECISION_FENCE,
            MAX_ENTITY_DECISION_RECEIPT_BYTES,
        )?,
        MAX_ENTITY_DECISION_RECEIPT_BYTES,
    )
}

#[derive(Debug, Clone)]
pub struct VerifiedDecisionPolicy {
    edges: BTreeSet<(RecordId, RecordId)>,
    families: Vec<BTreeSet<RecordId>>,
    receipts: BTreeMap<RecordId, EntityDecisionReceiptV1>,
    aliases: BTreeSet<RecordId>,
}
impl VerifiedDecisionPolicy {
    pub fn historical_alias_authority(&self, id: &RecordId) -> bool {
        self.aliases.contains(id)
    }
    pub fn supersession_edges(&self) -> &BTreeSet<(RecordId, RecordId)> {
        &self.edges
    }
    pub fn decisions_compatible(&self, ids: &[RecordId]) -> bool {
        ids.len() > 1
            && self
                .families
                .iter()
                .any(|family| ids.iter().all(|id| family.contains(id)))
    }
}
#[derive(Debug, Clone)]
pub struct VerifiedRemapOverlay {
    assertions: BTreeSet<RecordId>,
    policy: VerifiedDecisionPolicy,
    requires_retained: bool,
}
impl VerifiedRemapOverlay {
    pub(crate) fn requires_retained_input(&self) -> bool {
        self.requires_retained
    }
    pub fn authorized_assertions(&self) -> &BTreeSet<RecordId> {
        &self.assertions
    }
    pub fn supersession_edges(&self) -> &BTreeSet<(RecordId, RecordId)> {
        self.policy.supersession_edges()
    }
    pub fn decisions_compatible(&self, ids: &[RecordId]) -> bool {
        self.policy.decisions_compatible(ids)
    }
}

pub fn relevant_decision_ids(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
) -> BTreeSet<RecordId> {
    notes
        .values()
        .filter_map(|note| {
            note.canonical
                .as_ref()
                .filter(|r| {
                    r.kind() == RecordKind::Decision
                        && matches!(
                            r.string("wiki_action"),
                            Some("merge" | "split" | "add_alias" | "bind_mention")
                        )
                })
                .and_then(|r| has_fence(note).unwrap_or(true).then(|| r.id().clone()))
        })
        .collect()
}

// Remaining proof constructors are deliberately implemented below; all public
// hooks return sealed proofs rather than trusting deserialized receipt claims.

pub(crate) fn reference_set(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    entities: &BTreeSet<RecordId>,
) -> Result<BTreeMap<String, RecordId>> {
    reference_set_scoped(notes, entities, &mut ReceiptBudget::default())
}
fn reference_set_scoped(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    entities: &BTreeSet<RecordId>,
    budget: &mut ReceiptBudget,
) -> Result<BTreeMap<String, RecordId>> {
    let mut found = BTreeMap::new();
    if entities.is_empty() {
        return Ok(found);
    }
    for (path, note) in notes {
        let Some(record) = &note.canonical else {
            if note.fields.as_ref().is_some_and(|f| {
                ["wiki_subject_id", "wiki_object_id"].iter().any(|k| {
                    f.get(*k)
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| entities.iter().any(|id| id.as_str() == s))
                })
            }) {
                budget.admit(path, note)?;
                return Err(bad("malformed referring canonical envelope"));
            }
            continue;
        };
        match record.kind() {
            RecordKind::Assertion => {
                for f in [
                    AssertionEntityField::SubjectId,
                    AssertionEntityField::ObjectId,
                ] {
                    if let Some(id) = record.string(field(&f)).map(RecordId::new).transpose()?
                        && entities.contains(&id)
                    {
                        budget.admit(path, note)?;
                        found.insert(
                            format!("a:{}:{}", record.id(), field(&f)),
                            record.id().clone(),
                        );
                    }
                }
            }
            RecordKind::Extraction => {
                // Every Extraction is a negative-proof witness: its retained
                // bindings may still refer to the remapped identity.
                budget.admit(path, note)?;
                let a = artifact(note)?;
                for (local, binding) in &a.bindings {
                    if let MentionBinding::Resolved { entity_id, .. } = binding
                        && entities.contains(entity_id)
                    {
                        found.insert(
                            format!("m:{}:{local}", a.extraction_id),
                            a.extraction_id.clone(),
                        );
                    }
                }
            }
            _ => {}
        }
        if found.len() > MAX_ENTITY_REMAPS {
            return Err(bad("exhaustive reference set exceeds remap ceiling"));
        }
    }
    Ok(found)
}
pub(crate) fn output_ids(
    op: &EntityDecision,
    allocation: &EntityDecisionAllocation,
) -> Vec<RecordId> {
    match op {
        EntityDecision::MergeEntities { target_id, .. } => vec![target_id.clone()],
        EntityDecision::SplitEntity { .. } => allocation.entities.values().cloned().collect(),
        EntityDecision::AddAlias { entity_id, .. } => vec![entity_id.clone()],
    }
}
pub(crate) fn target_id(
    r: &EntityRemap,
    allocation: &EntityDecisionAllocation,
) -> Result<RecordId> {
    match target(r) {
        RemapTarget::ExistingEntity { entity_id } => Ok(entity_id.clone()),
        RemapTarget::NewEntity { key } => allocation
            .entities
            .get(key)
            .cloned()
            .ok_or_else(|| bad("unallocated split target key")),
    }
}

pub(crate) fn shape(receipt: &EntityDecisionReceiptV1) -> Result<()> {
    let normalized = normalize(receipt.request.clone())?;
    let (task, hash) = identity(&normalized)?;
    if receipt.schema != ENTITY_DECISION_RECEIPT_SCHEMA
        || normalized != receipt.request
        || task != receipt.task_id
        || hash != receipt.request_hash
        || receipt.allocations.len() != receipt.request.decisions.len()
        || receipt.operations.len() > crate::changes::prepare::MAX_OPS
        || receipt.record_paths.len() > crate::changes::prepare::MAX_OPS
        || receipt
            .operations
            .windows(2)
            .any(|w| w[0].target >= w[1].target)
        || receipt
            .operations
            .iter()
            .any(|p| !matches!(p.after, crate::vault::ExpectedState::Hash(_)))
    {
        return Err(bad("entity receipt version/hash/count/order differs"));
    }
    let mut allocated = BTreeSet::new();
    let mut mentions = BTreeSet::new();
    for (op, a) in receipt.request.decisions.iter().zip(&receipt.allocations) {
        if !allocated.insert(a.decision_id.clone()) {
            return Err(bad("duplicate allocated main decision"));
        }
        let expected_keys = match op {
            EntityDecision::SplitEntity { new_entities, .. } => {
                new_entities.iter().map(|n| n.key.clone()).collect()
            }
            _ => BTreeSet::new(),
        };
        if a.entities.keys().cloned().collect::<BTreeSet<_>>() != expected_keys {
            return Err(bad("split allocation keys differ"));
        }
        for id in a.entities.values() {
            if !allocated.insert(id.clone()) {
                return Err(bad("duplicate allocated entity"));
            }
        }
        let requested = remaps(op)
            .iter()
            .filter_map(|r| match r {
                EntityRemap::Mention {
                    extraction_id,
                    mention_id,
                    ..
                } => Some((extraction_id.clone(), mention_id.clone())),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let actual = a
            .mention_decisions
            .iter()
            .map(|m| (m.extraction_id.clone(), m.mention_id.clone()))
            .collect::<BTreeSet<_>>();
        if requested != actual || actual.len() != a.mention_decisions.len() {
            return Err(bad("mention replacement allocation map differs"));
        }
        for m in &a.mention_decisions {
            if !allocated.insert(m.decision_id.clone())
                || !mentions.insert((m.extraction_id.clone(), m.mention_id.clone()))
            {
                return Err(bad("duplicate replacement allocation"));
            }
        }
    }
    let mut paths = BTreeSet::new();
    for p in receipt.record_paths.values() {
        if !crate::sources::revision::canonical_path(p) || !paths.insert(p) {
            return Err(bad("receipt original paths escape/overlap"));
        }
    }
    if allocated
        .iter()
        .any(|id| !receipt.record_paths.contains_key(id))
    {
        return Err(bad("receipt lacks allocated original path"));
    }
    let path_ids = receipt
        .request
        .decisions
        .iter()
        .flat_map(expected)
        .map(|g| g.record_id.clone())
        .chain(allocated.iter().cloned())
        .collect::<BTreeSet<_>>();
    if receipt
        .request
        .decisions
        .iter()
        .flat_map(expected)
        .any(|g| allocated.contains(&g.record_id))
    {
        return Err(bad("new allocation overlaps existing guarded identity"));
    }
    if receipt
        .record_paths
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        != path_ids
    {
        return Err(bad("receipt original path keys not exact"));
    }
    let mut required_writes = BTreeSet::new();
    let mut allowed_writes = BTreeSet::new();
    for (op, a) in receipt.request.decisions.iter().zip(&receipt.allocations) {
        let mut ids = sources(op);
        ids.extend(remaps(op).iter().map(|r| match r {
            EntityRemap::Assertion { assertion_id, .. } => assertion_id.clone(),
            EntityRemap::Mention { extraction_id, .. } => extraction_id.clone(),
        }));
        ids.extend(a.entities.values().cloned());
        for id in ids {
            let path = receipt
                .record_paths
                .get(&id)
                .ok_or_else(|| bad("missing affected record path"))?;
            required_writes.insert(path.clone());
            allowed_writes.insert(path.clone());
        }
        if let EntityDecision::AddAlias { entity_id, .. } = op {
            allowed_writes.insert(
                receipt
                    .record_paths
                    .get(entity_id)
                    .ok_or_else(|| bad("missing alias path"))?
                    .clone(),
            );
        }
    }
    let actual_writes = receipt
        .operations
        .iter()
        .map(|p| p.target.clone())
        .collect::<BTreeSet<_>>();
    if !required_writes.is_subset(&actual_writes) || !actual_writes.is_subset(&allowed_writes) {
        return Err(bad("receipt contains omitted/extra nondecision writes"));
    }
    for proof in &receipt.operations {
        let (id, _) = receipt
            .record_paths
            .iter()
            .find(|(_, path)| *path == &proof.target)
            .ok_or_else(|| bad("operation path has no identity"))?;
        let allocated_entity = receipt
            .allocations
            .iter()
            .any(|a| a.entities.values().any(|entity| entity == id));
        let expected_before = if allocated_entity {
            crate::vault::ExpectedState::Absent
        } else {
            crate::vault::ExpectedState::Hash(
                receipt
                    .request
                    .decisions
                    .iter()
                    .flat_map(expected)
                    .find(|g| &g.record_id == id)
                    .ok_or_else(|| bad("operation lacks explicit hash"))?
                    .hash
                    .clone(),
            )
        };
        if proof.before != expected_before {
            return Err(bad("operation old state differs from explicit hash"));
        }
    }
    let assertions = receipt
        .request
        .decisions
        .iter()
        .flat_map(remaps)
        .filter(|r| matches!(r, EntityRemap::Assertion { .. }))
        .map(remap_key)
        .collect::<BTreeSet<_>>();
    let proofs = receipt
        .assertion_proofs
        .iter()
        .map(|p| format!("a:{}:{}", p.assertion_id, field(&p.field)))
        .collect::<BTreeSet<_>>();
    if assertions != proofs || proofs.len() != receipt.assertion_proofs.len() {
        return Err(bad("assertion remap proof membership differs"));
    }
    let proofs = receipt
        .extraction_proofs
        .iter()
        .flat_map(|p| {
            p.transitions
                .iter()
                .map(move |t| (p.extraction_id.clone(), t.mention_id.clone()))
        })
        .collect::<BTreeSet<_>>();
    if proofs != mentions
        || proofs.len()
            != receipt
                .extraction_proofs
                .iter()
                .map(|p| p.transitions.len())
                .sum::<usize>()
    {
        return Err(bad("extraction transition membership differs"));
    }
    if receipt
        .extraction_proofs
        .iter()
        .map(|p| &p.extraction_id)
        .collect::<BTreeSet<_>>()
        .len()
        != receipt.extraction_proofs.len()
    {
        return Err(bad("duplicate extraction proof"));
    }
    for (op, a) in receipt.request.decisions.iter().zip(&receipt.allocations) {
        for r in remaps(op) {
            match r {
                EntityRemap::Assertion {
                    assertion_id,
                    field: f,
                    old_entity_id,
                    ..
                } => {
                    let p = receipt
                        .assertion_proofs
                        .iter()
                        .find(|p| &p.assertion_id == assertion_id && field(&p.field) == field(f))
                        .ok_or_else(|| bad("missing assertion proof"))?;
                    if &p.before_entity_id != old_entity_id
                        || p.after_entity_id != target_id(r, a)?
                        || p.governing_decision_id != a.decision_id
                        || p.before_entity_id == p.after_entity_id
                    {
                        return Err(bad("assertion proof endpoint/authority differs"));
                    }
                }
                EntityRemap::Mention {
                    extraction_id,
                    mention_id,
                    old_entity_id,
                    ..
                } => {
                    let p = receipt
                        .extraction_proofs
                        .iter()
                        .find(|p| &p.extraction_id == extraction_id)
                        .ok_or_else(|| bad("missing extraction proof"))?;
                    let t = p
                        .transitions
                        .iter()
                        .find(|t| &t.mention_id == mention_id)
                        .ok_or_else(|| bad("missing mention proof"))?;
                    let allocated = a
                        .mention_decisions
                        .iter()
                        .find(|m| &m.extraction_id == extraction_id && &m.mention_id == mention_id)
                        .ok_or_else(|| bad("missing mention allocation"))?;
                    if t.governing_decision_id != a.decision_id
                        || p.prior_bindings.get(mention_id) != Some(&t.before)
                        || t.before
                            != (MentionBinding::Resolved {
                                entity_id: old_entity_id.clone(),
                                decision_id: allocated.predecessor_id.clone(),
                            })
                        || t.after
                            != (MentionBinding::Resolved {
                                entity_id: target_id(r, a)?,
                                decision_id: allocated.decision_id.clone(),
                            })
                    {
                        return Err(bad("mention proof transition/authority differs"));
                    }
                }
            }
        }
    }
    let mut edges = BTreeSet::new();
    for e in &receipt.main_supersessions {
        if !edges.insert((e.predecessor_id.clone(), e.successor_id.clone()))
            || !receipt
                .allocations
                .iter()
                .any(|a| a.decision_id == e.successor_id)
            || !receipt
                .request
                .decisions
                .iter()
                .flat_map(expected)
                .any(|g| g.record_id == e.predecessor_id)
        {
            return Err(bad("main supersession scope/hash differs"));
        }
    }
    Ok(())
}

fn receipts(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
) -> Result<BTreeMap<RecordId, EntityDecisionReceiptV1>> {
    receipts_scoped(notes, &mut ReceiptBudget::default())
}
fn receipts_scoped(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    budget: &mut ReceiptBudget,
) -> Result<BTreeMap<RecordId, EntityDecisionReceiptV1>> {
    let mut result = BTreeMap::new();
    for (path, note) in notes {
        if note.canonical.as_ref().is_some_and(|r| {
            r.kind() == RecordKind::Decision
                && matches!(
                    r.string("wiki_action"),
                    Some("merge" | "split" | "add_alias" | "bind_mention")
                )
        }) && fence_present(note)?
        {
            budget.admit(path, note)?;
            if note.raw.len() > MAX_ENTITY_DECISION_RECEIPT_BYTES + 262144 {
                return Err(super::receipt_budget::exhausted());
            }
            let r = receipt(note)?;
            shape(&r)?;
            if result.get(&r.task_id).is_some_and(|old| old != &r) {
                return Err(bad("contradictory canonical entity receipts"));
            }
            result.insert(r.task_id.clone(), r);
        }
    }
    Ok(result)
}
fn binding_chain(
    receipts: &BTreeMap<RecordId, EntityDecisionReceiptV1>,
    extraction: &RecordId,
    local: &PacketLocalId,
    before: &MentionBinding,
    after: &MentionBinding,
) -> Result<()> {
    let mut state = before.clone();
    let mut seen = BTreeSet::new();
    for _ in 0..=MAX_AUTHORIZED_EVOLUTION_HOPS {
        if &state == after {
            return Ok(());
        }
        let key = packet::canonical_json(&state)?;
        if !seen.insert(key) {
            return Err(bad("binding supersession cycle"));
        }
        let transitions = receipts
            .values()
            .flat_map(|r| r.extraction_proofs.iter())
            .filter(|p| &p.extraction_id == extraction)
            .flat_map(|p| p.transitions.iter())
            .filter(|t| &t.mention_id == local && t.before == state)
            .collect::<Vec<_>>();
        if transitions.len() != 1 {
            return Err(bad("missing/contradictory explicit binding successor"));
        }
        state = transitions[0].after.clone();
    }
    Err(bad("binding successor chain exceeds ceiling"))
}
fn endpoint_chain(
    receipts: &BTreeMap<RecordId, EntityDecisionReceiptV1>,
    id: &RecordId,
    f: &AssertionEntityField,
    before: &RecordId,
    after: &RecordId,
    unchanged: &Blake3Hash,
) -> Result<()> {
    let mut state = before.clone();
    let mut seen = BTreeSet::new();
    for _ in 0..=MAX_AUTHORIZED_EVOLUTION_HOPS {
        if &state == after {
            return Ok(());
        }
        if !seen.insert(state.clone()) {
            return Err(bad("assertion endpoint cycle"));
        }
        let transitions = receipts
            .values()
            .flat_map(|r| &r.assertion_proofs)
            .filter(|p| {
                &p.assertion_id == id
                    && field(&p.field) == field(f)
                    && p.before_entity_id == state
                    && &p.invariant_proposition_hash == unchanged
            })
            .collect::<Vec<_>>();
        if transitions.len() != 1 {
            return Err(bad("missing/contradictory assertion endpoint successor"));
        }
        state = transitions[0].after_entity_id.clone();
    }
    Err(bad("endpoint successor chain exceeds ceiling"))
}

fn envelope_policy(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    r: &EntityDecisionReceiptV1,
    all: &BTreeMap<RecordId, EntityDecisionReceiptV1>,
    budget: &mut ReceiptBudget,
) -> Result<EnvelopePolicy> {
    let mut family = BTreeSet::new();
    let mut edges = BTreeSet::new();
    for (op, a) in r.request.decisions.iter().zip(&r.allocations) {
        let (_, note) = budget.find(notes, &a.decision_id)?;
        let rec = note
            .canonical
            .as_ref()
            .ok_or_else(|| bad("main decision invalid"))?;
        let inputs = match op {
            EntityDecision::AddAlias { entity_id, .. } => vec![entity_id.clone()],
            _ => sources(op).into_iter().collect(),
        };
        if rec.kind() != RecordKind::Decision
            || rec.string("wiki_action") != Some(action(op))
            || id_list(rec, "wiki_input_ids")? != inputs
            || id_list(rec, "wiki_output_ids")? != output_ids(op, a)
            || receipt(note)? != *r
            || !matches!(rec.string("wiki_status"), Some("active" | "superseded"))
        {
            return Err(bad("main canonical decision envelope differs"));
        }
        let prior = r
            .main_supersessions
            .iter()
            .filter(|e| e.successor_id == a.decision_id)
            .collect::<Vec<_>>();
        if rec.string("wiki_supersedes_id")
            != if prior.len() == 1 {
                Some(prior[0].predecessor_id.as_str())
            } else {
                None
            }
        {
            return Err(bad(
                "main singular supersession differs from complete receipt edges",
            ));
        }
        family.insert(a.decision_id.clone());
        if let EntityDecision::SplitEntity { new_entities, .. } = op {
            for new in new_entities {
                let id = a
                    .entities
                    .get(&new.key)
                    .ok_or_else(|| bad("split entity allocation missing"))?;
                if budget
                    .find(notes, id)?
                    .1
                    .canonical
                    .as_ref()
                    .is_none_or(|entity| {
                        entity.kind() != RecordKind::Entity
                            || entity.string("wiki_entity_type") != Some(new.entity_type.as_str())
                    })
                {
                    return Err(bad("split entity immutable type differs"));
                }
            }
        }
        if rec.string("wiki_status") == Some("superseded")
            && !all.values().any(|r| {
                r.main_supersessions
                    .iter()
                    .any(|e| e.predecessor_id == a.decision_id)
            })
        {
            return Err(bad("superseded main lacks explicit successor authority"));
        }
        if rec.string("wiki_status") == Some("active") {
            for source in sources(op) {
                let (_, n) = budget.find(notes, &source)?;
                let entity = n
                    .canonical
                    .as_ref()
                    .ok_or_else(|| bad("superseded entity invalid"))?;
                if entity.kind() != RecordKind::Entity
                    || entity.string("wiki_status") != Some("superseded")
                    || matches!(op,EntityDecision::MergeEntities{target_id,..} if entity.string("wiki_superseded_by_id")!=Some(target_id.as_str()))
                {
                    return Err(bad("main entity outcome differs"));
                }
            }
            for id in output_ids(op, a) {
                let (_, n) = budget.find(notes, &id)?;
                let entity = n
                    .canonical
                    .as_ref()
                    .ok_or_else(|| bad("output entity invalid"))?;
                if entity.kind() != RecordKind::Entity
                    || if matches!(op, EntityDecision::AddAlias { .. }) {
                        !matches!(entity.string("wiki_status"), Some("active" | "superseded"))
                    } else {
                        entity.string("wiki_status") != Some("active")
                    }
                {
                    return Err(bad("main output identity inactive"));
                }
            }
            if let EntityDecision::AddAlias {
                entity_id, alias, ..
            } = op
            {
                let (_, n) = budget.find(notes, entity_id)?;
                if !n
                    .canonical
                    .as_ref()
                    .and_then(|r| r.field("aliases"))
                    .and_then(|v| v.as_array())
                    .is_some_and(|v| v.iter().any(|v| v.as_str() == Some(alias)))
                {
                    return Err(bad("explicit alias not present"));
                }
            }
        }
        for m in &a.mention_decisions {
            let (_, note) = budget.find(notes, &m.decision_id)?;
            let rec = note
                .canonical
                .as_ref()
                .ok_or_else(|| bad("replacement decision invalid"))?;
            let proof = r
                .extraction_proofs
                .iter()
                .find(|p| p.extraction_id == m.extraction_id)
                .and_then(|p| p.transitions.iter().find(|p| p.mention_id == m.mention_id))
                .ok_or_else(|| bad("replacement proof missing"))?;
            let MentionBinding::Resolved { entity_id, .. } = &proof.after else {
                return Err(bad("replacement binding not Resolved"));
            };
            if rec.kind() != RecordKind::Decision
                || rec.string("wiki_action") != Some("bind_mention")
                || rec.string("wiki_extraction_id") != Some(m.extraction_id.as_str())
                || rec.field("wiki_mention_ids") != Some(&json!([m.mention_id]))
                || id_list(rec, "wiki_input_ids")? != vec![m.extraction_id.clone()]
                || id_list(rec, "wiki_output_ids")? != vec![entity_id.clone()]
                || rec.string("wiki_supersedes_id") != Some(m.predecessor_id.as_str())
                || receipt(note)? != *r
            {
                return Err(bad("replacement envelope/supersession differs"));
            }
            let (_, old) = budget.find(notes, &m.predecessor_id)?;
            let old = old
                .canonical
                .as_ref()
                .ok_or_else(|| bad("old mention decision invalid"))?;
            if old.kind() != RecordKind::Decision
                || old.string("wiki_status") != Some("superseded")
                || !matches!(
                    old.string("wiki_action"),
                    Some("bind_mention" | "create_entity")
                )
                || old.string("wiki_extraction_id") != Some(m.extraction_id.as_str())
                || id_list(old, "wiki_input_ids")? != vec![m.extraction_id.clone()]
                || match &proof.before {
                    MentionBinding::Resolved { entity_id, .. } => {
                        id_list(old, "wiki_output_ids")? != vec![entity_id.clone()]
                    }
                    _ => true,
                }
                || !old
                    .field("wiki_mention_ids")
                    .and_then(|v| v.as_array())
                    .is_some_and(|v| v.contains(&json!(m.mention_id)))
            {
                return Err(bad("predecessor authority differs"));
            }
            let prior = r
                .extraction_proofs
                .iter()
                .find(|p| p.extraction_id == m.extraction_id)
                .ok_or_else(|| bad("predecessor extraction proof missing"))?;
            let complete = prior.prior_bindings.iter().filter_map(|(local, b)| {
                matches!(b, MentionBinding::Resolved { decision_id, .. } if decision_id == &m.predecessor_id)
                    .then_some(local.clone())
            }).collect::<BTreeSet<_>>();
            let declared = old
                .field("wiki_mention_ids")
                .and_then(|v| v.as_array())
                .ok_or_else(|| bad("predecessor mention authority missing"))?
                .iter()
                .map(|v| {
                    PacketLocalId::new(
                        v.as_str()
                            .ok_or_else(|| bad("predecessor mention not string"))?,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            if declared.iter().cloned().collect::<BTreeSet<_>>() != complete
                || declared.len() != complete.len()
            {
                return Err(bad("predecessor complete prior mention authority differs"));
            }
            if !matches!(rec.string("wiki_status"), Some("active" | "superseded")) {
                return Err(bad("replacement decision status invalid"));
            }
            let (_, e) = budget.find(notes, &m.extraction_id)?;
            let current = artifact(e)?;
            let current = current
                .bindings
                .get(&m.mention_id)
                .ok_or_else(|| bad("current mention missing"))?;
            binding_chain(all, &m.extraction_id, &m.mention_id, &proof.after, current)?;
            if rec.string("wiki_status") == Some("active") && current != &proof.after {
                return Err(bad("active replacement is not current binding"));
            }
            family.insert(m.decision_id.clone());
            edges.insert((m.predecessor_id.clone(), m.decision_id.clone()));
        }
    }
    for e in &r.main_supersessions {
        let (_, old) = budget.find(notes, &e.predecessor_id)?;
        if old.canonical.as_ref().is_none_or(|rec| {
            rec.kind() != RecordKind::Decision
                || rec.string("wiki_status") != Some("superseded")
                || !matches!(
                    rec.string("wiki_action"),
                    Some("merge" | "split" | "accept")
                )
        }) {
            return Err(bad("main supersession predecessor differs"));
        }
        let predecessor = old
            .canonical
            .as_ref()
            .ok_or_else(|| bad("invalid predecessor"))?;
        if predecessor.string("wiki_action") == Some("accept") {
            let ids = id_list(predecessor, "wiki_input_ids")?
                .into_iter()
                .chain(id_list(predecessor, "wiki_output_ids")?)
                .collect::<BTreeSet<_>>();
            let changed = r
                .assertion_proofs
                .iter()
                .filter(|p| p.governing_decision_id == e.successor_id)
                .map(|p| p.assertion_id.clone())
                .collect::<BTreeSet<_>>();
            if ids.is_empty() || !ids.is_subset(&changed) {
                return Err(bad(
                    "accept predecessor not entirely changed by one main operation",
                ));
            }
            for id in ids {
                if budget
                    .find(notes, &id)?
                    .1
                    .canonical
                    .as_ref()
                    .is_none_or(|r| r.kind() != RecordKind::Assertion)
                {
                    return Err(bad("accept predecessor target missing/nonassertion"));
                }
            }
        }
        edges.insert((e.predecessor_id.clone(), e.successor_id.clone()));
    }
    Ok((family, edges))
}

fn current_scoped_authorities(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    outputs: &BTreeSet<RecordId>,
    budget: &mut ReceiptBudget,
) -> Result<BTreeSet<RecordId>> {
    let mut ids = BTreeSet::new();
    for (path, note) in notes {
        let Some(d) = &note.canonical else { continue };
        if d.kind() != RecordKind::Decision
            || d.string("wiki_status") != Some("active")
            || !matches!(
                d.string("wiki_action"),
                Some("bind_mention" | "create_entity")
            )
        {
            continue;
        }
        let assigned = id_list(d, "wiki_output_ids")?;
        if assigned.len() != 1 || !outputs.contains(&assigned[0]) {
            continue;
        }
        budget.admit(path, note)?;
        let extraction = RecordId::new(
            d.string("wiki_extraction_id")
                .ok_or_else(|| bad("scoped extraction missing"))?,
        )?;
        if id_list(d, "wiki_input_ids")? != vec![extraction.clone()] {
            return Err(bad("scoped decision input authority differs"));
        }
        let locals = d
            .field("wiki_mention_ids")
            .and_then(|v| v.as_array())
            .ok_or_else(|| bad("scoped mentions missing"))?;
        let declared = locals
            .iter()
            .map(|v| {
                PacketLocalId::new(v.as_str().ok_or_else(|| bad("scoped mention not string"))?)
            })
            .collect::<Result<BTreeSet<_>>>()?;
        if declared.is_empty() || declared.len() != locals.len() {
            return Err(bad("scoped mention authority empty/duplicate"));
        }
        let (_, n) = budget.find(notes, &extraction)?;
        let current = artifact(n)?;
        let actual = current
            .bindings
            .iter()
            .filter_map(|(id, b)| match b {
                MentionBinding::Resolved {
                    entity_id,
                    decision_id,
                } if decision_id == d.id() => Some((id.clone(), entity_id)),
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        if actual.keys().cloned().collect::<BTreeSet<_>>() != declared
            || actual.values().any(|id| **id != assigned[0])
        {
            return Err(bad(
                "scoped decision complete current binding authority differs",
            ));
        }
        ids.insert(d.id().clone());
    }
    Ok(ids)
}

fn verify_current_binding(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    artifact: &ExtractionArtifactV1,
    local: &PacketLocalId,
    binding: &MentionBinding,
    budget: &mut ReceiptBudget,
) -> Result<()> {
    let (decision_id, output, allowed) = match binding {
        MentionBinding::Pending => return Ok(()),
        MentionBinding::Resolved {
            entity_id,
            decision_id,
        } => (
            decision_id,
            vec![entity_id.clone()],
            ["bind_mention", "create_entity"].as_slice(),
        ),
        MentionBinding::Rejected { decision_id } => {
            (decision_id, vec![], ["reject_mention"].as_slice())
        }
    };
    let (_, note) = budget.find(notes, decision_id)?;
    let d = note
        .canonical
        .as_ref()
        .ok_or_else(|| bad("current binding decision invalid"))?;
    if d.kind() != RecordKind::Decision
        || d.string("wiki_status") != Some("active")
        || !d
            .string("wiki_action")
            .is_some_and(|a| allowed.contains(&a))
        || d.string("wiki_extraction_id") != Some(artifact.extraction_id.as_str())
        || id_list(d, "wiki_input_ids")? != vec![artifact.extraction_id.clone()]
        || id_list(d, "wiki_output_ids")? != output
    {
        return Err(bad(
            "later Pending resolution lacks exact canonical authority",
        ));
    }
    let declared = d
        .field("wiki_mention_ids")
        .and_then(|v| v.as_array())
        .ok_or_else(|| bad("current binding mention authority missing"))?
        .iter()
        .map(|v| {
            PacketLocalId::new(
                v.as_str()
                    .ok_or_else(|| bad("current binding mention not string"))?,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let actual = artifact
        .bindings
        .iter()
        .filter_map(|(id, b)| match b {
            MentionBinding::Resolved {
                decision_id: other, ..
            }
            | MentionBinding::Rejected { decision_id: other }
                if other == decision_id =>
            {
                Some(id.clone())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    if declared.len() != actual.len()
        || declared.iter().cloned().collect::<BTreeSet<_>>() != actual
        || !actual.contains(local)
    {
        return Err(bad("later resolution complete mention authority differs"));
    }
    Ok(())
}

pub fn verify_decision_policy(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
) -> Result<Option<VerifiedDecisionPolicy>> {
    verify_decision_policy_scoped(notes, &mut ReceiptBudget::default())
}
pub(crate) fn verify_decision_policy_scoped(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    budget: &mut ReceiptBudget,
) -> Result<Option<VerifiedDecisionPolicy>> {
    let all = receipts_scoped(notes, budget)?;
    if all.is_empty() {
        return Ok(None);
    }
    let mut families = vec![];
    let mut aliases = BTreeSet::new();
    let mut edges = BTreeSet::new();
    for r in all.values() {
        let (mut family, new_edges) = envelope_policy(notes, r, &all, budget)?;
        for (op, a) in r.request.decisions.iter().zip(&r.allocations) {
            if matches!(op, EntityDecision::AddAlias { .. }) {
                aliases.insert(a.decision_id.clone());
                continue;
            }
            // Merges are combined once per retained target below. Only an active
            // split needs its own output family; historical authorities do not
            // grant compatibility to unrelated current decisions.
            if !matches!(op, EntityDecision::SplitEntity { .. })
                || budget
                    .find(notes, &a.decision_id)?
                    .1
                    .canonical
                    .as_ref()
                    .is_none_or(|d| d.string("wiki_status") != Some("active"))
            {
                continue;
            }
            let outputs = output_ids(op, a).into_iter().collect::<BTreeSet<_>>();
            family.extend(current_scoped_authorities(notes, &outputs, budget)?);
            let scope = outputs
                .union(&sources(op))
                .cloned()
                .collect::<BTreeSet<_>>();
            for prior in all.values() {
                for (alias_op, alias_alloc) in
                    prior.request.decisions.iter().zip(&prior.allocations)
                {
                    if let EntityDecision::AddAlias { entity_id, .. } = alias_op
                        && scope.contains(entity_id)
                    {
                        family.insert(alias_alloc.decision_id.clone());
                    }
                }
            }
        }
        families.push(family);
        edges.extend(new_edges);
        for p in &r.extraction_proofs {
            let (_, n) = budget.find(notes, &p.extraction_id)?;
            let current = artifact(n)?;
            if mention_state::immutable_hash(&current)? != p.immutable_extraction_hash
                || current.bindings.keys().ne(p.prior_bindings.keys())
                || p.prior_materialized_assertions
                    .windows(2)
                    .any(|w| w[0] >= w[1])
                || !p
                    .prior_materialized_assertions
                    .iter()
                    .all(|id| current.materialized_assertions.contains(id))
            {
                return Err(bad("remap immutable artifact/complete prior map differs"));
            }
            for (local, before) in &p.prior_bindings {
                if p.transitions.iter().any(|t| &t.mention_id == local) {
                    continue;
                }
                let now = current
                    .bindings
                    .get(local)
                    .ok_or_else(|| bad("untouched current mention absent"))?;
                match before {
                    MentionBinding::Resolved { .. } => {
                        binding_chain(&all, &p.extraction_id, local, before, now)?
                    }
                    MentionBinding::Rejected { .. } if before != now => {
                        return Err(bad("historical rejected binding changed"));
                    }
                    MentionBinding::Pending => {
                        verify_current_binding(notes, &current, local, now, budget)?
                    }
                    _ => {}
                }
            }
            for t in &p.transitions {
                binding_chain(
                    &all,
                    &p.extraction_id,
                    &t.mention_id,
                    &t.after,
                    current
                        .bindings
                        .get(&t.mention_id)
                        .ok_or_else(|| bad("missing current binding"))?,
                )?;
            }
            // Every predecessor's complete declared mention authority is carried.
            for t in &p.transitions {
                let MentionBinding::Resolved { decision_id, .. } = &t.before else {
                    return Err(bad("nonresolved remap prior binding"));
                };
                for (local, b) in &p.prior_bindings {
                    if matches!(b,MentionBinding::Resolved{decision_id:id,..} if id==decision_id)
                        && !p.transitions.iter().any(|t| &t.mention_id == local)
                    {
                        return Err(bad("partial shared predecessor supersession"));
                    }
                }
            }
        }
        for p in &r.assertion_proofs {
            let (_, n) = budget.find(notes, &p.assertion_id)?;
            let rec = n
                .canonical
                .as_ref()
                .ok_or_else(|| bad("current assertion invalid"))?;
            if rec.kind() != RecordKind::Assertion
                || invariant(rec)? != p.invariant_proposition_hash
            {
                return Err(bad("assertion invariant proposition differs"));
            }
            let current = RecordId::new(
                rec.string(field(&p.field))
                    .ok_or_else(|| bad("current assertion endpoint absent"))?,
            )?;
            endpoint_chain(
                &all,
                &p.assertion_id,
                &p.field,
                &p.after_entity_id,
                &current,
                &p.invariant_proposition_hash,
            )?;
        }
        for op in &r.request.decisions {
            let original_sources = sources(op);
            let refs = reference_set_scoped(notes, &original_sources, budget)?;
            if !refs.is_empty() {
                return Err(bad("unremapped canonical reference to superseded identity"));
            }
        }
    }
    fn visit(
        id: &RecordId,
        edges: &BTreeSet<(RecordId, RecordId)>,
        colors: &mut BTreeMap<RecordId, u8>,
        lengths: &mut BTreeMap<RecordId, usize>,
        depth: usize,
    ) -> Result<usize> {
        if depth > MAX_AUTHORIZED_EVOLUTION_HOPS || colors.get(id) == Some(&1) {
            return Err(bad("decision supersession cycle/chain ceiling"));
        }
        if colors.get(id) == Some(&2) {
            let suffix = *lengths
                .get(id)
                .ok_or_else(|| bad("missing proven chain length"))?;
            if depth + suffix > MAX_AUTHORIZED_EVOLUTION_HOPS {
                return Err(bad("decision supersession cycle/chain ceiling"));
            }
            return Ok(suffix);
        }
        colors.insert(id.clone(), 1);
        let mut longest = 0;
        for (_, next) in edges.iter().filter(|(old, _)| old == id) {
            longest = longest.max(1 + visit(next, edges, colors, lengths, depth + 1)?);
        }
        if longest > MAX_AUTHORIZED_EVOLUTION_HOPS {
            return Err(bad("decision supersession cycle/chain ceiling"));
        }
        colors.insert(id.clone(), 2);
        lengths.insert(id.clone(), longest);
        Ok(longest)
    }
    let mut colors = BTreeMap::new();
    let mut lengths = BTreeMap::new();
    for (id, _) in &edges {
        visit(id, &edges, &mut colors, &mut lengths, 0)?;
    }
    // Distinct explicit merges retaining one target have consistent current outcomes.
    let mut merges: BTreeMap<RecordId, (BTreeSet<RecordId>, BTreeSet<RecordId>)> = BTreeMap::new();
    for r in all.values() {
        for (op, a) in r.request.decisions.iter().zip(&r.allocations) {
            if let EntityDecision::MergeEntities { target_id, .. } = op
                && budget
                    .find(notes, &a.decision_id)?
                    .1
                    .canonical
                    .as_ref()
                    .is_some_and(|r| r.string("wiki_status") == Some("active"))
            {
                let (ids, absorbed) = merges.entry(target_id.clone()).or_default();
                ids.insert(a.decision_id.clone());
                for source in sources(op) {
                    if !absorbed.insert(source) {
                        return Err(bad("active merges have overlapping absorbed scopes"));
                    }
                }
            }
        }
    }
    for (target, (mut ids, absorbed)) in merges {
        for (path, note) in notes {
            let Some(d) = &note.canonical else {
                continue;
            };
            if d.kind() != RecordKind::Decision || d.string("wiki_status") != Some("active") {
                continue;
            }
            if aliases.contains(d.id())
                && id_list(d, "wiki_output_ids")?
                    .iter()
                    .any(|id| id == &target || absorbed.contains(id))
            {
                budget.admit(path, note)?;
                ids.insert(d.id().clone());
            }
        }
        ids.extend(current_scoped_authorities(
            notes,
            &BTreeSet::from([target]),
            budget,
        )?);
        if ids.len() > 1 {
            families.push(ids);
        }
    }
    Ok(Some(VerifiedDecisionPolicy {
        edges,
        families,
        receipts: all,
        aliases,
    }))
}

pub fn verify_binding_evolution(
    view: &SourceView<'_>,
    extraction_id: &RecordId,
    mention_id: &PacketLocalId,
    before: &MentionBinding,
    after: &MentionBinding,
) -> Result<()> {
    if before == after {
        return Ok(());
    }
    let policy = verify_decision_policy(&view.notes)?
        .ok_or_else(|| bad("binding remap has no explicit canonical receipt"))?;
    binding_chain(&policy.receipts, extraction_id, mention_id, before, after)
}
pub fn verify_proposition_evolution(
    view: &SourceView<'_>,
    assertion_id: &RecordId,
    before: &CanonicalRecord,
    after: &CanonicalRecord,
) -> Result<()> {
    if before.id() != assertion_id
        || after.id() != assertion_id
        || before.kind() != RecordKind::Assertion
        || after.kind() != RecordKind::Assertion
        || invariant(before)? != invariant(after)?
    {
        return Err(bad("historical proposition invariant differs"));
    }
    let policy = verify_decision_policy(&view.notes)?;
    for f in [
        AssertionEntityField::SubjectId,
        AssertionEntityField::ObjectId,
    ] {
        match (before.string(field(&f)), after.string(field(&f))) {
            (Some(old), Some(new)) if old != new => endpoint_chain(
                &policy
                    .as_ref()
                    .ok_or_else(|| bad("no explicit proposition successor receipt"))?
                    .receipts,
                assertion_id,
                &f,
                &RecordId::new(old)?,
                &RecordId::new(new)?,
                &invariant(before)?,
            )?,
            (a, b) if a == b => {}
            _ => return Err(bad("entity/literal shape changed")),
        }
    }
    Ok(())
}

fn verify_exact_decision_writes(
    logical: &SourceView<'_>,
    proposed: &SourceView<'_>,
    input: &ValidationInput,
    witness: Option<&RetainedGraphInput>,
    r: &EntityDecisionReceiptV1,
) -> Result<()> {
    let new_ids = r
        .allocations
        .iter()
        .flat_map(|a| {
            std::iter::once(a.decision_id.clone())
                .chain(a.mention_decisions.iter().map(|m| m.decision_id.clone()))
        })
        .collect::<BTreeSet<_>>();
    let old_ids = r
        .main_supersessions
        .iter()
        .map(|e| e.predecessor_id.clone())
        .chain(
            r.allocations
                .iter()
                .flat_map(|a| a.mention_decisions.iter().map(|m| m.predecessor_id.clone())),
        )
        .collect::<BTreeSet<_>>();
    let expected_targets = r
        .operations
        .iter()
        .map(|op| op.target.clone())
        .chain(
            new_ids
                .union(&old_ids)
                .map(|id| {
                    r.record_paths
                        .get(id)
                        .cloned()
                        .ok_or_else(|| bad("decision original path missing"))
                })
                .collect::<Result<Vec<_>>>()?,
        )
        .collect::<BTreeSet<_>>();
    let actual_targets = if let Some(w) = witness {
        w.operations()
            .iter()
            .map(|op| op.path().clone())
            .collect::<Vec<_>>()
    } else {
        input
            .overlay
            .iter()
            .map(|op| op.path.clone())
            .collect::<Vec<_>>()
    };
    if actual_targets.len() != expected_targets.len()
        || actual_targets.into_iter().collect::<BTreeSet<_>>() != expected_targets
    {
        return Err(bad("GraphDecide exact write membership differs"));
    }
    for id in new_ids {
        let path = r
            .record_paths
            .get(&id)
            .ok_or_else(|| bad("new decision path missing"))?;
        let (after_path, after) = find(&proposed.notes, &id)?;
        if after_path != path
            || logical
                .notes
                .values()
                .any(|n| n.canonical.as_ref().is_some_and(|d| d.id() == &id))
            || logical.notes.contains_key(path)
        {
            return Err(bad("new Decision is not absent at exact original path"));
        }
        let d = after
            .canonical
            .as_ref()
            .ok_or_else(|| bad("new decision invalid"))?;
        let (op, a) = r
            .request
            .decisions
            .iter()
            .zip(&r.allocations)
            .find(|(_, a)| {
                a.decision_id == id || a.mention_decisions.iter().any(|m| m.decision_id == id)
            })
            .ok_or_else(|| bad("new decision not allocated"))?;
        let mention = a.mention_decisions.iter().find(|m| m.decision_id == id);
        let stamp = d
            .string("wiki_created_at")
            .ok_or_else(|| bad("new decision time absent"))?;
        if super::decisions::decision_bytes_at(op, a, r, mention, stamp)? != after.raw {
            return Err(bad("new Decision differs from exact allocated creation"));
        }
    }
    for id in old_ids {
        let path = r
            .record_paths
            .get(&id)
            .ok_or_else(|| bad("old decision path missing"))?;
        let (before_path, before) = find(&logical.notes, &id)?;
        let (after_path, after) = find(&proposed.notes, &id)?;
        if before_path != path
            || after_path != path
            || before.canonical.as_ref().is_none_or(|d| {
                d.kind() != RecordKind::Decision || d.string("wiki_status") != Some("active")
            })
            || crate::records::edit_note(
                before,
                &BTreeMap::from([("wiki_status".into(), json!("superseded"))]),
                None,
                &before.source_hash,
            )? != after.raw
        {
            return Err(bad(
                "Decision predecessor differs from exact status-only write",
            ));
        }
    }
    Ok(())
}

pub fn verify_remap_overlay(
    fs: &VaultFs,
    input: &ValidationInput,
    witness: Option<&RetainedGraphInput>,
) -> Result<Option<VerifiedRemapOverlay>> {
    let current = SourceView::from_closed_input(
        fs,
        &ValidationInput {
            vault_id: input.vault_id.clone(),
            documents: input.documents.clone(),
            overlay: vec![],
        },
    )?;
    let proposed = SourceView::from_closed_input(fs, input)?;
    // An unchanged corrupt historical receipt remains a baseline diagnostic.
    let fresh_authority = input
        .overlay
        .iter()
        .filter_map(|t| t.bytes.as_ref())
        .map(|b| crate::records::parse_note(b))
        .any(|n| {
            n.canonical
                .as_ref()
                .is_some_and(|r| r.kind() == RecordKind::Decision)
                && has_fence(&n).unwrap_or(true)
        });
    let policy = match verify_decision_policy(&proposed.notes) {
        Ok(Some(p)) => p,
        Ok(None) if witness.is_some() => {
            return Err(bad(
                "retained GraphDecide origin/allocation must match exactly one receipt",
            ));
        }
        Ok(None) => return Ok(None),
        Err(e) if e.code == ErrorCode::BudgetExceeded => return Err(e),
        Err(_) if !fresh_authority && witness.is_none() => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut original = input
        .documents
        .iter()
        .map(|d| (d.path.clone(), d.clone()))
        .collect::<BTreeMap<_, _>>();
    if let Some(w) = witness {
        for op in w.operations() {
            match op.before_bytes() {
                Some(bytes) => {
                    original.insert(
                        op.path().clone(),
                        crate::changes::ScanDocument {
                            path: op.path().clone(),
                            bytes: bytes.to_vec(),
                            hash: Blake3Hash::digest(bytes),
                        },
                    );
                }
                None => {
                    original.remove(op.path());
                }
            }
        }
    }
    let logical = SourceView::from_closed_input(
        fs,
        &ValidationInput {
            vault_id: input.vault_id.clone(),
            documents: original.into_values().collect(),
            overlay: vec![],
        },
    )?;
    let mut authorized = BTreeSet::new();
    let mut requires_retained = false;
    if let Some(w) = witness {
        let matched = policy
            .receipts
            .values()
            .filter(|r| {
                w.origin().operation == OriginOperation::GraphDecide
                    && w.origin().packet_id == r.task_id
                    && w.origin().response_hash == r.request_hash
                    && w.allocated_ids() == &super::decisions::allocation_map(&r.allocations)
            })
            .count();
        if matched != 1 {
            return Err(bad(
                "retained GraphDecide origin/allocation must match exactly one receipt",
            ));
        }
    }
    for r in policy.receipts.values() {
        let relevant = if let Some(w) = witness {
            w.origin().operation == OriginOperation::GraphDecide
                && w.origin().packet_id == r.task_id
                && w.origin().response_hash == r.request_hash
        } else {
            r.allocations.iter().any(|a| {
                find(&current.notes, &a.decision_id).is_err()
                    && find(&proposed.notes, &a.decision_id).is_ok()
            })
        };
        if !relevant {
            continue;
        }
        requires_retained = true;
        verify_exact_decision_writes(&logical, &proposed, input, witness, r)?;
        verify_exact_changes(&logical, &proposed, &current, r)?;
        for guard in r.request.decisions.iter().flat_map(expected) {
            if find(&logical.notes, &guard.record_id)?.1.source_hash != guard.hash {
                return Err(conflict("logical original authorization hash differs"));
            }
        }
        if let Some(w) = witness {
            if w.allocated_ids() != &super::decisions::allocation_map(&r.allocations) {
                return Err(bad("retained allocation maps differ"));
            }
            let actual = w
                .operations()
                .iter()
                .filter(|op| {
                    op.after_bytes().is_none_or(|b| {
                        crate::records::parse_note(b)
                            .canonical
                            .as_ref()
                            .is_none_or(|r| r.kind() != RecordKind::Decision)
                    })
                })
                .map(|op| EntityDecisionWriteProof {
                    target: op.path().clone(),
                    before: op.before().clone(),
                    after: op.after().clone(),
                })
                .collect::<Vec<_>>();
            if actual != r.operations {
                return Err(bad("retained exact nondecision operations differ"));
            }
        }
        for proof in &r.operations {
            let observed = current
                .notes
                .get(&proof.target)
                .map_or(crate::vault::ExpectedState::Absent, |n| {
                    crate::vault::ExpectedState::Hash(n.source_hash.clone())
                });
            let actual = proposed
                .notes
                .get(&proof.target)
                .map_or(crate::vault::ExpectedState::Absent, |n| {
                    crate::vault::ExpectedState::Hash(n.source_hash.clone())
                });
            let before = logical
                .notes
                .get(&proof.target)
                .map_or(crate::vault::ExpectedState::Absent, |n| {
                    crate::vault::ExpectedState::Hash(n.source_hash.clone())
                });
            if (observed != proof.before && observed != proof.after)
                || actual != proof.after
                || before != proof.before
            {
                return Err(conflict(
                    "remap target differs from exact guarded prior/after state",
                ));
            }
        }
        for p in &r.assertion_proofs {
            let (_, after) = find(&proposed.notes, &p.assertion_id)?;
            let after = after
                .canonical
                .as_ref()
                .ok_or_else(|| bad("after assertion invalid"))?;
            let (_, before) = find(&logical.notes, &p.assertion_id)?;
            let before = before
                .canonical
                .as_ref()
                .ok_or_else(|| bad("prior assertion invalid"))?;
            if invariant(before)? != p.invariant_proposition_hash
                || invariant(after)? != p.invariant_proposition_hash
                || before.string(field(&p.field)) != Some(p.before_entity_id.as_str())
                || after.string(field(&p.field)) != Some(p.after_entity_id.as_str())
            {
                return Err(bad("overlay assertion transition not exact"));
            }
            for f in [
                AssertionEntityField::SubjectId,
                AssertionEntityField::ObjectId,
            ] {
                if before.string(field(&f)) != after.string(field(&f))
                    && !r.assertion_proofs.iter().any(|proof| {
                        proof.assertion_id == p.assertion_id
                            && field(&proof.field) == field(&f)
                            && before.string(field(&f)) == Some(proof.before_entity_id.as_str())
                            && after.string(field(&f)) == Some(proof.after_entity_id.as_str())
                    })
                {
                    return Err(bad("unlisted assertion endpoint change"));
                }
            }
            let changed = r
                .assertion_proofs
                .iter()
                .filter(|proof| proof.assertion_id == p.assertion_id)
                .map(|proof| companion(&proof.field))
                .collect::<BTreeSet<_>>();
            for key in before
                .fields()
                .keys()
                .chain(after.fields().keys())
                .collect::<BTreeSet<_>>()
            {
                if !matches!(
                    key.as_str(),
                    "wiki_subject_id" | "wiki_object_id" | "wiki_status"
                ) && !changed.contains(key.as_str())
                    && before.field(key) != after.field(key)
                {
                    return Err(bad("unlisted assertion field/evidence/qualifier mutation"));
                }
            }
            let expected_status = if before.string("wiki_status") == Some("accepted") {
                Some("proposed")
            } else {
                before.string("wiki_status")
            };
            if after.string("wiki_status") != expected_status {
                return Err(bad("remap assertion status transition differs"));
            }
            for proof in r
                .assertion_proofs
                .iter()
                .filter(|proof| proof.assertion_id == p.assertion_id)
            {
                let path = r
                    .record_paths
                    .get(&proof.after_entity_id)
                    .ok_or_else(|| bad("missing original endpoint path"))?;
                if after.string(companion(&proof.field)) != Some(format!("[[{path}]]").as_str()) {
                    return Err(bad("remap companion target differs"));
                }
            }
            authorized.insert(p.assertion_id.clone());
        }
        for op in &r.request.decisions {
            let old_refs = reference_set(&current.notes, &sources(op))?;
            let requested = remaps(op)
                .iter()
                .filter(|m| sources(op).contains(old(m)))
                .map(remap_key)
                .collect::<BTreeSet<_>>();
            if old_refs.keys().any(|key| !requested.contains(key)) {
                return Err(conflict("new referring record makes remap incomplete"));
            }
            let before_refs = reference_set(&logical.notes, &sources(op))?;
            if before_refs.keys().cloned().collect::<BTreeSet<_>>() != requested {
                return Err(conflict("logical original remap is not exhaustive"));
            }
        }
    }
    Ok(Some(VerifiedRemapOverlay {
        assertions: authorized,
        policy,
        requires_retained,
    }))
}

/// Canonical acknowledgement; no manifest or historical whole-note bytes invented.
pub fn load_entity_decision_receipt(
    view: &SourceView<'_>,
    task_id: &RecordId,
) -> Result<Option<VerifiedEntityDecisionReceipt>> {
    let all = receipts(&view.notes)?;
    let Some(r) = all.get(task_id) else {
        return Ok(None);
    };
    verify_decision_policy(&view.notes)?.ok_or_else(|| bad("canonical decision policy missing"))?;
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
    let mut decision_locators = vec![];
    for (op, a) in r.request.decisions.iter().zip(&r.allocations) {
        for id in std::iter::once(&a.decision_id)
            .chain(a.mention_decisions.iter().map(|m| &m.decision_id))
        {
            let (p, n) = find(&view.notes, id)?;
            decision_locators.push(packet::locator(view, p, n)?);
        }
        if let EntityDecision::SplitEntity { new_entities, .. } = op {
            for e in new_entities {
                let id = a
                    .entities
                    .get(&e.key)
                    .ok_or_else(|| bad("split allocation missing"))?;
                let (_, n) = find(&view.notes, id)?;
                if n.canonical.as_ref().is_none_or(|r| {
                    r.kind() != RecordKind::Entity
                        || r.string("wiki_entity_type") != Some(e.entity_type.as_str())
                }) {
                    return Err(bad("split created identity type differs"));
                }
                let original_path = r
                    .record_paths
                    .get(id)
                    .ok_or_else(|| bad("split original path missing"))?;
                let expected_hash = Blake3Hash::digest(super::decisions::entity_bytes(
                    id,
                    &e.title,
                    &e.entity_type,
                )?);
                if !r.operations.iter().any(|p| {
                    &p.target == original_path
                        && p.before == crate::vault::ExpectedState::Absent
                        && p.after == crate::vault::ExpectedState::Hash(expected_hash.clone())
                }) {
                    return Err(bad("split original creation proof differs"));
                }
            }
        }
    }
    // Full bounded source/raw/window/quote proof belongs to this public loader;
    // the pure policy constructor never recurses into the extraction loader.
    for p in &r.extraction_proofs {
        let verified = import::load_extraction(view, &p.extraction_id)?;
        for dep in verified.dependencies {
            if deps
                .insert(dep.path, dep.expected.clone())
                .is_some_and(|old| old != dep.expected)
            {
                return Err(conflict("restoration source dependency changed"));
            }
        }
    }
    Ok(Some(VerifiedEntityDecisionReceipt {
        receipt: r.clone(),
        dependencies: packet::dependencies(deps),
        decision_locators,
    }))
}

/// This historical proof is reachable only through an authenticated committed
/// inverse lineage. Unmodified historical bytes are unavailable, not invented.
pub(crate) fn verify_committed_anchor(
    fs: &VaultFs,
    input: &ValidationInput,
    seal: &RetainedGraphInverseInput,
) -> Result<BTreeSet<RecordId>> {
    let anchor = seal.anchor();
    if anchor.origin().operation != OriginOperation::GraphDecide {
        return Err(bad("inverse anchor is not GraphDecide"));
    }
    let mut before = input
        .documents
        .iter()
        .map(|d| (d.path.clone(), d.bytes.as_slice()))
        .collect::<BTreeMap<_, _>>();
    let mut candidate = None;
    for op in anchor.operations() {
        match op.before_bytes() {
            Some(bytes) => {
                before.insert(op.path().clone(), bytes);
            }
            None => {
                before.remove(op.path());
            }
        }
        if op.before() == &crate::vault::ExpectedState::Absent
            && let Some(bytes) = op.after_bytes()
        {
            let note = crate::records::parse_note(bytes);
            if note
                .canonical
                .as_ref()
                .is_some_and(|d| d.kind() == RecordKind::Decision)
            {
                let r = receipt(&note)?;
                shape(&r)?;
                if candidate.as_ref().is_some_and(|old| old != &r) {
                    return Err(bad("inverse anchor receipt copies differ"));
                }
                candidate = Some(r);
            }
        }
    }
    let r = candidate.ok_or_else(|| bad("inverse anchor original receipt absent"))?;
    if r.task_id != anchor.origin().packet_id
        || r.request_hash != anchor.origin().response_hash
        || anchor.allocated_ids() != &super::decisions::allocation_map(&r.allocations)
    {
        return Err(bad("inverse anchor origin/allocation proof differs"));
    }
    let mut after_bytes = before.clone();
    for op in anchor.operations() {
        match op.after_bytes() {
            Some(bytes) => {
                after_bytes.insert(op.path().clone(), bytes);
            }
            None => {
                after_bytes.remove(op.path());
            }
        }
    }
    for bytes in [&before, &after_bytes] {
        if bytes.len() > 4096
            || bytes
                .values()
                .try_fold(0usize, |n, b| n.checked_add(b.len()))
                .is_none_or(|n| n > packet::SOURCE_CAP)
        {
            return Err(bad("inverse historical captured view exceeds ceiling"));
        }
    }
    let historical = ValidationInput {
        vault_id: input.vault_id.clone(),
        documents: before
            .into_iter()
            .map(|(path, bytes)| crate::changes::ScanDocument {
                path,
                hash: Blake3Hash::digest(bytes),
                bytes: bytes.to_vec(),
            })
            .collect(),
        overlay: anchor
            .operations()
            .iter()
            .map(|op| crate::changes::ProposedTarget {
                path: op.path().clone(),
                bytes: op.after_bytes().map(Vec::from),
            })
            .collect(),
    };
    let logical = SourceView::from_closed_input(
        fs,
        &ValidationInput {
            vault_id: historical.vault_id.clone(),
            documents: historical.documents.clone(),
            overlay: vec![],
        },
    )?;
    let after = SourceView::from_closed_input(fs, &historical)?;
    verify_exact_decision_writes(&logical, &after, &historical, Some(anchor), &r)?;
    verify_exact_changes_with_anchor(&logical, &after, &logical, &r, Some(seal))?;
    for guard in r.request.decisions.iter().flat_map(expected) {
        let path = r
            .record_paths
            .get(&guard.record_id)
            .ok_or_else(|| bad("anchor guard path missing"))?;
        if let Some(op) = anchor.operations().iter().find(|op| op.path() == path)
            && op.before() != &crate::vault::ExpectedState::Hash(guard.hash.clone())
        {
            return Err(bad(
                "anchor written expected record differs from retained before hash",
            ));
        }
    }
    let operations = anchor
        .operations()
        .iter()
        .filter(|op| {
            op.after_bytes().is_none_or(|bytes| {
                crate::records::parse_note(bytes)
                    .canonical
                    .as_ref()
                    .is_none_or(|d| d.kind() != RecordKind::Decision)
            })
        })
        .map(|op| EntityDecisionWriteProof {
            target: op.path().clone(),
            before: op.before().clone(),
            after: op.after().clone(),
        })
        .collect::<Vec<_>>();
    if operations != r.operations {
        return Err(bad("anchor exact nondecision writes differ"));
    }
    for proof in &r.assertion_proofs {
        let (_, before) = find(&logical.notes, &proof.assertion_id)?;
        let (_, after) = find(&after.notes, &proof.assertion_id)?;
        let before = before
            .canonical
            .as_ref()
            .ok_or_else(|| bad("anchor prior assertion invalid"))?;
        let after = after
            .canonical
            .as_ref()
            .ok_or_else(|| bad("anchor after assertion invalid"))?;
        if before.kind() != RecordKind::Assertion
            || after.kind() != RecordKind::Assertion
            || invariant(before)? != proof.invariant_proposition_hash
            || invariant(after)? != proof.invariant_proposition_hash
            || before.string(field(&proof.field)) != Some(proof.before_entity_id.as_str())
            || after.string(field(&proof.field)) != Some(proof.after_entity_id.as_str())
        {
            return Err(bad("anchor assertion invariant/endpoint proof differs"));
        }
    }
    for proof in &r.extraction_proofs {
        for transition in &proof.transitions {
            let MentionBinding::Resolved {
                entity_id,
                decision_id,
            } = &transition.before
            else {
                return Err(bad("anchor prior mention not Resolved"));
            };
            let (_, old) = find(&logical.notes, decision_id)?;
            let d = old
                .canonical
                .as_ref()
                .ok_or_else(|| bad("anchor predecessor invalid"))?;
            let complete = proof.prior_bindings.iter().filter_map(|(id, b)|
                matches!(b, MentionBinding::Resolved { decision_id: other, .. } if other == decision_id).then_some(id.clone()))
                .collect::<BTreeSet<_>>();
            let declared = d
                .field("wiki_mention_ids")
                .and_then(|v| v.as_array())
                .ok_or_else(|| bad("anchor predecessor mention list absent"))?
                .iter()
                .map(|v| {
                    PacketLocalId::new(v.as_str().ok_or_else(|| bad("anchor mention not string"))?)
                })
                .collect::<Result<Vec<_>>>()?;
            if d.kind() != RecordKind::Decision
                || d.string("wiki_status") != Some("active")
                || !matches!(
                    d.string("wiki_action"),
                    Some("bind_mention" | "create_entity")
                )
                || d.string("wiki_extraction_id") != Some(proof.extraction_id.as_str())
                || id_list(d, "wiki_input_ids")? != vec![proof.extraction_id.clone()]
                || id_list(d, "wiki_output_ids")? != vec![entity_id.clone()]
                || declared.len() != complete.len()
                || declared.into_iter().collect::<BTreeSet<_>>() != complete
                || complete
                    .iter()
                    .any(|id| !proof.transitions.iter().any(|t| &t.mention_id == id))
            {
                return Err(bad("anchor complete mention predecessor authority differs"));
            }
        }
    }
    Ok(r.assertion_proofs
        .iter()
        .map(|p| p.assertion_id.clone())
        .collect())
}

fn verify_declared_anchor_predecessor(
    logical: &SourceView<'_>,
    op: &EntityDecision,
    id: &RecordId,
) -> Result<()> {
    let (_, note) = find(&logical.notes, id)?;
    let d = note
        .canonical
        .as_ref()
        .ok_or_else(|| bad("anchor main predecessor invalid"))?;
    if d.kind() != RecordKind::Decision || d.string("wiki_status") != Some("active") {
        return Err(bad("anchor main predecessor not active Decision"));
    }
    match d.string("wiki_action") {
        Some("merge" | "split")
            if id_list(d, "wiki_output_ids")?
                .iter()
                .any(|id| sources(op).contains(id)) =>
        {
            Ok(())
        }
        Some("accept") => {
            let ids = id_list(d, "wiki_input_ids")?
                .into_iter()
                .chain(id_list(d, "wiki_output_ids")?)
                .collect::<BTreeSet<_>>();
            let changed = remaps(op)
                .iter()
                .filter_map(|r| match r {
                    EntityRemap::Assertion { assertion_id, .. } => Some(assertion_id.clone()),
                    _ => None,
                })
                .collect::<BTreeSet<_>>();
            if ids.is_empty() || !ids.is_subset(&changed) {
                return Err(bad("anchor acceptance scope not entirely changed"));
            }
            for id in ids {
                if find(&logical.notes, &id)?
                    .1
                    .canonical
                    .as_ref()
                    .is_none_or(|a| {
                        a.kind() != RecordKind::Assertion
                            || a.string("wiki_status") != Some("accepted")
                    })
                {
                    return Err(bad("anchor acceptance target not accepted assertion"));
                }
            }
            Ok(())
        }
        _ => Err(bad("anchor main predecessor action/scope differs")),
    }
}

fn verify_exact_changes(
    logical: &SourceView<'_>,
    proposed: &SourceView<'_>,
    current: &SourceView<'_>,
    r: &EntityDecisionReceiptV1,
) -> Result<()> {
    verify_exact_changes_with_anchor(logical, proposed, current, r, None)
}

fn verify_exact_changes_with_anchor(
    logical: &SourceView<'_>,
    proposed: &SourceView<'_>,
    current: &SourceView<'_>,
    r: &EntityDecisionReceiptV1,
    anchor: Option<&RetainedGraphInverseInput>,
) -> Result<()> {
    let mut edits: BTreeMap<RecordId, BTreeMap<String, serde_json::Value>> = BTreeMap::new();
    let put = |edits: &mut BTreeMap<RecordId, BTreeMap<String, serde_json::Value>>,
               id: &RecordId,
               key: &str,
               value: serde_json::Value|
     -> Result<()> {
        let fields = edits.entry(id.clone()).or_default();
        if fields
            .insert(key.into(), value.clone())
            .is_some_and(|old| old != value)
        {
            return Err(bad("conflicting exact semantic edits"));
        }
        Ok(())
    };
    let mut expected_main_edges = BTreeSet::new();
    for (op, a) in r.request.decisions.iter().zip(&r.allocations) {
        for id in sources(op) {
            let (_, n) = find(&logical.notes, &id)?;
            if n.canonical.as_ref().is_none_or(|rec| {
                rec.kind() != RecordKind::Entity || rec.string("wiki_status") != Some("active")
            }) {
                return Err(bad("logical absorbed source is not active entity"));
            }
            put(&mut edits, &id, "wiki_status", json!("superseded"))?;
            if let EntityDecision::MergeEntities { target_id, .. } = op {
                put(&mut edits, &id, "wiki_superseded_by_id", json!(target_id))?;
                put(
                    &mut edits,
                    &id,
                    "wiki_superseded_by",
                    json!(format!(
                        "[[{}]]",
                        r.record_paths
                            .get(target_id)
                            .ok_or_else(|| bad("missing original target path"))?
                    )),
                )?;
            }
        }
        if let EntityDecision::AddAlias {
            entity_id, alias, ..
        } = op
            && !(anchor.is_some()
                && r.record_paths
                    .get(entity_id)
                    .is_some_and(|path| !r.operations.iter().any(|op| &op.target == path)))
        {
            let (_, n) = find(&logical.notes, entity_id)?;
            let rec = n
                .canonical
                .as_ref()
                .ok_or_else(|| bad("original alias target invalid"))?;
            if rec.kind() != RecordKind::Entity || rec.string("wiki_status") != Some("active") {
                return Err(bad("original alias target inactive"));
            }
            let mut aliases = rec
                .field("aliases")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if !aliases.contains(&json!(alias)) {
                aliases.push(json!(alias));
            }
            put(&mut edits, entity_id, "aliases", json!(aliases))?;
        }
        let predecessors = if anchor.is_some() {
            r.main_supersessions
                .iter()
                .filter(|e| e.successor_id == a.decision_id)
                .map(|e| e.predecessor_id.clone())
                .collect()
        } else {
            super::decisions::predecessors(logical, op)?
        };
        for id in predecessors {
            if anchor.is_some() {
                verify_declared_anchor_predecessor(logical, op, &id)?;
            }
            expected_main_edges.insert((id.clone(), a.decision_id.clone()));
            put(&mut edits, &id, "wiki_status", json!("superseded"))?;
        }
        for m in &a.mention_decisions {
            put(
                &mut edits,
                &m.predecessor_id,
                "wiki_status",
                json!("superseded"),
            )?;
        }
        if let EntityDecision::SplitEntity { new_entities, .. } = op {
            for e in new_entities {
                let id = a
                    .entities
                    .get(&e.key)
                    .ok_or_else(|| bad("missing split allocation"))?;
                let (_, n) = find(&proposed.notes, id)?;
                if n.raw != super::decisions::entity_bytes(id, &e.title, &e.entity_type)? {
                    return Err(bad("new split entity bytes differ"));
                }
            }
        }
        let changed = remaps(op)
            .iter()
            .filter_map(|m| match m {
                EntityRemap::Assertion { assertion_id, .. } => Some(assertion_id.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        for note in current.notes.values().filter(|_| anchor.is_none()) {
            let Some(d) = &note.canonical else {
                continue;
            };
            if d.kind() != RecordKind::Decision || d.string("wiki_status") != Some("active") {
                continue;
            }
            let unexpected = match d.string("wiki_action") {
                Some("accept") => id_list(d, "wiki_input_ids")?
                    .into_iter()
                    .chain(id_list(d, "wiki_output_ids")?)
                    .any(|id| changed.contains(&id)),
                Some("merge" | "split") => id_list(d, "wiki_output_ids")?
                    .iter()
                    .any(|id| sources(op).contains(id)),
                _ => false,
            };
            if unexpected
                && !r
                    .main_supersessions
                    .iter()
                    .any(|e| e.predecessor_id == *d.id() && e.successor_id == a.decision_id)
            {
                return Err(conflict(
                    "new active decision makes guarded authority incomplete",
                ));
            }
        }
    }
    if expected_main_edges
        != r.main_supersessions
            .iter()
            .map(|e| (e.predecessor_id.clone(), e.successor_id.clone()))
            .collect()
    {
        return Err(bad("missing/spurious main or acceptance supersession edge"));
    }
    for p in &r.assertion_proofs {
        put(
            &mut edits,
            &p.assertion_id,
            field(&p.field),
            json!(p.after_entity_id),
        )?;
        put(
            &mut edits,
            &p.assertion_id,
            companion(&p.field),
            json!(format!(
                "[[{}]]",
                r.record_paths
                    .get(&p.after_entity_id)
                    .ok_or_else(|| bad("missing endpoint path"))?
            )),
        )?;
        let (_, n) = find(&logical.notes, &p.assertion_id)?;
        let rec = n
            .canonical
            .as_ref()
            .ok_or_else(|| bad("original assertion invalid"))?;
        if rec.string("wiki_status") == Some("accepted") {
            put(
                &mut edits,
                &p.assertion_id,
                "wiki_status",
                json!("proposed"),
            )?;
        }
    }
    for (id, fields) in edits {
        let (_, before) = find(&logical.notes, &id)?;
        let (_, after) = find(&proposed.notes, &id)?;
        let bytes = crate::records::edit_note(before, &fields, None, &before.source_hash)?;
        if bytes != after.raw {
            return Err(bad("record differs from exact request-authorized edit"));
        }
    }
    for p in &r.extraction_proofs {
        let (_, before) = find(&logical.notes, &p.extraction_id)?;
        let (_, after) = find(&proposed.notes, &p.extraction_id)?;
        let mut a = artifact(before)?;
        if a.bindings != p.prior_bindings
            || a.materialized_assertions != p.prior_materialized_assertions
            || mention_state::immutable_hash(&a)? != p.immutable_extraction_hash
        {
            return Err(bad("retained logical prior artifact differs"));
        }
        for t in &p.transitions {
            if a.bindings.get(&t.mention_id) != Some(&t.before) {
                return Err(bad("exact original binding differs"));
            }
            a.bindings.insert(t.mention_id.clone(), t.after.clone());
        }
        if import::edit_extraction_artifact(before, &a)? != after.raw {
            return Err(bad("extraction changed outside exact binding map"));
        }
    }
    Ok(())
}
