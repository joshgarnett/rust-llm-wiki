//! Source-only, unassessed navigation annotations. These are neither graph
//! assertions nor facts. The caller owns eligibility, accounting, authentication,
//! final admission and citation rendering; this module does no I/O or model work.
use crate::domain::{Blake3Hash, ByteSpan, ErrorCode, RecordId, Result, RevisionId, WikiError};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::Instant,
};

pub(crate) const SCHEMA: &str = "lwiki.source-relations.v1";
pub(crate) const MAX_ANNOTATION_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_NODES: usize = 4096;
pub(crate) const MAX_EDGES: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceBinding {
    pub source_id: RecordId,
    pub source_revision: RevisionId,
    /// Hash of original captured bytes, never a normalized/derived document.
    pub raw_hash: Blake3Hash,
}

/// All identities come from retained preparation authority, not model claims.
/// Model identity fingerprints the resolved provider/model/deployment. Recipe
/// hash covers the complete source-only recipe; settings hash covers all actual
/// generation parameters. The generation hash names the retained artifact,
/// rather than a self-hash of this envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparationBinding {
    pub recipe_id: RecordId,
    pub recipe_hash: Blake3Hash,
    pub prompt_hash: Blake3Hash,
    pub schema_hash: Blake3Hash,
    pub model_identity_hash: Blake3Hash,
    pub settings_hash: Blake3Hash,
    pub retained_generation_id: RecordId,
    pub retained_generation_hash: Blake3Hash,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpanBinding {
    pub span: ByteSpan,
    pub quote_hash: Blake3Hash,
}
impl SpanBinding {
    fn validate(&self, original: &str) -> Result<()> {
        let text = self.span.slice(original).map_err(|e| invalid(e.message))?;
        if self.span.is_empty() || Blake3Hash::digest(text) != self.quote_hash {
            return Err(invalid(
                "annotation span is empty or its original-byte hash differs",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NodeKind {
    Rule,
    Condition,
    Exception,
    Alternative,
    Requirement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RelationKind {
    Qualifies,
    Requires,
    Excepts,
    Alternative,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Node {
    pub id: RecordId,
    pub kind: NodeKind,
    /// Short generated navigation hint, never proposal text or a truth value.
    pub scope: String,
    pub evidence: SpanBinding,
}

/// Direction is always from the anchored rule/node to the dependent span.
/// `qualifies`: to qualifies from; `requires`: from requires to;
/// `excepts`: to is an exception to from; `alternative`: to is an alternative.
/// These labels establish no entailment or applicability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Edge {
    pub from: RecordId,
    pub to: RecordId,
    pub kind: RelationKind,
    /// Exact source bytes supporting the proposed relation, separately nominated.
    pub evidence: SpanBinding,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Envelope {
    pub schema: String,
    pub source: SourceBinding,
    pub preparation: PreparationBinding,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// Caller has already admitted this original through its selected proof. This
/// wrapper grants no membership authority. The parser verifies its hash once;
/// the resulting object borrows these same bytes for every later nomination.
#[derive(Clone, Copy)]
pub(crate) struct AdmittedOriginal<'a> {
    pub binding: &'a SourceBinding,
    pub text: &'a str,
}

pub(crate) struct ValidatedRelations<'a> {
    envelope: Envelope,
    original: AdmittedOriginal<'a>,
    nodes: BTreeMap<RecordId, usize>,
    outgoing: BTreeMap<RecordId, Vec<usize>>,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retain validated annotation-size receipt; tests verify exact serialized-byte accounting"
        )
    )]
    annotation_bytes: usize,
    validation_hash_bytes: u64,
}

/// Remaining allowance from the enclosing preparation/query proof, not a new
/// per-Source budget. Parsing performs no reads. Every hashed span occurrence
/// counts separately, even when offsets repeat.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ValidationLimits {
    pub max_hash_bytes: u64,
    pub deadline: Instant,
}

fn invalid(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::ExtractionInvalid, message)
}

/// Enriched envelope is assembled locally from model-returned exact span IDs;
/// models must not supply offsets, hashes or retained authority. The caller owns
/// that raw-ID resolution seam and must reject unknown IDs before assembly.
/// Exactly one typed JSON object; serde's struct decoders reject duplicate keys,
/// unknown fields (including nested fields), missing fields and trailing input.
/// Never salvage a partial/truncated response or silently discard excess items.
/// Input reads/hash verification and parse work must be reserved by the caller.
pub(crate) fn parse<'a>(
    bytes: &[u8],
    original: AdmittedOriginal<'a>,
    expected: &PreparationBinding,
    limits: ValidationLimits,
) -> Result<ValidatedRelations<'a>> {
    check(limits.deadline)?;
    if bytes.is_empty() || bytes.len() > MAX_ANNOTATION_BYTES {
        return Err(invalid(
            "source annotation byte ceiling exceeded or input empty",
        ));
    }
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))?;
    if envelope.schema != SCHEMA
        || &envelope.source != original.binding
        || &envelope.preparation != expected
    {
        return Err(invalid(
            "source annotation schema or retained provenance differs",
        ));
    }
    if envelope.nodes.len() > MAX_NODES || envelope.edges.len() > MAX_EDGES {
        return Err(invalid("source annotation item ceiling exceeded"));
    }
    // Preflight all range accesses and checked total work BEFORE any digest.
    let mut hash_bytes = original.text.len() as u64;
    for evidence in envelope
        .nodes
        .iter()
        .map(|n| &n.evidence)
        .chain(envelope.edges.iter().map(|e| &e.evidence))
    {
        check(limits.deadline)?;
        evidence
            .span
            .slice(original.text)
            .map_err(|e| invalid(e.message))?;
        if evidence.span.is_empty() {
            return Err(invalid("source annotation span is empty"));
        }
        hash_bytes = hash_bytes.checked_add(evidence.span.len()).ok_or_else(|| {
            WikiError::new(
                ErrorCode::BudgetExceeded,
                "source relation validation hash-byte overflow",
            )
        })?;
    }
    if hash_bytes > limits.max_hash_bytes {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "source relation validation hash-byte allowance exceeded",
        ));
    }
    let mut nodes = BTreeMap::new();
    for (index, node) in envelope.nodes.iter().enumerate() {
        if node.scope.trim().is_empty()
            || node.scope.len() > 256
            || node.scope.chars().any(char::is_control)
        {
            return Err(invalid(
                "source annotation scope must be a short nonempty label",
            ));
        }
        if nodes.insert(node.id.clone(), index).is_some() {
            return Err(invalid("duplicate source annotation node ID"));
        }
        check(limits.deadline)?;
    }
    let mut outgoing: BTreeMap<RecordId, Vec<usize>> = BTreeMap::new();
    let mut edges = BTreeSet::new();
    for (index, edge) in envelope.edges.iter().enumerate() {
        if !nodes.contains_key(&edge.from) || !nodes.contains_key(&edge.to) {
            return Err(invalid("source annotation edge references an unknown node"));
        }
        if !edges.insert((&edge.from, &edge.to, edge.kind)) {
            return Err(invalid("duplicate source annotation relation"));
        }
        check(limits.deadline)?;
        outgoing.entry(edge.from.clone()).or_default().push(index);
    }
    let mut consumed = 0u64;
    let validation = (|| -> Result<()> {
        check(limits.deadline)?;
        consumed += original.text.len() as u64;
        if Blake3Hash::digest(original.text) != original.binding.raw_hash {
            return Err(invalid("source annotation original-byte hash differs"));
        }
        for evidence in envelope
            .nodes
            .iter()
            .map(|n| &n.evidence)
            .chain(envelope.edges.iter().map(|e| &e.evidence))
        {
            check(limits.deadline)?;
            consumed += evidence.span.len();
            evidence.validate(original.text)?;
        }
        check(limits.deadline)
    })();
    if let Err(mut error) = validation {
        error.details = serde_json::json!({"source_relation_hash_bytes_consumed":consumed});
        return Err(error);
    }
    Ok(ValidatedRelations {
        envelope,
        original,
        nodes,
        outgoing,
        annotation_bytes: bytes.len(),
        validation_hash_bytes: consumed,
    })
}

