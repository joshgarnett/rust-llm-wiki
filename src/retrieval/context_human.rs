//! Present admitted evidence without introducing another evidence allocator.
use crate::retrieval::{context_types::*, types::ExcerptLabel};
use crate::{
    domain::*,
    graph::{GraphObject, GraphPathStep, GraphQualifiers, NavigationEdge, NavigationReason},
    sources::CitationState,
};
use std::collections::BTreeSet;

/// Observations made by the canonical renderer, not another catalog read.
/// Each trial owns only the state of its own passages and paths. Rejected trials
/// are dropped; this does not accumulate a cache across allocation attempts.
#[derive(Default)]
pub(super) struct ObservedStates {
    pub citations: Vec<Vec<CitationState>>,
    pub paths: Vec<Vec<(Eligibility, Option<String>)>>,
}

fn pair(citation: &CitationRef) -> (&RecordId, &RecordId) {
    match citation {
        CitationRef::Source(reference) => (&reference.source_id, &reference.source_revision),
        CitationRef::Assertion(reference) => (&reference.source_id, &reference.source_revision),
    }
}
fn state(value: Eligibility) -> &'static str {
    match value {
        Eligibility::Current => "current",
        Eligibility::Historical => "historical",
        Eligibility::Stale => "stale",
        Eligibility::Invalid => "invalid",
        Eligibility::Withdrawn => "withdrawn",
        Eligibility::Unsupported => "unsupported",
    }
}
fn citation_state(value: CitationState) -> &'static str {
    match value {
        CitationState::Current => "current",
        CitationState::Historical => "historical",
        CitationState::Withdrawn => "withdrawn",
    }
}
fn quoted(value: &str) -> String {
    // JSON string escaping keeps metadata on one line and preserves its value.
    // These are scalar labels, never replacement citation objects.
    serde_json::to_string(value).expect("string serialization")
}
fn optional(value: &Option<String>) -> String {
    value
        .as_deref()
        .map(quoted)
        .unwrap_or_else(|| "none".into())
}
fn debug_optional(value: &Option<String>) -> String {
    // Bundle/path statuses use Rust Debug in canonical text. Keep that exact
    // scalar escaping (including NUL's short escape), removing only Some(...).
    value
        .as_ref()
        .map(|value| format!("{value:?}"))
        .unwrap_or_else(|| "none".into())
}
fn object(value: &GraphObject) -> String {
    match value {
        GraphObject::Entity { record_ref } => record_ref.record_id.to_string(),
        GraphObject::Literal {
            literal_type,
            value,
        } => {
            format!("{} ({})", quoted(value), quoted(literal_type))
        }
    }
}
fn qualifiers(text: &mut String, value: &GraphQualifiers) {
    text.push_str(&format!(
        "Negated: {}\nModality: {}\nProperty: {}\nUnit: {}\nValid from: {}\nValid until: {}\n",
        value.negated,
        quoted(&value.modality),
        optional(&value.property),
        optional(&value.unit),
        optional(&value.valid_from),
        optional(&value.valid_until),
    ));
}
fn path(text: &mut String, step: &GraphPathStep, observed: &(Eligibility, Option<String>)) {
    let direction = match step.traversal {
        crate::graph::TraversalDirection::Outgoing => "outgoing",
        crate::graph::TraversalDirection::Incoming => "incoming",
    };
    text.push_str(&format!(
        "{}: {} {} {}; {} traversal; {}; status {}\n",
        step.assertion.record_id,
        step.subject.record_id,
        step.predicate,
        object(&step.object),
        direction,
        state(observed.0),
        debug_optional(&observed.1),
    ));
    qualifiers(text, &step.qualifiers);
}

