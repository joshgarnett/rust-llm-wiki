//! Complete unassessed relation groups from already admitted discovery owners.
//! Metadata chooses spans only. Original bytes and ordinary citation rendering
//! remain authoritative; rejected closures never yield a rescued head passage.
use super::{
    context::{self, DocumentOwner, Packet, RelationGroup},
    context_types::{ContextOmission, ContextRequest, ContextScope, ContextSelectionSignals},
    source_relation_selection::rank_anchors,
    source_relations::{self, AdmittedOriginal, NominationLimits, SourceBinding, ValidationLimits},
    types::{HitSet, SearchExcerpt},
};
use crate::{catalog::query_types::QueryCatalog, domain::*};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
    time::Instant,
};

const MAX_OWNERS: usize = 10;
const OWNER_SPANS: usize = 32;
const GROUP_PASSAGES: usize = 4;
const LOCAL_VALIDATION_HASH_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Default)]
pub(super) struct GroupSelection {
    pub packets: Vec<Packet>,
    /// Shared with ordinary selection; distinct exact ranges, not discovery hits.
    pub reserved_spans: BTreeMap<usize, BTreeSet<(u64, u64)>>,
    pub validation_hash_bytes: u64,
    /// Actual citation quote hashing by document_passage, separately from parse.
    pub passage_hash_bytes: u64,
    pub node_visits: usize,
    pub edge_visits: usize,
}

fn check(reader: &dyn QueryCatalog, deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "source relation group deadline exceeded",
        ));
    }
    reader.check_query_budget()
}

fn omit(omissions: &mut Vec<ContextOmission>, owner: &DocumentOwner, reason: &str, count: usize) {
    if count != 0 {
        omissions.push(ContextOmission {
            record_id: owner.document.owner_revision.clone(),
            path: Some(owner.document.path.clone()),
            reason: reason.into(),
            count,
        });
    }
}

