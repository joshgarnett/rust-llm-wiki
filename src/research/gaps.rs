//! Bounded assessment proposals over caller-selected current evidence IDs.
use super::frontier::{self, StageLimits};
use crate::domain::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GapAssessment {
    pub covered_evidence_ids: Vec<RecordId>,
    pub gaps: Vec<String>,
    pub next_queries: Vec<String>,
    pub next_urls: Vec<String>,
    pub stop: bool,
}
pub fn validate(
    bytes: &[u8],
    limits: &StageLimits,
    current_evidence: &BTreeSet<RecordId>,
    exclusions: &[String],
) -> Result<GapAssessment> {
    let output: GapAssessment = frontier::parse(bytes, limits)?;
    frontier::count(output.covered_evidence_ids.len(), limits.max_citations)?;
    frontier::count(output.gaps.len(), limits.max_strings)?;
    let mut seen = BTreeSet::new();
    for id in &output.covered_evidence_ids {
        if !current_evidence.contains(id) || !seen.insert(id) {
            return Err(WikiError::invalid(
                "unknown or duplicate research evidence ID",
            ));
        }
    }
    for gap in &output.gaps {
        frontier::text(gap, limits.max_text_bytes, false)?;
    }
    frontier::leads(&output.next_queries, &output.next_urls, limits, exclusions)?;
    if output.stop && (!output.next_queries.is_empty() || !output.next_urls.is_empty()) {
        return Err(WikiError::invalid(
            "stopped assessment contains continuation leads",
        ));
    }
    Ok(output)
}
pub fn schema() -> Value {
    let l = StageLimits::default();
    frontier::envelope(
        frontier::object(
            json!({
                "covered_evidence_ids":frontier::array(json!({"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$"}),l.max_citations),
                "gaps":frontier::array(frontier::string(l.max_text_bytes,false),l.max_strings),
                "next_queries":frontier::array(frontier::string(600,false),l.max_queries),
                "next_urls":frontier::array(frontier::string(l.max_url_bytes,false),l.max_urls),"stop":{"type":"boolean"}
            }),
            &[
                "covered_evidence_ids",
                "gaps",
                "next_queries",
                "next_urls",
                "stop",
            ],
        ),
        "urn:lwiki:research-gaps:1",
    )
}