/// Remaining allowances in the enclosing query, never additional allowances.
/// Every distinct exact span consumes one candidate, including relation evidence.
/// These are proposal caps: the caller still enforces four final passages/owner,
/// rendered bytes/tokens, owner/proof bounds, and final eligibility rechecks.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NominationLimits {
    pub max_candidates: usize,
    pub max_span_bytes: usize,
    pub max_text_bytes: usize,
    pub max_node_visits: usize,
    pub max_edge_visits: usize,
    pub max_depth: usize,
    pub deadline: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Assessment {
    Unassessed,
}

#[derive(Debug)]
pub(crate) struct SpanNomination<'a> {
    pub evidence: SpanBinding,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Preserve original-text binding on dependency nominations; tests audit exact source entailment coordinates"
        )
    )]
    pub text: &'a str,
}

#[derive(Debug)]
pub(crate) struct Dependency {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retain unassessed dependency identity and exact support closure for the default-off relation workflow; tests audit these links"
        )
    )]
    pub from: RecordId,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retain unassessed dependency identity and exact support closure for the default-off relation workflow; tests audit these links"
        )
    )]
    pub to: RecordId,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retain unassessed dependency identity and exact support closure for the default-off relation workflow; tests audit these links"
        )
    )]
    pub kind: RelationKind,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retain unassessed dependency identity and exact support closure for the default-off relation workflow; tests audit these links"
        )
    )]
    pub assessment: Assessment,
    /// Indexes into exact-span nominations; no generated label enters evidence.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retain unassessed dependency identity and exact support closure for the default-off relation workflow; tests audit these links"
        )
    )]
    pub from_candidate: usize,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retain unassessed dependency identity and exact support closure for the default-off relation workflow; tests audit these links"
        )
    )]
    pub to_candidate: usize,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retain unassessed dependency identity and exact support closure for the default-off relation workflow; tests audit these links"
        )
    )]
    pub relation_candidate: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::enum_variant_names,
    reason = "Retain distinct explicit limit reasons and their existing Debug receipt names for the relation workflow"
)]
pub(crate) enum OmissionReason {
    CandidateLimit,
    SpanLimit,
    TextLimit,
    NodeWorkLimit,
    EdgeWorkLimit,
    DepthLimit,
}

