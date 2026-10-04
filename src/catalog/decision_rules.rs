//! Pure Decision and graph rules. Inputs must include every Decision in the
//! affected overlap/supersession component, all its referenced rows and notes,
//! and complete, verified policy authority. An arbitrary selected map is not a
//! full-graph validator; callers must establish completeness before invoking.
use super::{
    scan::list,
    structural_rules::{self, StructuralRecorder, invalidate as mark_invalid},
    types::{CatalogDiagnostic, RecordRow},
};
use crate::{
    domain::{CanonicalRecord, Eligibility, RecordId, RecordKind, Result, VaultRelativePath},
    records::ParsedNote,
};
use std::collections::{BTreeMap, BTreeSet};

/// Already verified authority only; this interface performs no receipt scans.
pub(crate) trait DecisionPolicy {
    fn historical_alias_authority(&self, id: &RecordId) -> bool;
    fn decisions_compatible(&self, ids: &[RecordId]) -> bool;
}
impl DecisionPolicy for crate::graph::remap::VerifiedDecisionPolicy {
    fn historical_alias_authority(&self, id: &RecordId) -> bool {
        self.historical_alias_authority(id)
    }
    fn decisions_compatible(&self, ids: &[RecordId]) -> bool {
        self.decisions_compatible(ids)
    }
}

// Supersession maps successor to predecessors. Bound the combined ordinary,
// entity-decision and review graph independently of ID order or receipt type.
pub(crate) fn overlong_supersession_roots(
    edges: &BTreeMap<RecordId, BTreeSet<RecordId>>,
) -> BTreeSet<RecordId> {
    let mut pending = BTreeMap::new();
    let mut successors: BTreeMap<RecordId, BTreeSet<RecordId>> = BTreeMap::new();
    for (successor, predecessors) in edges {
        pending.insert(successor.clone(), predecessors.len());
        for predecessor in predecessors {
            successors
                .entry(predecessor.clone())
                .or_default()
                .insert(successor.clone());
        }
    }
    for predecessor in successors.keys() {
        pending.entry(predecessor.clone()).or_insert(0);
    }
    let mut ready: Vec<_> = pending
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| id.clone())
        .collect();
    let mut longest: BTreeMap<RecordId, usize> = BTreeMap::new();
    let mut invalid = BTreeSet::new();
    while let Some(id) = ready.pop() {
        let depth = longest.get(&id).copied().unwrap_or(0);
        if depth > crate::graph::MAX_AUTHORIZED_EVOLUTION_HOPS {
            invalid.insert(id.clone());
        }
        for successor in successors.get(&id).into_iter().flatten() {
            let next = longest.entry(successor.clone()).or_default();
            *next = (*next).max(depth + 1);
            let left = pending.get_mut(successor).expect("supersession node");
            *left -= 1;
            if *left == 0 {
                ready.push(successor.clone());
            }
        }
    }
    // Cycles are diagnosed separately; no receipt/ID order affects longest paths.
    invalid
}

pub(crate) fn cycle_members(edges: &BTreeMap<RecordId, BTreeSet<RecordId>>) -> BTreeSet<RecordId> {
    let mut cycles = BTreeSet::new();
    for start in edges.keys() {
        let mut visited = BTreeSet::new();
        let mut pending: Vec<_> = edges.get(start).into_iter().flatten().cloned().collect();
        while let Some(next) = pending.pop() {
            if &next == start {
                cycles.insert(start.clone());
                break;
            }
            if visited.insert(next.clone()) {
                pending.extend(edges.get(&next).into_iter().flatten().cloned());
            }
        }
    }
    cycles
}

