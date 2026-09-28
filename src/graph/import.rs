//! Guarded source-local extraction retention and validated Markdown restoration.
use super::{
    extraction_types::*,
    packet::*,
    wire::{self, array_has},
};
use crate::{
    changes::*,
    domain::*,
    records::{ParsedNote, parse_note},
    sources::{
        SourceView,
        revision::{common, record_bytes, timestamp},
    },
    vault::{ExpectedState, WriterPermit},
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const ARTIFACT_FENCE: &str = "lwiki-extraction-state-v1";
pub fn artifact_body(artifact: &ExtractionArtifactV1) -> Result<Vec<u8>> {
    let response: ExtractionResponse = decode(artifact.raw_response.as_bytes(), 262144)?;
    let mut body=format!("\nSource-local proposals from packet {}. All endpoint bindings require explicit decisions.\n\n",artifact.packet_id).into_bytes();
    for mention in &response.mentions {
        body.extend_from_slice(
            format!(
                "- Mention {}: {} ({})\n",
                mention.id,
                serde_json::to_string(&mention.label).map_err(|e| invalid(e.to_string()))?,
                mention.entity_type
            )
            .as_bytes(),
        );
    }
    for assertion in &response.assertions {
        body.extend_from_slice(
            format!(
                "- Proposed assertion {}: {} {} {}\n",
                assertion.id,
                assertion.subject,
                assertion.predicate,
                serde_json::to_string(&assertion.object).map_err(|e| invalid(e.to_string()))?
            )
            .as_bytes(),
        );
    }
    body.extend(render_fence(artifact, ARTIFACT_FENCE, MAX_ARTIFACT_BYTES)?);
    Ok(body)
}
fn artifact_note(artifact: &ExtractionArtifactV1) -> Result<Vec<u8>> {
    let mut fields = common(
        &artifact.extraction_id,
        RecordKind::Extraction,
        "Source-local extraction proposals",
    );
    for (key, value) in [
        (
            "wiki_status",
            if decode::<ExtractionResponse>(artifact.raw_response.as_bytes(), 262144)?
                .unresolved
                .is_empty()
            {
                "completed"
            } else {
                "partial"
            },
        ),
        ("wiki_packet_id", artifact.packet_id.as_str()),
        ("wiki_input_hash", artifact.packet_fingerprint.as_str()),
        ("wiki_executor", "agent"),
    ] {
        fields.insert(key.into(), value.into());
    }
    fields.insert(
        "wiki_extractor_fingerprint".into(),
        Blake3Hash::digest(
            b"lwiki-source-local-import-v1;strict-schema-v1;exact-utf8-spans;pending-mappings-v1",
        )
        .as_str()
        .into(),
    );
    fields.insert(
        "wiki_source_ids".into(),
        serde_json::json!([artifact.source_id]),
    );
    fields.insert(
        "wiki_source_revision_ids".into(),
        serde_json::json!([artifact.source_revision]),
    );
    fields.insert("wiki_completed_at".into(), timestamp()?.into());
    fields.insert(
        "wiki_packet".into(),
        format!(
            "[[knowledge/extractions/packets/{}.md]]",
            artifact.packet_id
        )
        .into(),
    );
    record_bytes(CanonicalRecord::new(fields)?, &artifact_body(artifact)?)
}
fn state_coverage(
    artifact: &ExtractionArtifactV1,
    packet: &ExtractionPacket,
    response: &ExtractionResponse,
    source_bytes: usize,
) -> ExtractionCoverage {
    let mut coverage = coverage(packet, source_bytes);
    coverage.unresolved = response.unresolved.len();
    coverage.pending_mentions = artifact
        .bindings
        .values()
        .filter(|b| matches!(b, MentionBinding::Pending))
        .count();
    coverage.rejected_mentions = artifact
        .bindings
        .values()
        .filter(|b| matches!(b, MentionBinding::Rejected { .. }))
        .count();
    coverage.materialized_assertions = artifact.materialized_assertions.len();
    coverage
}
fn resolve_guarded<'a>(
    view: &'a SourceView<'_>,
    id: &RecordId,
    kind: RecordKind,
    map: &mut BTreeMap<VaultRelativePath, ExpectedState>,
) -> Result<&'a CanonicalRecord> {
    let (path, note) = view.resolve(id, kind, None)?;
    SourceView::note_dependency(path, note, map);
    Ok(note.canonical.as_ref().expect("resolved canonical record"))
}
fn validate_bindings(
    view: &SourceView<'_>,
    artifact: &ExtractionArtifactV1,
    response: &ExtractionResponse,
    map: &mut BTreeMap<VaultRelativePath, ExpectedState>,
) -> Result<()> {
    for (local, binding) in &artifact.bindings {
        let (decision_id, entity) = match binding {
            MentionBinding::Pending => continue,
            MentionBinding::Resolved {
                entity_id,
                decision_id,
            } => (decision_id, Some(entity_id)),
            MentionBinding::Rejected { decision_id } => (decision_id, None),
        };
        let decision = resolve_guarded(view, decision_id, RecordKind::Decision, map)?;
        if decision.string("wiki_status") != Some("active")
            || decision.string("wiki_extraction_id") != Some(artifact.extraction_id.as_str())
            || !array_has(decision, "wiki_mention_ids", local.as_str())
        {
            return Err(invalid(
                "binding lacks an active explicit extraction/mention decision",
            ));
        }
        if let Some(entity_id) = entity {
            resolve_guarded(view, entity_id, RecordKind::Entity, map)?;
            if !matches!(
                decision.string("wiki_action"),
                Some("bind_mention" | "create_entity")
            ) || !array_has(decision, "wiki_output_ids", entity_id.as_str())
            {
                return Err(invalid(
                    "resolved binding disagrees with explicit entity decision",
                ));
            }
        } else if decision.string("wiki_action") != Some("reject_mention") {
            return Err(invalid("rejected binding disagrees with explicit decision"));
        }
    }
    let materialized: BTreeSet<_> = artifact.materialized_assertions.iter().collect();
    for assertion in &response.assertions {
        let allocated = &artifact.allocations.assertions[&assertion.id];
        if !materialized.contains(&assertion.id) {
            for reserved in std::iter::once(allocated)
                .chain(artifact.allocations.evidence[&assertion.id].iter())
            {
                if view.notes.values().any(|note| {
                    note.fields
                        .as_ref()
                        .and_then(|f| f.get("wiki_id"))
                        .and_then(Value::as_str)
                        == Some(reserved.as_str())
                }) {
                    return Err(invalid(
                        "unmaterialized reserved ID already has a canonical envelope",
                    ));
                }
            }
            continue;
        }
        let bound = |local: &PacketLocalId| -> Result<&RecordId> {
            match artifact.bindings.get(local) {
                Some(MentionBinding::Resolved { entity_id, .. }) => Ok(entity_id),
                _ => Err(invalid(
                    "materialized assertion has unresolved/rejected endpoint",
                )),
            }
        };
        let subject = bound(&assertion.subject)?;
        let object = match &assertion.object {
            ExtractionObject::Mention { mention_id } => Some(bound(mention_id)?),
            ExtractionObject::Literal { .. } => None,
        };
        let expected = wire::proposition(assertion, subject, object, allocated)?;
        let actual = resolve_guarded(view, allocated, RecordKind::Assertion, map)?;
        for key in [
            "wiki_subject_id",
            "wiki_predicate",
            "wiki_object_id",
            "wiki_literal_type",
            "wiki_literal_value",
            "wiki_property",
            "wiki_unit",
            "wiki_valid_from",
            "wiki_valid_until",
        ] {
            if actual.field(key) != expected.field(key) {
                return Err(invalid(
                    "materialized assertion disagrees with response/bindings",
                ));
            }
        }
        if actual
            .field("wiki_negated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            != assertion.negated
            || actual.string("wiki_modality").unwrap_or("asserted") != assertion.modality
        {
            return Err(invalid("materialized assertion qualifier mismatch"));
        }
        for (i, wire_evidence) in assertion.evidence.iter().enumerate() {
            let evidence_id = &artifact.allocations.evidence[&assertion.id][i];
            let evidence = resolve_guarded(view, evidence_id, RecordKind::Evidence, map)?;
            let reference = crate::sources::evidence::evidence_reference(evidence)?;
            let span = &artifact.evidence_spans[&assertion.id][i];
            if reference.assertion_id != *allocated
                || reference.source_id != artifact.source_id
                || reference.source_revision != artifact.source_revision
                || reference.span != span.span
                || reference.quote_hash != span.quote_hash
                || evidence.string("wiki_stance") != Some(wire_evidence.stance.as_str())
                || evidence.string("wiki_extraction_id") != Some(artifact.extraction_id.as_str())
            {
                return Err(invalid(
                    "reserved evidence disagrees with immutable source trace",
                ));
            }
            // Original evidence may now be retracted or historical after explicit
            // review/revalidation; verify its original bytes, not current eligibility.
            view.resolve(
                &reference.source_id,
                RecordKind::Source,
                evidence.string("wiki_source"),
            )?;
            view.resolve(
                &reference.source_revision,
                RecordKind::Revision,
                evidence.string("wiki_revision"),
            )?;
            let (path, note) = view.resolve(
                &reference.assertion_id,
                RecordKind::Assertion,
                evidence.string("wiki_assertion"),
            )?;
            SourceView::note_dependency(path, note, map);
            let (path, note) = view.resolve(evidence_id, RecordKind::Evidence, None)?;
            SourceView::note_dependency(path, note, map);
            if crate::sources::evidence::note_quote(note)? != wire_evidence.quote.as_bytes() {
                return Err(invalid("evidence quotation changed"));
            }
        }
    }
    Ok(())
}
fn artifact_envelope(note: &ParsedNote, artifact: &ExtractionArtifactV1) -> Result<()> {
    let record = note
        .canonical
        .as_ref()
        .filter(|record| record.kind() == RecordKind::Extraction)
        .ok_or_else(|| invalid("invalid extraction note envelope"))?;
    if record.id() != &artifact.extraction_id
        || record.string("wiki_packet_id") != Some(artifact.packet_id.as_str())
        || record.string("wiki_input_hash") != Some(artifact.packet_fingerprint.as_str())
        || record.field("wiki_source_ids") != Some(&serde_json::json!([artifact.source_id]))
        || record.field("wiki_source_revision_ids")
            != Some(&serde_json::json!([artifact.source_revision]))
    {
        return Err(invalid("extraction envelope/artifact mismatch"));
    }
    Ok(())
}
fn load_note(
    view: &SourceView<'_>,
    path: &VaultRelativePath,
    note: &ParsedNote,
) -> Result<VerifiedExtractionArtifact> {
    let artifact: ExtractionArtifactV1 = decode(
        fenced_json(note, ARTIFACT_FENCE, MAX_ARTIFACT_BYTES)?,
        MAX_ARTIFACT_BYTES,
    )?;
    artifact_envelope(note, &artifact)?;
    let packet = load_packet(view, &artifact.packet_id)?;
    let validated = wire::validate_response(&packet, view, artifact.raw_response.as_bytes())?;
    wire::immutable_artifact(&artifact, &validated)?;
    let mut map = dependency_map(&validated.dependencies)?;
    SourceView::note_dependency(path, note, &mut map);
    validate_bindings(view, &artifact, &validated.response, &mut map)?;
    Ok(VerifiedExtractionArtifact {
        artifact,
        response: validated.response,
        packet,
        locator: locator(view, path, note)?,
        dependencies: dependencies(map),
    })
}
pub fn load_extraction(view: &SourceView<'_>, id: &RecordId) -> Result<VerifiedExtractionArtifact> {
    let (path, note) = view.resolve(id, RecordKind::Extraction, None)?;
    load_note(view, path, note)
}
/// Verify a proposed parsed note against the complete caller-supplied projection.
pub(crate) fn verify_extraction_note(
    view: &SourceView<'_>,
    path: &VaultRelativePath,
    note: &ParsedNote,
) -> Result<VerifiedExtractionArtifact> {
    load_note(view, path, note)
}
/// Resolution may change Pending bindings and extend materialization only.
pub(crate) fn verify_resolution_transition(
    before: &ExtractionArtifactV1,
    after: &ExtractionArtifactV1,
) -> Result<()> {
    let mut normalized = after.clone();
    normalized.bindings = before.bindings.clone();
    normalized.materialized_assertions = before.materialized_assertions.clone();
    if &normalized != before || before.bindings.keys().ne(after.bindings.keys()) {
        return Err(invalid(
            "resolution changed immutable extraction fields or map keys",
        ));
    }
    for (mention, old) in &before.bindings {
        let next = &after.bindings[mention];
        if old != next
            && (!matches!(old, MentionBinding::Pending) || matches!(next, MentionBinding::Pending))
        {
            return Err(invalid("resolution cannot overwrite a decided mention"));
        }
    }
    let old: BTreeSet<_> = before.materialized_assertions.iter().collect();
    let new: BTreeSet<_> = after.materialized_assertions.iter().collect();
    if old.len() != before.materialized_assertions.len()
        || new.len() != after.materialized_assertions.len()
        || !old.is_subset(&new)
        || new
            .iter()
            .any(|id| !after.allocations.assertions.contains_key(*id))
    {
        return Err(invalid("resolution materialization set is invalid"));
    }
    Ok(())
}
/// Replace only the existing artifact JSON, preserving surrounding author prose.
pub(crate) fn edit_extraction_artifact(
    note: &ParsedNote,
    artifact: &ExtractionArtifactV1,
) -> Result<Vec<u8>> {
    let old = fenced_json(note, ARTIFACT_FENCE, MAX_ARTIFACT_BYTES)?;
    let start = old.as_ptr() as usize - note.body().as_ptr() as usize;
    let end = start + old.len();
    let replacement = canonical_json(artifact)?;
    if replacement.len() > MAX_ARTIFACT_BYTES {
        return Err(invalid("extraction artifact exceeds byte ceiling"));
    }
    let mut body = Vec::with_capacity(note.body().len() - old.len() + replacement.len() + 1);
    body.extend_from_slice(&note.body()[..start]);
    body.extend_from_slice(&replacement);
    body.extend_from_slice(&note.body()[end..]);
    crate::records::edit_note(note, &BTreeMap::new(), Some(&body), &note.source_hash)
}
fn retained_note(
    engine: &ChangeEngine,
    change: &ChangeInspection,
) -> Result<(VaultRelativePath, ParsedNote, ExtractionArtifactV1)> {
    let id = change
        .manifest
        .allocated_ids
        .get("extraction")
        .ok_or_else(|| invalid("retained graph import lacks extraction allocation"))?;
    let target = VaultRelativePath::new(format!("knowledge/extractions/{id}.md"))?;
    let (index, op) = change
        .manifest
        .operations
        .iter()
        .enumerate()
        .find(|(_, op)| op.target == target)
        .ok_or_else(|| invalid("retained graph import lacks extraction payload"))?;
    let bytes = engine
        .verify_payload(
            &change.manifest.change_id,
            index,
            "proposed",
            &target,
            &op.after,
            &op.after_payload,
        )?
        .ok_or_else(|| invalid("retained extraction payload is absent"))?;
    let note = parse_note(&bytes);
    let artifact: ExtractionArtifactV1 = decode(
        fenced_json(&note, ARTIFACT_FENCE, MAX_ARTIFACT_BYTES)?,
        MAX_ARTIFACT_BYTES,
    )?;
    artifact_envelope(&note, &artifact)?;
    let mut expected = BTreeMap::from([("extraction".into(), artifact.extraction_id.clone())]);
    for (local, id) in &artifact.allocations.assertions {
        expected.insert(format!("assertion:{local}"), id.clone());
    }
    for (local, ids) in &artifact.allocations.evidence {
        for (i, id) in ids.iter().enumerate() {
            expected.insert(format!("evidence:{local}:{i}"), id.clone());
        }
    }
    if change.manifest.allocated_ids != expected || artifact.extraction_id != *id {
        return Err(invalid("retained manifest and artifact allocations differ"));
    }
    Ok((target, note, artifact))
}
fn from_retained(
    engine: &ChangeEngine,
    view: &SourceView<'_>,
    change: &ChangeInspection,
) -> Result<VerifiedExtractionArtifact> {
    let (target, note, _) = retained_note(engine, change)?;
    load_note(view, &target, &note)
}
pub fn stage_import(
    engine: &ChangeEngine,
    writer: &WriterPermit,
    validated: &ValidatedExtraction,
    policy: OriginPolicy,
) -> Result<ImportOutcome> {
    writer.require_root(engine.fs().root())?;
    // Guard original authorization before allocating IDs or returning reuse.
    let guard = ChangeDraft {
        title: "Verify extraction import dependencies".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: validated.dependencies.clone(),
        operations: vec![],
    };
    engine.plan(&guard)?;
    let view = SourceView::from_fs_bounded(engine.fs(), SOURCE_CAP, 4096)?;
    let mut matching = None;
    let mut differing = false;
    for (path, note) in &view.notes {
        if note
            .fields
            .as_ref()
            .and_then(|f| f.get("wiki_kind"))
            .and_then(Value::as_str)
            != Some("extraction")
        {
            continue;
        }
        let extracted = load_note(&view, path, note)?;
        if extracted.artifact.packet_id == validated.packet.packet.packet_id {
            if extracted.artifact.response_hash == validated.response_hash {
                if matching.replace(extracted).is_some() {
                    return Err(invalid("duplicate canonical extraction origins"));
                }
            } else {
                differing = true;
            }
        }
    }
    if let Some(extracted) = matching {
        // Canonical Markdown restoration does not invent a missing changes manifest.
        // Preserve all explicit resolution/review state already present in this note.
        let content = view.revision_content_bounded(
            &extracted.artifact.source_id,
            &extracted.artifact.source_revision,
            &mut BTreeMap::new(),
            SOURCE_CAP,
            SOURCE_CAP,
        )?;
        let mut retained = None;
        for id in engine.change_ids()? {
            let inspection = engine.inspect(&id)?;
            if inspection.manifest.origin.as_ref().is_some_and(|o| {
                o.operation == OriginOperation::GraphImport
                    && o.packet_id == extracted.artifact.packet_id
                    && o.response_hash == extracted.artifact.response_hash
            }) {
                let (_, _, mut initial) = retained_note(engine, &inspection)?;
                wire::immutable_artifact(&initial, validated)?;
                initial.bindings = extracted.artifact.bindings.clone();
                initial.materialized_assertions =
                    extracted.artifact.materialized_assertions.clone();
                if initial != extracted.artifact {
                    return Err(invalid(
                        "canonical and retained import immutable identities/allocations differ",
                    ));
                }
                if retained.replace(inspection).is_some() {
                    return Err(invalid("duplicate retained import origins"));
                }
            }
        }
        let coverage = state_coverage(
            &extracted.artifact,
            &extracted.packet.packet,
            &extracted.response,
            content.len(),
        );
        return Ok(ImportOutcome {
            extraction: extracted.locator,
            prepared: retained.as_ref().map(|c| c.prepared.clone()),
            status: retained.as_ref().map(|c| c.status),
            disposition: if retained.is_some() {
                ImportDisposition::RetainedChange
            } else {
                ImportDisposition::CanonicalRestored
            },
            allocations: extracted.artifact.allocations,
            coverage,
            reused: true,
        });
    }
    if differing && policy == OriginPolicy::ReuseOrConflict {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "different canonical response exists for packet",
        ));
    }
    let origin = ChangeOrigin {
        operation: OriginOperation::GraphImport,
        packet_id: validated.packet.packet.packet_id.clone(),
        response_hash: validated.response_hash.clone(),
    };
    let outcome = engine.prepare_or_reuse(writer, origin, policy, || {
        let extraction_id = RecordId::generate(RecordKind::Extraction)?;
        let mut ids = BTreeMap::from([("extraction".into(), extraction_id.clone())]);
        let mut assertions = BTreeMap::new();
        let mut evidence = BTreeMap::new();
        for assertion in &validated.response.assertions {
            let id = RecordId::generate(RecordKind::Assertion)?;
            ids.insert(format!("assertion:{}", assertion.id), id.clone());
            assertions.insert(assertion.id.clone(), id);
            let mut allocated = vec![];
            for i in 0..assertion.evidence.len() {
                let id = RecordId::generate(RecordKind::Evidence)?;
                ids.insert(format!("evidence:{}:{i}", assertion.id), id.clone());
                allocated.push(id);
            }
            evidence.insert(assertion.id.clone(), allocated);
        }
        let packet = &validated.packet.packet;
        let artifact = ExtractionArtifactV1 {
            schema: EXTRACTION_STATE_SCHEMA.into(),
            extraction_id: extraction_id.clone(),
            packet_id: packet.packet_id.clone(),
            packet_fingerprint: packet.packet_fingerprint.clone(),
            source_id: packet.source_id.clone(),
            source_revision: packet.source_revision.clone(),
            snapshot_hash: packet.snapshot_hash.clone(),
            response_hash: validated.response_hash.clone(),
            raw_response: validated.raw_response.clone(),
            mention_spans: validated.mention_spans.clone(),
            evidence_spans: validated.evidence_spans.clone(),
            allocations: ExtractionAllocations {
                assertions,
                evidence,
            },
            bindings: validated
                .response
                .mentions
                .iter()
                .map(|m| (m.id.clone(), MentionBinding::Pending))
                .collect(),
            materialized_assertions: vec![],
        };
        wire::immutable_artifact(&artifact, validated)?;
        let bytes = artifact_note(&artifact)?;
        Ok(ChangeDraft {
            title: "Retain source-local extraction proposals".into(),
            origin: None,
            inverse_of: None,
            allocated_ids: ids,
            read_preconditions: validated.dependencies.clone(),
            operations: vec![ExpectedWrite {
                target: VaultRelativePath::new(format!(
                    "knowledge/extractions/{extraction_id}.md"
                ))?,
                expected: ExpectedState::Absent,
                proposed: Some(bytes),
                apply_after: vec![],
            }],
        })
    })?;
    let extracted = from_retained(engine, &view, &outcome.change)?;
    let content = view.revision_content_bounded(
        &extracted.artifact.source_id,
        &extracted.artifact.source_revision,
        &mut BTreeMap::new(),
        SOURCE_CAP,
        SOURCE_CAP,
    )?;
    Ok(ImportOutcome {
        extraction: extracted.locator,
        prepared: Some(outcome.change.prepared),
        status: Some(outcome.change.status),
        disposition: ImportDisposition::RetainedChange,
        allocations: extracted.artifact.allocations.clone(),
        coverage: state_coverage(
            &extracted.artifact,
            &extracted.packet.packet,
            &extracted.response,
            content.len(),
        ),
        reused: outcome.reused,
    })
}