#[derive(Debug)]
pub(crate) struct Nominations<'a> {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Retain immutable source binding on relation nominations; tests verify source identity"
        )
    )]
    pub source: SourceBinding,
    pub spans: Vec<SpanNomination<'a>>,
    pub dependencies: Vec<Dependency>,
    /// Counts omitted immediate work, not unknown transitive closure size.
    pub omissions: Vec<(OmissionReason, usize)>,
    pub node_visits: usize,
    pub edge_visits: usize,
    pub text_bytes: usize,
    /// Known queued nodes not expanded when traversal stops at its edge cap.
    /// This is not an estimate of unknown descendants.
    pub unexpanded_nodes: usize,
}
impl Nominations<'_> {
    fn omit(&mut self, reason: OmissionReason, count: usize) {
        if count == 0 {
            return;
        }
        if let Some((_, value)) = self.omissions.iter_mut().find(|(r, _)| *r == reason) {
            *value += count;
        } else {
            self.omissions.push((reason, count));
        }
    }
}

impl<'a> ValidatedRelations<'a> {
    pub(crate) fn envelope(&self) -> &Envelope {
        &self.envelope
    }
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Expose the retained validation-size receipt to relation accounting tests"
        )
    )]
    pub(crate) fn annotation_bytes(&self) -> usize {
        self.annotation_bytes
    }
    pub(crate) fn validation_hash_bytes(&self) -> u64 {
        self.validation_hash_bytes
    }

    /// Caller chooses query anchors using existing admitted discovery. No query
    /// interpretation, ranking, condition evaluation or confidence threshold.
    /// A cycle is visited once; alternatives/conditions remain Unassessed.
    /// A dependency is returned only when BOTH endpoints AND its supporting span
    /// fit the same candidate/byte caps. Partial nomination remains disclosed.
    pub(crate) fn nominate(
        &self,
        anchors: &[RecordId],
        limits: NominationLimits,
    ) -> Result<Nominations<'a>> {
        check(limits.deadline)?;
        if limits.max_candidates > 80
            || limits.max_span_bytes > 1024
            || limits.max_text_bytes > 12000
            || limits.max_node_visits > MAX_NODES
            || limits.max_edge_visits > MAX_EDGES
            || anchors.len() > MAX_NODES
        {
            return Err(invalid("nomination limits exceed static bounds"));
        }
        if anchors.len() > limits.max_node_visits {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "source relation anchors exceed node work allowance",
            ));
        }
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::new();
        for anchor in anchors {
            check(limits.deadline)?;
            if !self.nodes.contains_key(anchor) || !seen.insert(anchor.clone()) {
                return Err(invalid("unknown or duplicate source relation anchor"));
            }
            queue.push_back((anchor.clone(), 0usize));
        }
        let mut result = Nominations {
            source: self.envelope.source.clone(),
            spans: Vec::new(),
            dependencies: Vec::new(),
            omissions: Vec::new(),
            node_visits: anchors.len(),
            edge_visits: 0,
            text_bytes: 0,
            unexpanded_nodes: 0,
        };
        let mut span_ids = BTreeMap::new();
        let mut expanded: BTreeSet<_> = anchors.iter().cloned().collect();
        while let Some((id, depth)) = queue.pop_front() {
            check(limits.deadline)?;
            let node = &self.envelope.nodes[self.nodes[&id]];
            let Some(from) = self.add_span(&node.evidence, limits, &mut result, &mut span_ids)?
            else {
                continue;
            };
            let Some(outgoing) = self.outgoing.get(&id) else {
                continue;
            };
            if depth >= limits.max_depth {
                result.omit(OmissionReason::DepthLimit, outgoing.len());
                continue;
            }
            for (position, index) in outgoing.iter().enumerate() {
                check(limits.deadline)?;
                if result.edge_visits == limits.max_edge_visits {
                    result.omit(OmissionReason::EdgeWorkLimit, outgoing.len() - position);
                    // Stop all traversal; remaining queued work stays unresolved.
                    result.unexpanded_nodes = queue.len();
                    return Ok(result);
                }
                result.edge_visits += 1;
                let edge = &self.envelope.edges[*index];
                // Visit target only within the node allowance, even when the
                // supporting span is already present from another dependency.
                if !seen.contains(&edge.to) {
                    if result.node_visits >= limits.max_node_visits {
                        result.omit(OmissionReason::NodeWorkLimit, 1);
                        continue;
                    }
                    result.node_visits += 1;
                    seen.insert(edge.to.clone());
                }
                let target = &self.envelope.nodes[self.nodes[&edge.to]];
                // Reserve the whole dependency atomically: no hidden evidence
                // or rejected dependency consuming candidate slots piecemeal.
                let mut pending = BTreeMap::new();
                for evidence in [&target.evidence, &edge.evidence] {
                    let key = (evidence.span.start(), evidence.span.end());
                    if !span_ids.contains_key(&key) {
                        pending.insert(key, evidence);
                    }
                }
                let reason = if pending
                    .values()
                    .any(|s| s.span.len() > limits.max_span_bytes as u64)
                {
                    Some(OmissionReason::SpanLimit)
                } else if result.spans.len() + pending.len() > limits.max_candidates {
                    Some(OmissionReason::CandidateLimit)
                } else if pending.values().map(|s| s.span.len()).sum::<u64>()
                    > limits.max_text_bytes.saturating_sub(result.text_bytes) as u64
                {
                    Some(OmissionReason::TextLimit)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    result.omit(reason, 1);
                    continue;
                }
                let to = self
                    .add_span(&target.evidence, limits, &mut result, &mut span_ids)?
                    .expect("reserved target span");
                let relation = self
                    .add_span(&edge.evidence, limits, &mut result, &mut span_ids)?
                    .expect("reserved relation span");
                if !expanded.contains(&edge.to) {
                    expanded.insert(edge.to.clone());
                    queue.push_back((edge.to.clone(), depth + 1));
                }
                result.dependencies.push(Dependency {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    kind: edge.kind,
                    assessment: Assessment::Unassessed,
                    from_candidate: from,
                    to_candidate: to,
                    relation_candidate: relation,
                });
            }
        }
        Ok(result)
    }

    fn add_span(
        &self,
        evidence: &SpanBinding,
        limits: NominationLimits,
        result: &mut Nominations<'a>,
        ids: &mut BTreeMap<(u64, u64), usize>,
    ) -> Result<Option<usize>> {
        check(limits.deadline)?;
        let key = (evidence.span.start(), evidence.span.end());
        if let Some(index) = ids.get(&key) {
            return Ok(Some(*index));
        }
        let reason = if evidence.span.len() > limits.max_span_bytes as u64 {
            Some(OmissionReason::SpanLimit)
        } else if result.spans.len() >= limits.max_candidates {
            Some(OmissionReason::CandidateLimit)
        } else if evidence.span.len()
            > limits.max_text_bytes.saturating_sub(result.text_bytes) as u64
        {
            Some(OmissionReason::TextLimit)
        } else {
            None
        };
        if let Some(reason) = reason {
            result.omit(reason, 1);
            return Ok(None);
        }
        let text = evidence
            .span
            .slice(self.original.text)
            .map_err(|e| invalid(e.message))?;
        let index = result.spans.len();
        result.text_bytes += text.len();
        result.spans.push(SpanNomination {
            evidence: evidence.clone(),
            text,
        });
        ids.insert(key, index);
        Ok(Some(index))
    }
}
fn check(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "source relation nomination deadline exceeded",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "source_relations_tests.rs"]
mod tests;
