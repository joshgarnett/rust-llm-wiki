//! Query-scoped navigation over authenticated, unassessed Source annotations.
//! Generated scopes select node IDs only; exact evidence and citations remain
//! the nomination/admission/rendering caller's responsibility.
use super::{
    context_selection::{query_terms, term_key},
    excerpts::Tokenizer,
    source_relations::{MAX_NODES, ValidatedRelations},
    types::MAX_QUERY_BYTES,
};
use crate::{
    catalog::query_types::QueryCatalog,
    domain::{ErrorCode, RecordId, Result, WikiError},
};
use std::{collections::BTreeSet, time::Instant};

const MAX_OWNERS: usize = 10;
const MAX_ANCHORS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelationAnchor {
    pub owner_index: usize,
    pub node_id: RecordId,
    pub covered_terms: Vec<usize>,
    /// Sum of caller-supplied ordinary selection weights; never confidence.
    pub local_relevance: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RelationAnchorSelection {
    pub anchors: Vec<RelationAnchor>,
    pub matched_anchors: usize,
    /// Exact matching nodes excluded by the caller's anchor output allowance.
    /// This does not count dependency/span omissions from later nomination.
    pub omitted_anchors: usize,
    pub scanned_nodes: usize,
    pub scanned_scope_bytes: usize,
    /// Same query-term cap as ordinary selection; excluded terms remain unknown.
    pub query_terms_capped: bool,
}

fn check(reader: &dyn QueryCatalog, deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "source relation anchor selection deadline exceeded",
        ));
    }
    reader.check_query_budget()
}

/// `owners` must already be eligible, authenticated original Sources in the
/// enclosing query. Owner indexes retain ordinary discovery rank. This adapter
/// does no Source reads or hashing and grants no new proof/candidate allowance.
///
/// `term_weights` is the actual ordinary SelectionResult vector for this query;
/// frequencies are not recomputed. Every validated scope is inspected, including
/// nodes outside discovery seed spans. Distinct term matches use the ordinary
/// unicode61 tokenizer and term keys, with no truth/applicability interpretation.
/// `max_anchors` is supplied from remaining shared candidate capacity (0..=80).
/// The caller still reserves distinct exact spans/dependencies atomically.
pub(crate) fn rank_anchors(
    reader: &dyn QueryCatalog,
    query: &str,
    owners: &[(usize, &ValidatedRelations<'_>)],
    term_weights: &[u64],
    max_anchors: usize,
    deadline: Instant,
) -> Result<RelationAnchorSelection> {
    check(reader, deadline)?;
    if owners.len() > MAX_OWNERS || max_anchors > MAX_ANCHORS || query.len() > MAX_QUERY_BYTES {
        return Err(WikiError::invalid(
            "source relation anchor input exceeds query/owner/candidate bounds",
        ));
    }
    // Aggregate only already-validated metadata; never reload or rehash owners.
    let mut owner_indexes = BTreeSet::new();
    let mut scanned_nodes = 0usize;
    let mut scanned_scope_bytes = 0usize;
    for (owner_index, relations) in owners {
        check(reader, deadline)?;
        if !owner_indexes.insert(*owner_index) {
            return Err(WikiError::invalid("duplicate source relation owner index"));
        }
        let nodes = &relations.envelope().nodes;
        if nodes.len() > MAX_NODES {
            return Err(WikiError::invalid("source relation node bound exceeded"));
        }
        scanned_nodes = scanned_nodes
            .checked_add(nodes.len())
            .ok_or_else(|| WikiError::invalid("source relation aggregate node count overflow"))?;
        for node in nodes {
            check(reader, deadline)?;
            scanned_scope_bytes = scanned_scope_bytes
                .checked_add(node.scope.len())
                .ok_or_else(|| {
                    WikiError::invalid("source relation aggregate scope byte count overflow")
                })?;
        }
    }
    if scanned_nodes > MAX_OWNERS * MAX_NODES {
        return Err(WikiError::invalid(
            "source relation aggregate node bound exceeded",
        ));
    }
    let tokenizer = Tokenizer::new(reader.connection())?;
    check(reader, deadline)?;
    let (terms, query_terms_capped) = query_terms(&tokenizer, query)?;
    check(reader, deadline)?;
    if terms.len() != term_weights.len() {
        return Err(WikiError::invalid(
            "source relation query terms differ from ordinary selection weights",
        ));
    }
    let mut anchors = Vec::new();
    for (owner_index, relations) in owners {
        for node in &relations.envelope().nodes {
            // Validated scopes are <=256 bytes; each synchronous tokenizer call
            // is bounded, and the unchanged current deadline brackets it.
            check(reader, deadline)?;
            let tokens = tokenizer.tokens(&node.scope)?;
            check(reader, deadline)?;
            let mut covered_terms = BTreeSet::new();
            for token in tokens {
                check(reader, deadline)?;
                if let Some(index) = terms.get(&term_key(&token.text)) {
                    covered_terms.insert(*index);
                }
            }
            if covered_terms.is_empty() {
                continue;
            }
            let local_relevance = covered_terms.iter().try_fold(0u64, |sum, index| {
                sum.checked_add(term_weights[*index])
                    .ok_or_else(|| WikiError::invalid("source relation term weight sum overflow"))
            })?;
            anchors.push(RelationAnchor {
                owner_index: *owner_index,
                node_id: node.id.clone(),
                covered_terms: covered_terms.into_iter().collect(),
                local_relevance,
            });
        }
    }
    check(reader, deadline)?;
    anchors.sort_by(|a, b| {
        b.local_relevance
            .cmp(&a.local_relevance)
            .then(a.owner_index.cmp(&b.owner_index))
            .then(a.node_id.cmp(&b.node_id))
    });
    check(reader, deadline)?;
    let matched_anchors = anchors.len();
    anchors.truncate(max_anchors);
    Ok(RelationAnchorSelection {
        omitted_anchors: matched_anchors - anchors.len(),
        anchors,
        matched_anchors,
        scanned_nodes,
        scanned_scope_bytes,
        query_terms_capped,
    })
}

#[cfg(test)]
#[path = "source_relation_selection_tests.rs"]
mod tests;