pub(super) fn render(
    scope: ContextScope,
    passages: &[ContextPassage],
    bundles: &[EvidenceBundle],
    navigation: &[NavigationEdge],
    observed: &ObservedStates,
    canonical_bytes: usize,
) -> Result<String> {
    if observed.citations.len() != passages.len() || observed.paths.len() != bundles.len() {
        return Err(WikiError::invalid(
            "context presentation observations are incomplete",
        ));
    }
    let mut text = if scope == ContextScope::Snapshot {
        "Saved index; unverified.\n\n".to_owned()
    } else {
        String::new()
    };
    if passages.iter().any(|passage| !passage.citations.is_empty()) {
        text.push_str(
            "Keep the same --wiki and configuration options for these read commands.\n\n",
        );
    }
    for (i, passage) in passages.iter().enumerate() {
        let states = &observed.citations[i];
        if states.len() != passage.citations.len() {
            return Err(WikiError::invalid(
                "context citation presentation states are incomplete",
            ));
        }
        text.push_str(&format!(
            "{} {i} ({})\n",
            match passage.label {
                ExcerptLabel::CapturedSource => "Captured text",
                ExcerptLabel::NoteText => "Note text",
            },
            state(passage.eligibility),
        ));
        if passage.label == ExcerptLabel::NoteText || passage.citations.is_empty() {
            text.push_str(&format!("Note: {}\n", passage.locator.path));
        }
        let mut shown = BTreeSet::new();
        for citation in &passage.citations {
            let (source, revision) = pair(citation);
            if !shown.insert((source, revision)) {
                continue;
            }
            // Group each action with its supplied readable labels and references.
            // A missing optional label cannot remove the revision-pinned action.
            for link in passage
                .source_links
                .iter()
                .filter(|link| &link.source_id == source && &link.revision_id == revision)
            {
                text.push_str(&format!(
                    "Source: {} ({})\n",
                    link.markdown,
                    link.status.label()
                ));
            }
            for (reference, observed_state) in passage
                .citations
                .iter()
                .zip(states)
                .filter(|(reference, _)| pair(reference) == (source, revision))
            {
                match reference {
                    CitationRef::Source(_) => {
                        let linked = passage.source_links.iter().any(|link| {
                            &link.source_id == source
                                && &link.revision_id == revision
                                && matches!(
                                    (link.status, observed_state),
                                    (SourceReadLinkStatus::Current, CitationState::Current)
                                        | (
                                            SourceReadLinkStatus::Historical,
                                            CitationState::Historical
                                        )
                                        | (
                                            SourceReadLinkStatus::Withdrawn,
                                            CitationState::Withdrawn
                                        )
                                )
                        });
                        if !linked {
                            text.push_str(&format!(
                                "Source state: {}\n",
                                citation_state(*observed_state)
                            ));
                        }
                    }
                    CitationRef::Assertion(reference) => {
                        text.push_str(&format!(
                            "Evidence {} for {} ({})\n",
                            reference.evidence_id,
                            reference.assertion_id,
                            citation_state(*observed_state)
                        ));
                    }
                }
            }
            text.push_str(&format!(
                "Read: lwiki source read {source} --revision {revision} --max-bytes 4000\n"
            ));
        }
        // Retain every supplied optional link, including navigation without a cite.
        for link in &passage.source_links {
            if !shown.contains(&(&link.source_id, &link.revision_id)) {
                text.push_str(&format!(
                    "Source: {} ({})\n",
                    link.markdown,
                    link.status.label()
                ));
            }
        }
        for contributor in &passage.contributors {
            let reference = &contributor.reference;
            text.push_str(&format!(
                "Evidence {} for {} from {} / {}: {}; {}; status {}\n",
                reference.evidence_id,
                reference.assertion_id,
                reference.source_id,
                reference.source_revision,
                contributor.stance.as_str(),
                state(contributor.eligibility),
                optional(&contributor.authored_status),
            ));
        }
        // Never parse, sanitize, trim or deduplicate source text at presentation.
        text.push_str(&passage.text);
        text.push_str("\n\n");
    }
    for (bundle, states) in bundles.iter().zip(&observed.paths) {
        if states.len() != bundle.path.len() {
            return Err(WikiError::invalid(
                "context path presentation states are incomplete",
            ));
        }
        text.push_str(&format!(
            "Assertion {} ({}; status {}; disputed {})\n{} {} {}\n",
            bundle.assertion.record_id,
            state(bundle.eligibility),
            debug_optional(&bundle.authored_status),
            bundle.disputed,
            bundle.subject.record_id,
            bundle.predicate,
            object(&bundle.object),
        ));
        qualifiers(&mut text, &bundle.qualifiers);
        text.push_str("Path:\n");
        for (step, observed) in bundle.path.iter().zip(states) {
            path(&mut text, step, observed);
        }
        text.push_str(&format!(
            "Passages: {:?}; omitted support {}; omitted contradiction {}\n\n",
            bundle.passage_indices, bundle.omitted_support, bundle.omitted_contradictions,
        ));
    }
    for edge in navigation {
        text.push_str(&format!(
            "Navigation ({}; no asserted relationship)\nFrom: {} ({})\nTo: {} ({})\n\n",
            match edge.reason {
                NavigationReason::PageLink => "page link",
                NavigationReason::Provenance => "provenance",
            },
            edge.from.path,
            edge.from
                .record
                .as_ref()
                .expect("validated navigation")
                .record_id,
            edge.to.path,
            edge.to
                .record
                .as_ref()
                .expect("validated navigation")
                .record_id,
        ));
    }
    // The companion spends only canonical metadata savings. An invariant failure
    // must surface; never hide it by changing packing or deleting evidence.
    if text.len() > canonical_bytes {
        return Err(WikiError::invalid(
            "context presentation exceeds canonical rendered bound",
        ));
    }
    Ok(text)
}