/// The proof layer supplies authenticated annotation DTOs and its unchanged
/// deadline. This boundary deliberately revalidates against the admitted cited
/// content (document.hash/raw_text), never original.bin or caller-asserted hashes.
/// The 64 MiB parse allowance is local assembly work, not new canonical proof
/// authority. It is cumulative across owners. Traversal work is also cumulative
/// across anchors, including groups later rejected. No source/catalog reads.
#[expect(
    clippy::too_many_arguments,
    reason = "Relation nomination keeps authenticated owners, request limits, query signal and omission sink explicit"
)]
pub(super) fn nominate_groups(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    hits: &HitSet,
    owners: &[DocumentOwner],
    query: &str,
    signals: &ContextSelectionSignals,
    term_weights: &[u64],
    omissions: &mut Vec<ContextOmission>,
) -> Result<GroupSelection> {
    let mut result = GroupSelection::default();
    if signals.relations.is_empty() {
        return Ok(result);
    }
    let attempt = assemble(
        reader,
        request,
        hits,
        owners,
        query,
        signals,
        term_weights,
        omissions,
        &mut result,
    );
    if let Err(mut error) = attempt {
        // Preserve child details and measured work on a failed query. A child
        // traversal error marks its unfinished visit count explicitly unknown.
        let child = std::mem::replace(&mut error.details, serde_json::Value::Null);
        error.details = serde_json::json!({
            "source_relation_group_validation_hash_bytes": result.validation_hash_bytes,
            "source_relation_group_passage_hash_bytes": result.passage_hash_bytes,
            "source_relation_group_node_visits": result.node_visits,
            "source_relation_group_edge_visits": result.edge_visits,
            "cause": child,
        });
        return Err(error);
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn assemble(
    reader: &dyn QueryCatalog,
    request: &ContextRequest,
    hits: &HitSet,
    owners: &[DocumentOwner],
    query: &str,
    signals: &ContextSelectionSignals,
    term_weights: &[u64],
    omissions: &mut Vec<ContextOmission>,
    result: &mut GroupSelection,
) -> Result<()> {
    let deadline = signals
        .relation_deadline
        .ok_or_else(|| WikiError::invalid("source relation proof deadline missing"))?;
    check(reader, deadline)?;
    if owners.len() > MAX_OWNERS
        || owners.len() != hits.hits.len()
        || signals.relations.len() > MAX_OWNERS
    {
        return Err(WikiError::invalid(
            "source relation owner/annotation bounds differ from discovery",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut inputs = Vec::new();
    for annotation in &signals.relations {
        check(reader, deadline)?;
        if !seen.insert(&annotation.owner) {
            return Err(WikiError::invalid(
                "duplicate source relation annotation owner",
            ));
        }
        let Some(index) = owners
            .iter()
            .position(|owner| owner.document.path == annotation.owner)
        else {
            // Signals may include authenticated owners outside this filtered
            // discovery subset. They never create a new retrieval owner.
            omissions.push(ContextOmission {
                record_id: None,
                path: Some(annotation.owner.clone()),
                reason: "source_relation_owner_outside_discovery".into(),
                count: 1,
            });
            continue;
        };
        let owner = &owners[index];
        let hit = &hits.hits[index];
        if !owner.allowed {
            omit(omissions, owner, "source_relation_owner_ineligible", 1);
            continue;
        }
        if owner.document.path != hit.locator.path
            || owner.document.hash != hit.locator.observed_hash
        {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "source relation discovery owner differs",
            ));
        }
        let Some(seed) =
            context::document_passage(reader, owner, hit, &hit.excerpt, request, index + 1)?
        else {
            omit(omissions, owner, "source_relation_seed_ineligible", 1);
            continue;
        };
        if request.scope != ContextScope::Snapshot && owner.document.owner_revision.is_some() {
            result.passage_hash_bytes += seed.span.len();
        }
        check(reader, deadline)?;
        let (Some(source_id), Some(source_revision)) =
            (&owner.document.source_id, &owner.document.owner_revision)
        else {
            return Err(WikiError::invalid(
                "source relation annotation has no captured owner",
            ));
        };
        inputs.push((
            index,
            SourceBinding {
                source_id: source_id.clone(),
                source_revision: source_revision.clone(),
                raw_hash: owner.document.hash.clone(),
            },
            annotation,
        ));
    }
    if inputs.is_empty() {
        return Ok(());
    }
    let mut validated = Vec::new();
    for (index, binding, annotation) in &inputs {
        check(reader, deadline)?;
        if annotation.envelope.nodes.len() > source_relations::MAX_NODES
            || annotation.envelope.edges.len() > source_relations::MAX_EDGES
        {
            return Err(WikiError::new(
                ErrorCode::ExtractionInvalid,
                "source relation annotation count ceiling exceeded",
            ));
        }
        // Bounded writer avoids allocating an oversized serialized DTO before
        // enforcing the existing one-MiB annotation ceiling. No truncation.
        let mut bytes = AnnotationBytes {
            bytes: Vec::new(),
            deadline,
        };
        serde_json::to_writer(&mut bytes, &annotation.envelope).map_err(|error| {
            WikiError::new(
                if Instant::now() >= deadline {
                    ErrorCode::BudgetExceeded
                } else {
                    ErrorCode::ExtractionInvalid
                },
                error.to_string(),
            )
        })?;
        check(reader, deadline)?;
        let parsed = source_relations::parse(
            &bytes.bytes,
            AdmittedOriginal {
                binding,
                text: &owners[*index].document.raw_text,
            },
            &annotation.envelope.preparation,
            ValidationLimits {
                max_hash_bytes: LOCAL_VALIDATION_HASH_BYTES - result.validation_hash_bytes,
                deadline,
            },
        );
        let relations = match parsed {
            Ok(relations) => relations,
            Err(error) => {
                result.validation_hash_bytes += error
                    .details
                    .get("source_relation_hash_bytes_consumed")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                return Err(error);
            }
        };
        result.validation_hash_bytes += relations.validation_hash_bytes();
        validated.push((*index, relations));
    }
    let references: Vec<_> = validated
        .iter()
        .map(|(index, relations)| (*index, relations))
        .collect();
    let ranked = rank_anchors(reader, query, &references, term_weights, 80, deadline)?;
    if ranked.omitted_anchors != 0 {
        omissions.push(ContextOmission {
            record_id: None,
            path: None,
            reason: "source_relation_anchor_cap".into(),
            count: ranked.omitted_anchors,
        });
    }
    if ranked.query_terms_capped {
        omissions.push(ContextOmission {
            record_id: None,
            path: None,
            reason: "source_relation_query_terms_capped".into(),
            count: 1,
        });
    }
    for (ordinal, anchor) in ranked.anchors.into_iter().enumerate() {
        check(reader, deadline)?;
        let owner = &owners[anchor.owner_index];
        if result.node_visits == source_relations::MAX_NODES {
            omit(omissions, owner, "source_relation_group_node_work_cap", 1);
            continue;
        }
        let relations = &validated
            .iter()
            .find(|(index, _)| *index == anchor.owner_index)
            .expect("ranked validated owner")
            .1;
        let nomination = relations
            .nominate(
                std::slice::from_ref(&anchor.node_id),
                NominationLimits {
                    max_candidates: OWNER_SPANS,
                    max_span_bytes: request.documents.limits.excerpt_bytes.min(1024),
                    max_text_bytes: request.budget.max_bytes.min(12000),
                    max_node_visits: source_relations::MAX_NODES - result.node_visits,
                    max_edge_visits: source_relations::MAX_EDGES - result.edge_visits,
                    max_depth: 32,
                    deadline,
                },
            )
            .map_err(|mut error| {
                let child = std::mem::replace(&mut error.details, serde_json::Value::Null);
                error.details = serde_json::json!({
                    "source_relation_group_traversal_usage_complete": false,
                    "cause": child,
                });
                error
            })?;
        result.node_visits += nomination.node_visits;
        result.edge_visits += nomination.edge_visits;
        if !nomination.omissions.is_empty() || nomination.unexpanded_nodes != 0 {
            let reason = format!(
                "source_relation_group_incomplete:{:?}",
                nomination.omissions
            );
            omit(omissions, owner, &reason, 1);
            continue;
        }
        if nomination.spans.is_empty() || nomination.spans.len() > GROUP_PASSAGES {
            omit(omissions, owner, "source_relation_group_passage_cap", 1);
            continue;
        }
        let spans: BTreeSet<_> = nomination
            .spans
            .iter()
            .map(|span| (span.evidence.span.start(), span.evidence.span.end()))
            .collect();
        let reserved = result.reserved_spans.get(&anchor.owner_index);
        let new_count = spans
            .iter()
            .filter(|span| !reserved.is_some_and(|set| set.contains(*span)))
            .count();
        if reserved.map_or(0, BTreeSet::len) + new_count > OWNER_SPANS {
            omit(omissions, owner, "source_relation_group_owner_span_cap", 1);
            continue;
        }
        // Retained pool is 32/owner (<=320), independently of 80 discovery or
        // anchor proposals. Never shrink ordinary selection to global 80.
        let total: usize = result.reserved_spans.values().map(BTreeSet::len).sum();
        if total + new_count > OWNER_SPANS * owners.len() {
            return Err(WikiError::invalid(
                "source relation retained span accounting exceeds owner pool",
            ));
        }
        let mut passages = Vec::new();
        for span in &nomination.spans {
            check(reader, deadline)?;
            let excerpt = SearchExcerpt {
                text: String::new(),
                span: span.evidence.span,
                matched_spans: vec![],
                label: hits.hits[anchor.owner_index].excerpt.label,
                citation: None,
            };
            let Some(passage) = context::document_passage(
                reader,
                owner,
                &hits.hits[anchor.owner_index],
                &excerpt,
                request,
                anchor.owner_index + 1,
            )?
            else {
                break;
            };
            if request.scope != ContextScope::Snapshot {
                result.passage_hash_bytes += passage.span.len();
            }
            passages.push(passage);
        }
        check(reader, deadline)?;
        if passages.len() != nomination.spans.len() {
            omit(
                omissions,
                owner,
                "source_relation_group_passage_ineligible",
                1,
            );
            continue;
        }
        let original_bytes = union_bytes(&spans);
        result
            .reserved_spans
            .entry(anchor.owner_index)
            .or_default()
            .extend(spans);
        result.packets.push(Packet {
            passages,
            bundle: None,
            navigation: None,
            key: format!(
                "relation:{}:{}",
                owner.document.path.as_str(),
                anchor.node_id.as_str()
            ),
            score: 0.0,
            selection_ordinal: Some(ordinal),
            selection: None,
            unit_score: None,
            unit_origin: None,
            fallback: None,
            unit_clipped: false,
            relation: Some(RelationGroup {
                covered_terms: anchor.covered_terms,
                local_relevance: anchor.local_relevance,
                original_bytes,
            }),
        });
    }
    check(reader, deadline)
}

fn union_bytes(spans: &BTreeSet<(u64, u64)>) -> u64 {
    let mut total = 0;
    let mut merged: Option<(u64, u64)> = None;
    for &(start, end) in spans {
        match merged {
            Some((left, right)) if start <= right => merged = Some((left, right.max(end))),
            Some((left, right)) => {
                total += right - left;
                merged = Some((start, end));
            }
            None => merged = Some((start, end)),
        }
    }
    total + merged.map_or(0, |(left, right)| right - left)
}

struct AnnotationBytes {
    bytes: Vec<u8>,
    deadline: Instant,
}
impl Write for AnnotationBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "source relation serialization deadline",
            ));
        }
        if bytes.len() > source_relations::MAX_ANNOTATION_BYTES.saturating_sub(self.bytes.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source relation annotation byte ceiling",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "context_relation_groups_tests.rs"]
mod tests;
