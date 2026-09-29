//! Citation provenance checks for unassessed prose and page proposals.
use super::frontier::{self, StageLimits};
use crate::{
    domain::*,
    sources::{CitationScope, SourceView, VerifiedCitation},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub text: String,
    pub citations: Vec<CitationRef>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub heading: String,
    pub claims: Vec<Claim>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResearchProposal {
    CreatePage {
        title: String,
        body: String,
        citations: Vec<CitationRef>,
    },
    UpdatePage {
        record: RecordRef,
        body: String,
        citations: Vec<CitationRef>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Synthesis {
    pub sections: Vec<Section>,
    pub unanswered_questions: Vec<String>,
    pub proposed_changes: Vec<ResearchProposal>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Unassessed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimAssessment {
    pub section_index: usize,
    pub claim_index: usize,
    pub status: ClaimStatus,
    pub provenance_verified: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidatedSynthesis {
    pub output: Synthesis,
    pub verified_citations: Vec<VerifiedCitation>,
    pub claim_assessments: Vec<ClaimAssessment>,
    pub gaps: Vec<String>,
}
/// Membership binds exact identity; verification binds actual bytes, ownership and currentness.
/// Neither establishes entailment, and this API cannot accept assertions or apply pages.
pub fn validate(
    bytes: &[u8],
    limits: &StageLimits,
    allowed_citations: &[CitationRef],
    allowed_records: &[RecordRef],
    view: &SourceView<'_>,
) -> Result<ValidatedSynthesis> {
    let value = frontier::parse_value(bytes, limits)?;
    reference_shapes(&value)?;
    let output: Synthesis = serde_json::from_value(value)
        .map_err(|_| WikiError::invalid("research synthesis schema mismatch"))?;
    frontier::count(output.sections.len(), limits.max_sections)?;
    frontier::count(output.unanswered_questions.len(), limits.max_strings)?;
    frontier::count(output.proposed_changes.len(), limits.max_proposals)?;
    let mut result = ValidatedSynthesis {
        output,
        verified_citations: vec![],
        claim_assessments: vec![],
        gaps: vec![],
    };
    let mut claims = 0usize;
    let mut citations = 0usize;
    for (section_index, section) in result.output.sections.iter().enumerate() {
        frontier::text(&section.heading, limits.max_heading_bytes, false)?;
        claims = claims
            .checked_add(section.claims.len())
            .ok_or_else(|| WikiError::invalid("research claim count overflow"))?;
        frontier::count(claims, limits.max_claims)?;
        for (claim_index, claim) in section.claims.iter().enumerate() {
            frontier::text(&claim.text, limits.max_text_bytes, false)?;
            verify_citations(
                &claim.citations,
                limits,
                allowed_citations,
                view,
                &mut citations,
                &mut result.verified_citations,
            )?;
            result.claim_assessments.push(ClaimAssessment {
                section_index,
                claim_index,
                status: ClaimStatus::Unassessed,
                provenance_verified: !claim.citations.is_empty(),
            });
            if claim.citations.is_empty() {
                result.gaps.push(format!("Section {section_index} claim {claim_index} has no verified citation and remains unassessed."));
            }
        }
    }
    for question in &result.output.unanswered_questions {
        frontier::text(question, limits.max_text_bytes, false)?;
    }
    for proposal in &result.output.proposed_changes {
        let (body, refs) = match proposal {
            ResearchProposal::CreatePage {
                title,
                body,
                citations,
            } => {
                frontier::text(title, limits.max_heading_bytes, false)?;
                (body, citations)
            }
            ResearchProposal::UpdatePage {
                record,
                body,
                citations,
            } => {
                if record.expected_kind != RecordKind::Page || !allowed_records.contains(record) {
                    return Err(WikiError::invalid(
                        "research update is not a caller-authorized page reference",
                    ));
                }
                (body, citations)
            }
        };
        frontier::text(body, limits.max_text_bytes, false)?;
        verify_citations(
            refs,
            limits,
            allowed_citations,
            view,
            &mut citations,
            &mut result.verified_citations,
        )?;
    }
    Ok(result)
}
fn verify_citations(
    refs: &[CitationRef],
    limits: &StageLimits,
    allowed: &[CitationRef],
    view: &SourceView<'_>,
    count: &mut usize,
    verified: &mut Vec<VerifiedCitation>,
) -> Result<()> {
    *count = count
        .checked_add(refs.len())
        .ok_or_else(|| WikiError::invalid("research citation count overflow"))?;
    frontier::count(*count, limits.max_citations)?;
    let mut seen = Vec::new();
    for citation in refs {
        if !allowed.contains(citation) || seen.contains(&citation) {
            return Err(WikiError::invalid(
                "unknown or duplicate research citation identity",
            ));
        }
        seen.push(citation);
        let span = match citation {
            CitationRef::Source(reference) => reference.span,
            CitationRef::Assertion(reference) => reference.span,
        };
        if span.len() > limits.max_text_bytes as u64 {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "research citation quote byte ceiling",
            ));
        }
        let proof = view.verify(citation, CitationScope::Current)?;
        if !verified.iter().any(|p| &p.citation == citation) {
            verified.push(proof);
        }
    }
    Ok(())
}
// Shared domain reference DTOs are deliberately permissive in other contexts. This stage
// rejects unknown nested keys before deserializing them into those existing identity types.
fn exact(value: &Value, fields: &[&str]) -> Result<()> {
    let obj = value
        .as_object()
        .ok_or_else(|| WikiError::invalid("research reference must be an object"))?;
    if obj.len() != fields.len() || fields.iter().any(|key| !obj.contains_key(*key)) {
        return Err(WikiError::invalid(
            "research reference contains missing or unknown fields",
        ));
    }
    Ok(())
}
fn citation_shape(value: &Value) -> Result<()> {
    exact(value, &["kind", "reference"])?;
    let reference = &value["reference"];
    match value["kind"].as_str() {
        Some("source") => exact(
            reference,
            &["source_id", "source_revision", "span", "quote_hash"],
        )?,
        Some("assertion") => exact(
            reference,
            &[
                "evidence_id",
                "assertion_id",
                "source_id",
                "source_revision",
                "span",
                "quote_hash",
            ],
        )?,
        _ => return Err(WikiError::invalid("research citation kind invalid")),
    }
    exact(&reference["span"], &["start", "end"])
}
fn citation_array(value: &Value) -> Result<()> {
    for citation in value
        .as_array()
        .ok_or_else(|| WikiError::invalid("research citations must be an array"))?
    {
        citation_shape(citation)?;
    }
    Ok(())
}
fn reference_shapes(value: &Value) -> Result<()> {
    if let Some(sections) = value["sections"].as_array() {
        for section in sections {
            if let Some(claims) = section["claims"].as_array() {
                for claim in claims {
                    citation_array(&claim["citations"])?;
                }
            }
        }
    }
    if let Some(proposals) = value["proposed_changes"].as_array() {
        for proposal in proposals {
            citation_array(&proposal["citations"])?;
            if proposal["kind"] == "update_page" {
                exact(
                    &proposal["record"],
                    &["vault_id", "record_id", "expected_kind"],
                )?;
            }
        }
    }
    Ok(())
}
fn id_schema() -> Value {
    json!({"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$"})
}
fn citation_schema() -> Value {
    let span = frontier::object(
        json!({"start":{"type":"integer","minimum":0,"maximum":u64::MAX},"end":{"type":"integer","minimum":0,"maximum":u64::MAX}}),
        &["start", "end"],
    );
    let source = json!({"source_id":id_schema(),"source_revision":id_schema(),"span":span,"quote_hash":{"type":"string","pattern":"^blake3:[0-9a-f]{64}$"}});
    let mut evidence = source.clone();
    evidence["evidence_id"] = id_schema();
    evidence["assertion_id"] = id_schema();
    json!({"oneOf":[
        frontier::object(json!({"kind":{"const":"source"},"reference":frontier::object(source,&["source_id","source_revision","span","quote_hash"])}),&["kind","reference"]),
        frontier::object(json!({"kind":{"const":"assertion"},"reference":frontier::object(evidence,&["evidence_id","assertion_id","source_id","source_revision","span","quote_hash"])}),&["kind","reference"])
    ]})
}
pub fn schema() -> Value {
    let l = StageLimits::default();
    let citations = frontier::array(citation_schema(), l.max_citations);
    let claim = frontier::object(
        json!({"text":frontier::string(l.max_text_bytes,false),"citations":citations}),
        &["text", "citations"],
    );
    let section = frontier::object(
        json!({"heading":frontier::string(l.max_heading_bytes,false),"claims":frontier::array(claim,l.max_claims)}),
        &["heading", "claims"],
    );
    let record = frontier::object(
        json!({"vault_id":id_schema(),"record_id":id_schema(),"expected_kind":{"const":"page"}}),
        &["vault_id", "record_id", "expected_kind"],
    );
    let proposals = json!({"oneOf":[
        frontier::object(json!({"kind":{"const":"create_page"},"title":frontier::string(l.max_heading_bytes,false),"body":frontier::string(l.max_text_bytes,false),"citations":citations}),&["kind","title","body","citations"]),
        frontier::object(json!({"kind":{"const":"update_page"},"record":record,"body":frontier::string(l.max_text_bytes,false),"citations":citations}),&["kind","record","body","citations"])
    ]});
    frontier::envelope(
        frontier::object(
            json!({"sections":frontier::array(section,l.max_sections),"unanswered_questions":frontier::array(frontier::string(l.max_text_bytes,false),l.max_strings),"proposed_changes":frontier::array(proposals,l.max_proposals)}),
            &["sections", "unanswered_questions", "proposed_changes"],
        ),
        "urn:lwiki:research-synthesis:1",
    )
}