fn is_mention_action(record: &CanonicalRecord) -> bool {
    matches!(
        record.string("wiki_action"),
        Some("bind_mention" | "create_entity" | "reject_mention")
    )
}
fn compatible_mention_operations(decisions: &[&RecordRow]) -> bool {
    let mut scopes = BTreeSet::new();
    let mut any_mentions = false;
    for row in decisions {
        if row.record.string("wiki_action") == Some("add_alias") {
            continue;
        }
        if !is_mention_action(&row.record) {
            return false;
        }
        any_mentions = true;
        let Some(extraction) = row.record.string("wiki_extraction_id") else {
            return false;
        };
        let mentions = list(&row.record, "wiki_mention_ids");
        if mentions.is_empty() {
            return false;
        }
        for mention in mentions {
            if !scopes.insert((extraction.to_owned(), mention)) {
                return false;
            }
        }
    }
    any_mentions
}
fn mention_authority(
    decision: &CanonicalRecord,
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    records: &BTreeMap<RecordId, RecordRow>,
) -> bool {
    use crate::graph::{ExtractionArtifactV1, MentionBinding, import::ARTIFACT_FENCE, packet};
    let Some(extraction) = decision
        .string("wiki_extraction_id")
        .and_then(|id| RecordId::new(id).ok())
    else {
        return false;
    };
    if !list(decision, "wiki_input_ids").contains(&extraction.as_str().to_owned()) {
        return false;
    }
    let Some(row) = records
        .get(&extraction)
        .filter(|r| r.record.kind() == RecordKind::Extraction)
    else {
        return false;
    };
    let Some(note) = notes.get(&row.path) else {
        return false;
    };
    let Ok(json) = packet::fenced_json(note, ARTIFACT_FENCE, crate::graph::MAX_ARTIFACT_BYTES)
    else {
        return false;
    };
    let Ok(artifact) =
        packet::decode::<ExtractionArtifactV1>(json, crate::graph::MAX_ARTIFACT_BYTES)
    else {
        return false;
    };
    if artifact.extraction_id != extraction
        || artifact.schema != crate::graph::EXTRACTION_STATE_SCHEMA
    {
        return false;
    }
    let Ok(response) = packet::decode::<crate::graph::ExtractionResponse>(
        artifact.raw_response.as_bytes(),
        crate::graph::MAX_RESPONSE_BYTES,
    ) else {
        return false;
    };
    if crate::graph::wire::artifact_membership(&artifact, &response).is_err()
        || row.record.string("wiki_packet_id") != Some(artifact.packet_id.as_str())
        || row.record.string("wiki_input_hash") != Some(artifact.packet_fingerprint.as_str())
    {
        return false;
    }
    let mentions = list(decision, "wiki_mention_ids");
    let outputs = list(decision, "wiki_output_ids");
    if mentions.is_empty() || mentions.iter().collect::<BTreeSet<_>>().len() != mentions.len() {
        return false;
    }
    for mention in mentions {
        let Ok(local) = crate::graph::PacketLocalId::new(mention) else {
            return false;
        };
        match artifact.bindings.get(&local) {
            Some(MentionBinding::Resolved {
                entity_id,
                decision_id,
            }) => {
                if decision_id != decision.id()
                    || !matches!(
                        decision.string("wiki_action"),
                        Some("bind_mention" | "create_entity")
                    )
                    || outputs != [entity_id.as_str()]
                {
                    return false;
                }
            }
            Some(MentionBinding::Rejected { decision_id }) => {
                if decision_id != decision.id()
                    || decision.string("wiki_action") != Some("reject_mention")
                    || !outputs.is_empty()
                {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

pub(crate) fn apply_decisions(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    records: &mut BTreeMap<RecordId, RecordRow>,
    diagnostics: &mut Vec<CatalogDiagnostic>,
    decision_policy: Option<&dyn DecisionPolicy>,
    review_edges: &BTreeSet<(RecordId, RecordId)>,
    recorder: &mut StructuralRecorder,
) -> Result<()> {
    let decisions: Vec<_> = records
        .values()
        .filter(|r| r.record.kind() == RecordKind::Decision)
        .cloned()
        .collect();
    // A malformed/cyclic superseder has no authority to disable its predecessor.
    let mut affected: BTreeMap<RecordId, Vec<&RecordRow>> = BTreeMap::new();
    for decision in &decisions {
        recorder.producer(Some(decision.record.id().clone()));
        if decision.record.string("wiki_status") != Some("active")
            || decision.eligibility == Eligibility::Invalid
        {
            continue;
        }
        let inputs: BTreeSet<_> = list(&decision.record, "wiki_input_ids")
            .into_iter()
            .collect();
        let outputs: BTreeSet<_> = list(&decision.record, "wiki_output_ids")
            .into_iter()
            .collect();
        for value in inputs.union(&outputs) {
            affected
                .entry(RecordId::new(value)?)
                .or_default()
                .push(decision);
        }
        let mut disagreements = Vec::new();
        if is_mention_action(&decision.record)
            && !mention_authority(&decision.record, notes, records)
        {
            disagreements.push((
                decision.record.id().clone(),
                "decision_mention_mapping_disagreement",
            ));
            if let Some(extraction) = decision.record.string("wiki_extraction_id") {
                disagreements.push((
                    RecordId::new(extraction)?,
                    "decision_mention_mapping_disagreement",
                ));
            }
        }
        let action = decision
            .record
            .string("wiki_action")
            .expect("validated action");
        let targets: Vec<_> = inputs
            .union(&outputs)
            .filter_map(|value| RecordId::new(value).ok())
            .filter_map(|id| records.get(&id))
            .collect();
        match action {
            "accept" | "reject" => {
                if targets.is_empty() {
                    disagreements.push((
                        decision.record.id().clone(),
                        "decision_missing_assertion_outcome",
                    ));
                }
                let required = if action == "accept" {
                    "accepted"
                } else {
                    "rejected"
                };
                for target in &targets {
                    if target.record.kind() != RecordKind::Assertion
                        || target.record.string("wiki_status") != Some(required)
                    {
                        disagreements
                            .push((target.record.id().clone(), "decision_status_disagreement"));
                    }
                }
            }
            "merge" | "split" => {
                let unique_output = if outputs.len() == 1 {
                    outputs.iter().next().map(String::as_str)
                } else {
                    None
                };
                for value in &inputs {
                    if let Some(target) = records.get(&RecordId::new(value)?)
                        && (target.record.kind() != RecordKind::Entity
                            || target.record.string("wiki_status") != Some("superseded")
                            || action == "merge"
                                && target.record.string("wiki_superseded_by_id") != unique_output)
                    {
                        disagreements.push((
                            target.record.id().clone(),
                            "decision_entity_outcome_disagreement",
                        ));
                    }
                }
                for value in &outputs {
                    if let Some(target) = records.get(&RecordId::new(value)?)
                        && (target.record.kind() != RecordKind::Entity
                            || target.record.string("wiki_status") != Some("active"))
                    {
                        disagreements.push((
                            target.record.id().clone(),
                            "decision_entity_outcome_disagreement",
                        ));
                    }
                }
                if inputs.is_empty()
                    || outputs.is_empty()
                    || action == "merge" && outputs.len() != 1
                {
                    disagreements.push((
                        decision.record.id().clone(),
                        "decision_missing_entity_outcome",
                    ));
                }
            }
            "create_entity" | "bind_mention" | "add_alias" => {
                // A desired label/mention binding is carried in the later P09 exact
                // operations/packet contract. Existing v1 fields prove only identity.
                let entity_targets: Vec<_> = if action == "add_alias" {
                    targets.clone()
                } else {
                    outputs
                        .iter()
                        .filter_map(|value| RecordId::new(value).ok())
                        .filter_map(|id| records.get(&id))
                        .collect()
                };
                if entity_targets.is_empty() {
                    disagreements.push((
                        decision.record.id().clone(),
                        "decision_missing_entity_outcome",
                    ));
                }
                for target in entity_targets {
                    let historical_alias = action == "add_alias"
                        && target.record.string("wiki_status") == Some("superseded")
                        && decision_policy.is_some_and(|policy| {
                            policy.historical_alias_authority(decision.record.id())
                        });
                    if target.record.kind() != RecordKind::Entity
                        || target.record.string("wiki_status") != Some("active")
                            && !historical_alias
                    {
                        disagreements.push((
                            target.record.id().clone(),
                            "decision_entity_outcome_disagreement",
                        ));
                    }
                }
            }
            "correct" => {
                for value in inputs.difference(&outputs) {
                    if let Some(target) = records.get(&RecordId::new(value)?) {
                        let required = match target.record.kind() {
                            RecordKind::Assertion => Some("superseded"),
                            RecordKind::Evidence => Some("retracted"),
                            RecordKind::Entity => Some("superseded"),
                            _ => None,
                        };
                        if required.is_some_and(|required| {
                            target.record.string("wiki_status") != Some(required)
                        }) {
                            disagreements.push((
                                target.record.id().clone(),
                                "decision_correction_predecessor_disagreement",
                            ));
                        }
                    }
                }
                for value in &outputs {
                    if let Some(target) = records.get(&RecordId::new(value)?)
                        && target.record.kind() == RecordKind::Evidence
                    {
                        let predecessor = target.record.string("wiki_supersedes_id");
                        if predecessor.is_none_or(|id| !inputs.contains(id)) {
                            disagreements.push((
                                target.record.id().clone(),
                                "decision_correction_successor_disagreement",
                            ));
                        }
                    }
                }
                if outputs.is_empty() {
                    disagreements.push((
                        decision.record.id().clone(),
                        "decision_missing_correction_outcome",
                    ));
                }
            }
            "reject_mention" => {}
            _ => unreachable!("validated decision action"),
        }
        if action == "add_alias"
            && decision_policy
                .is_some_and(|policy| policy.historical_alias_authority(decision.record.id()))
            && targets
                .iter()
                .any(|target| target.record.string("wiki_status") == Some("superseded"))
            && let Some(row) = records.get_mut(decision.record.id())
        {
            structural_rules::state(
                row,
                Eligibility::Historical,
                "alias_identity_superseded",
                recorder,
            );
        }
        for (id, reason) in disagreements {
            if let Some(row) = records.get_mut(&id) {
                mark_invalid(
                    row,
                    reason,
                    diagnostics,
                    serde_json::json!({"decision":decision.record.id()}),
                    recorder,
                );
            }
            if let Some(row) = records.get_mut(decision.record.id()) {
                mark_invalid(
                    row,
                    "decision_outcome_disagreement",
                    diagnostics,
                    serde_json::json!({"target":id,"reason":reason}),
                    recorder,
                );
            }
        }
    }
    let mut explicitly_superseded: BTreeSet<_> = records
        .values()
        .filter(|r| {
            r.record.kind() == RecordKind::Decision && r.eligibility != Eligibility::Invalid
        })
        .filter_map(|r| r.record.string("wiki_supersedes_id"))
        .map(str::to_owned)
        .collect();
    explicitly_superseded.extend(review_edges.iter().map(|(before, _)| before.to_string()));
    for decision in &decisions {
        recorder.producer(Some(decision.record.id().clone()));
        if decision.record.string("wiki_status") == Some("active")
            && explicitly_superseded.contains(decision.record.id().as_str())
            && let Some(row) = records.get_mut(decision.record.id())
        {
            mark_invalid(
                row,
                "active_superseded_decision",
                diagnostics,
                serde_json::Value::Null,
                recorder,
            );
        }
    }
    for (id, decisions) in affected {
        recorder.producer(Some(id.clone()));
        let actions: BTreeSet<_> = decisions
            .iter()
            .map(|r| {
                (
                    r.record.string("wiki_action"),
                    list(&r.record, "wiki_input_ids")
                        .into_iter()
                        .collect::<BTreeSet<_>>(),
                    list(&r.record, "wiki_output_ids")
                        .into_iter()
                        .collect::<BTreeSet<_>>(),
                )
            })
            .collect();
        // Multiple alias additions commute because their result is already explicit
        // in canonical aliases; all other incompatible overlapping outcomes conflict.
        let compatible_aliases = decisions
            .iter()
            .all(|r| r.record.string("wiki_action") == Some("add_alias"));
        let compatible_mentions = compatible_mention_operations(&decisions);
        let compatible_entity_decisions = decision_policy.is_some_and(|policy| {
            policy.decisions_compatible(
                &decisions
                    .iter()
                    .map(|r| r.record.id().clone())
                    .collect::<Vec<_>>(),
            )
        });
        if actions.len() > 1
            && !compatible_aliases
            && !compatible_mentions
            && !compatible_entity_decisions
        {
            if let Some(row) = records.get_mut(&id) {
                mark_invalid(
                    row,
                    "conflicting_active_decisions",
                    diagnostics,
                    serde_json::json!({"decisions":decisions.iter().map(|r|r.record.id()).collect::<Vec<_>>()}),
                    recorder,
                );
            }
            for decision in &decisions {
                if let Some(row) = records.get_mut(decision.record.id()) {
                    mark_invalid(
                        row,
                        "conflicting_active_decisions",
                        diagnostics,
                        serde_json::json!({"target":id}),
                        recorder,
                    );
                }
            }
        }
    }
    Ok(())
}
