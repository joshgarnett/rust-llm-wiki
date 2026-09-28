//! Strict bounded source-local proposals; validation does not accept factual assertions.
use super::{extraction_types::*, packet::*};
use crate::{
    domain::*,
    sources::{SourceView, evidence::unique_quote_span},
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn proposition(
    assertion: &ExtractionAssertion,
    subject: &RecordId,
    object: Option<&RecordId>,
    id: &RecordId,
) -> Result<CanonicalRecord> {
    let mut fields = crate::sources::revision::common(
        id,
        RecordKind::Assertion,
        "Source-local assertion proposal",
    );
    for (key, value) in [
        ("wiki_status", "proposed"),
        ("wiki_subject_id", subject.as_str()),
        ("wiki_predicate", assertion.predicate.as_str()),
        ("wiki_modality", assertion.modality.as_str()),
    ] {
        fields.insert(key.into(), value.into());
    }
    fields.insert("wiki_negated".into(), assertion.negated.into());
    match &assertion.object {
        ExtractionObject::Mention { .. } => {
            fields.insert(
                "wiki_object_id".into(),
                object
                    .ok_or_else(|| invalid("missing resolved object"))?
                    .as_str()
                    .into(),
            );
        }
        ExtractionObject::Literal {
            literal_type,
            value,
        } => {
            string_bound(value, 12000, false)?;
            fields.insert("wiki_literal_type".into(), literal_type.as_str().into());
            fields.insert("wiki_literal_value".into(), value.as_str().into());
        }
    }
    for (key, value, max) in [
        ("wiki_valid_from", &assertion.valid_from, 10),
        ("wiki_valid_until", &assertion.valid_until, 10),
        ("wiki_property", &assertion.property, 1024),
        ("wiki_unit", &assertion.unit, 256),
    ] {
        if let Some(value) = value {
            string_bound(value, max, true)?;
            fields.insert(key.into(), value.as_str().into());
        }
    }
    CanonicalRecord::new(fields).map_err(|e| invalid(e.message))
}
fn trace(
    window: &PacketWindow,
    quote: &str,
    explicit: Option<ByteSpan>,
    content: &str,
) -> Result<TraceSpan> {
    string_bound(quote, MAX_WINDOW_BYTES, true)?;
    let span = if let Some(span) = explicit {
        if span.is_empty()
            || span.start() < window.span.start()
            || span.end() > window.span.end()
            || span.slice(content).map_err(|e| invalid(e.message))? != quote
        {
            return Err(invalid(
                "explicit quotation span differs from its window/source bytes",
            ));
        }
        span
    } else {
        unique_quote_span(content.as_bytes(), quote.as_bytes(), window.span)
            .map_err(|e| invalid(e.message))?
    };
    Ok(TraceSpan {
        window_id: window.id.clone(),
        span,
        quote_hash: Blake3Hash::digest(quote.as_bytes()),
    })
}
pub fn validate_response(
    packet: &VerifiedPacket,
    view: &SourceView<'_>,
    bytes: &[u8],
) -> Result<ValidatedExtraction> {
    limits(&packet.packet.limits)?;
    let response: ExtractionResponse = decode(bytes, packet.packet.limits.max_output_bytes)?;
    if response.schema != EXTRACTION_SCHEMA
        || response.packet_id != packet.packet.packet_id
        || response.packet_fingerprint != packet.packet.packet_fingerprint
    {
        return Err(invalid("response schema/packet identity differs"));
    }
    if response.mentions.len() > packet.packet.limits.max_mentions
        || response.assertions.len() > packet.packet.limits.max_assertions
        || response.unresolved.len() > MAX_UNRESOLVED
    {
        return Err(invalid("response item limits exceeded"));
    }
    let mut map = BTreeMap::new();
    let content = view.revision_content_bounded(
        &packet.packet.source_id,
        &packet.packet.source_revision,
        &mut map,
        SOURCE_CAP,
        SOURCE_CAP,
    )?;
    if Blake3Hash::digest(&content) != packet.packet.snapshot_hash {
        return Err(invalid("response snapshot hash differs"));
    }
    let text = std::str::from_utf8(&content).map_err(|_| invalid("snapshot is not UTF-8"))?;
    let mut observed = packet.dependencies.clone();
    observed.extend(dependencies(map));
    let map = dependency_map(&observed)?;
    let windows: BTreeMap<_, _> = packet.packet.windows.iter().map(|w| (&w.id, w)).collect();
    let mut ids = BTreeSet::new();
    let mut mention_spans = BTreeMap::new();
    for mention in &response.mentions {
        if !ids.insert(&mention.id) {
            return Err(invalid("duplicate response local ID"));
        }
        string_bound(&mention.label, 1024, true)?;
        entity_type(&mention.entity_type)?;
        if let Some(description) = &mention.description {
            string_bound(description, 4096, false)?;
        }
        let window = windows
            .get(&mention.window_id)
            .ok_or_else(|| invalid("mention references unknown window"))?;
        mention_spans.insert(
            mention.id.clone(),
            trace(window, &mention.quote, mention.span, text)?,
        );
    }
    let mentions: BTreeSet<_> = response.mentions.iter().map(|m| &m.id).collect();
    let mut evidence_spans = BTreeMap::new();
    let mut total = 0usize;
    for assertion in &response.assertions {
        if !ids.insert(&assertion.id) || !mentions.contains(&assertion.subject) {
            return Err(invalid("assertion duplicate ID or unknown subject mention"));
        }
        if let ExtractionObject::Mention { mention_id } = &assertion.object
            && !mentions.contains(mention_id)
        {
            return Err(invalid("assertion unknown object mention"));
        }
        // Reuse exactly the canonical predicate/literal/qualifier contract, without
        // persisting the synthetic endpoints or accepting the source proposition.
        let synthetic = RecordId::new("entity_internal_validation")?;
        proposition(
            assertion,
            &synthetic,
            Some(&synthetic),
            &RecordId::new("assertion_internal_validation")?,
        )?;
        if assertion.evidence.is_empty() || assertion.evidence.len() > MAX_EVIDENCE_PER_ASSERTION {
            return Err(invalid("assertion evidence item limits exceeded"));
        }
        total += assertion.evidence.len();
        if total > MAX_TOTAL_EVIDENCE {
            return Err(invalid("total evidence limit exceeded"));
        }
        let mut spans = vec![];
        for evidence in &assertion.evidence {
            let window = windows
                .get(&evidence.window_id)
                .ok_or_else(|| invalid("evidence references unknown window"))?;
            spans.push(trace(window, &evidence.quote, evidence.span, text)?);
        }
        evidence_spans.insert(assertion.id.clone(), spans);
    }
    for unresolved in &response.unresolved {
        string_bound(&unresolved.reason, 4096, true)?;
        string_bound(&unresolved.quote, MAX_WINDOW_BYTES, true)?;
        let window = windows
            .get(&unresolved.window_id)
            .ok_or_else(|| invalid("unresolved entry references unknown window"))?;
        // Unresolved items describe a coverage gap, not a uniquely located citation.
        if !window.text.contains(&unresolved.quote) {
            return Err(invalid("unresolved quote is absent from its source window"));
        }
    }
    Ok(ValidatedExtraction {
        packet: packet.clone(),
        response,
        raw_response: std::str::from_utf8(bytes)
            .map_err(|_| invalid("response is not UTF-8"))?
            .to_owned(),
        response_hash: Blake3Hash::digest(bytes),
        mention_spans,
        evidence_spans,
        dependencies: dependencies(map),
    })
}

pub(crate) fn immutable_artifact(
    value: &ExtractionArtifactV1,
    validated: &ValidatedExtraction,
) -> Result<()> {
    let packet = &validated.packet.packet;
    if value.schema != EXTRACTION_STATE_SCHEMA
        || value.packet_id != packet.packet_id
        || value.packet_fingerprint != packet.packet_fingerprint
        || value.source_id != packet.source_id
        || value.source_revision != packet.source_revision
        || value.snapshot_hash != packet.snapshot_hash
        || value.response_hash != validated.response_hash
        || value.raw_response != validated.raw_response
        || value.mention_spans != validated.mention_spans
        || value.evidence_spans != validated.evidence_spans
    {
        return Err(invalid(
            "extraction artifact raw response/proofs/packet identity mismatch",
        ));
    }
    artifact_membership(value, &validated.response)
}

/// Complete structural membership independent of source availability. Source
/// quotation/hash proof remains the sealed validated extraction constructor.
pub(crate) fn artifact_membership(
    value: &ExtractionArtifactV1,
    response: &ExtractionResponse,
) -> Result<()> {
    let mut locals = BTreeSet::new();
    if response.schema != EXTRACTION_SCHEMA
        || response.packet_id != value.packet_id
        || response.packet_fingerprint != value.packet_fingerprint
        || response.mentions.len() > MAX_MENTIONS
        || response.assertions.len() > MAX_ASSERTIONS
        || response.unresolved.len() > MAX_UNRESOLVED
        || response.mentions.iter().any(|m| !locals.insert(&m.id))
        || response.assertions.iter().any(|a| !locals.insert(&a.id))
        || value.response_hash != Blake3Hash::digest(value.raw_response.as_bytes())
    {
        return Err(invalid(
            "extraction response membership/identity is invalid",
        ));
    }
    let mentions: BTreeSet<_> = response.mentions.iter().map(|m| &m.id).collect();
    let assertions: BTreeSet<_> = response.assertions.iter().map(|a| &a.id).collect();
    if value.bindings.keys().collect::<BTreeSet<_>>() != mentions
        || value.mention_spans.keys().collect::<BTreeSet<_>>() != mentions
        || value.evidence_spans.keys().collect::<BTreeSet<_>>() != assertions
        || value.allocations.assertions.keys().collect::<BTreeSet<_>>() != assertions
        || value.allocations.evidence.keys().collect::<BTreeSet<_>>() != assertions
    {
        return Err(invalid(
            "extraction artifact mapping must completely cover response IDs",
        ));
    }
    let mut durable = BTreeSet::from([&value.extraction_id]);
    for assertion in &response.assertions {
        if !durable.insert(&value.allocations.assertions[&assertion.id])
            || value.allocations.evidence[&assertion.id].len() != assertion.evidence.len()
            || value.evidence_spans[&assertion.id].len() != assertion.evidence.len()
        {
            return Err(invalid("extraction assertion/evidence allocation mismatch"));
        }
        for evidence in &value.allocations.evidence[&assertion.id] {
            if !durable.insert(evidence) {
                return Err(invalid("duplicate reserved durable ID"));
            }
        }
    }
    let mut materialized = BTreeSet::new();
    for local in &value.materialized_assertions {
        if !assertions.contains(local) || !materialized.insert(local) {
            return Err(invalid("invalid materialized assertion membership"));
        }
    }
    Ok(())
}

pub(crate) fn array_has(record: &CanonicalRecord, key: &str, id: &str) -> bool {
    record
        .field(key)
        .and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(|v| v.as_str() == Some(id)))
}
