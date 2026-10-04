//! Authored status is preserved; only derived eligibility is computed here.
use super::{
    eligibility_facts::{EligibilityRole, NormalizedEligibilityFacts},
    eligibility_rules::{self as rules, GenerationContext},
    scan::{
        diagnostic, input_notes, isolated_fields, list, project_validation,
        project_validation_closed, readable_id,
    },
    structural_rules::{self, StructuralRecorder, StructuralStage, invalidate as mark_invalid},
    types::*,
};
use crate::{
    changes::{
        GraphValidator, ReadDependency, RetainedGraphInput, RetainedGraphInverseInput,
        ValidatedGraph, ValidationInput,
    },
    domain::{
        Blake3Hash, CanonicalRecord, CitationRef, Eligibility, ErrorCode, RecordId, RecordKind,
        Result, VaultRelativePath, WikiError,
    },
    records::{LinkResolution, ParsedNote, RegistryEntry, links::IndexedRegistry},
    sources::{CitationScope, SourceView, evidence::evidence_reference, revision::canonical_path},
    vault::VaultFs,
};
use std::collections::{BTreeMap, BTreeSet};

/// Baseline invalid/plain notes may stay readable. Any newly invalid typed record,
/// changed invalid adopted envelope, duplicate, or newly broken typed reference
/// rejects activation. Stale/unsupported descendants of valid lifecycle changes
/// are permitted: those are derived states, not structural corruption.
impl GraphValidator for CatalogGraphValidator {
    fn validate(&self, fs: &VaultFs, input: &ValidationInput) -> Result<ValidatedGraph> {
        validate_projection(fs, input, false, None, None)
    }
    fn validate_retained(
        &self,
        fs: &VaultFs,
        input: &ValidationInput,
        retained: &RetainedGraphInput,
    ) -> Result<ValidatedGraph> {
        validate_projection(fs, input, false, Some(retained), None)
    }
    fn validate_inverse(
        &self,
        fs: &VaultFs,
        input: &ValidationInput,
        inverse: &RetainedGraphInverseInput,
    ) -> Result<ValidatedGraph> {
        validate_projection(fs, input, false, None, Some(inverse))
    }
}
impl CatalogGraphValidator {
    /// Preparation-only validation of a bounded closed captured overlay.
    /// Unprovided assets cannot supply current support. Synthetic missing closed
    /// dependencies are not physical read guards; apply retains full reproof.
    pub fn validate_closed(&self, fs: &VaultFs, input: &ValidationInput) -> Result<ValidatedGraph> {
        validate_projection(fs, input, true, None, None)
    }
}
fn validate_projection(
    fs: &VaultFs,
    input: &ValidationInput,
    closed: bool,
    retained: Option<&RetainedGraphInput>,
    inverse_witness: Option<&RetainedGraphInverseInput>,
) -> Result<ValidatedGraph> {
    let review_inverse = inverse_witness
        .filter(|w| w.anchor().origin().operation == crate::changes::OriginOperation::GraphReview)
        .map(|w| crate::graph::review::verify_review_inverse_overlay(fs, input, w))
        .transpose()?;
    let inverse = inverse_witness
        .filter(|w| w.anchor().origin().operation == crate::changes::OriginOperation::GraphDecide)
        .map(|w| crate::graph::inverse::verify_inverse_overlay(fs, input, w))
        .transpose()?;
    let is_inverse = inverse_witness.is_some();
    let remap = if !is_inverse {
        crate::graph::remap::verify_remap_overlay(
            fs,
            input,
            retained
                .filter(|w| w.origin().operation == crate::changes::OriginOperation::GraphDecide),
        )?
    } else {
        None
    };
    let review = if !is_inverse {
        crate::graph::review::verify_review_overlay(
            fs,
            input,
            retained
                .filter(|w| w.origin().operation == crate::changes::OriginOperation::GraphReview),
        )?
    } else {
        None
    };
    if !closed
        && review.is_some()
        && retained
            .is_none_or(|w| w.origin().operation != crate::changes::OriginOperation::GraphReview)
    {
        return Err(WikiError::invalid(
            "review activation requires authenticated GraphReview origin and allocations",
        ));
    }
    if !closed
        && remap.as_ref().is_some_and(|p| p.requires_retained_input())
        && retained
            .is_none_or(|w| w.origin().operation != crate::changes::OriginOperation::GraphDecide)
    {
        return Err(WikiError::invalid(
            "entity decision activation requires authenticated GraphDecide origin and allocations",
        ));
    }
    let projector = if closed {
        project_validation_closed
    } else {
        project_validation
    };
    let proposed = projector(fs, input)?;
    let baseline = projector(
        fs,
        &ValidationInput {
            vault_id: input.vault_id.clone(),
            documents: input.documents.clone(),
            overlay: vec![],
        },
    )?;
    let notes = input_notes(input)?;
    let before = input_notes(&ValidationInput {
        vault_id: input.vault_id.clone(),
        documents: input.documents.clone(),
        overlay: vec![],
    })?;
    for target in &input.overlay {
        if !canonical_path(&target.path) {
            continue;
        }
        if let Some(note) = notes.get(&target.path) {
            let changed = before
                .get(&target.path)
                .is_none_or(|old| old.raw != note.raw);
            let adopted = adopted_marker(note)
                || note
                    .fields
                    .as_ref()
                    .is_some_and(|fields| fields.keys().any(|key| key.starts_with("wiki_")));
            if changed && adopted && note.canonical.is_none() {
                return Err(WikiError::invalid(format!(
                    "proposed adopted envelope is invalid: {}",
                    target.path
                )));
            }
            if changed
                && let Some(id) = readable_id(note)
                && proposed
                    .records
                    .get(&id)
                    .is_none_or(|row| row.eligibility == Eligibility::Invalid)
            {
                return Err(WikiError::invalid(format!(
                    "proposed typed record is invalid: {}",
                    target.path
                )));
            }
        }
    }
    for (id, row) in &proposed.records {
        if row.record.kind() == RecordKind::Assertion {
            if let Some(old) = baseline.records.get(id)
                && proposition(&old.record) != proposition(&row.record)
                && !remap
                    .as_ref()
                    .is_some_and(|proof| proof.authorized_assertions().contains(id))
                && !inverse
                    .as_ref()
                    .is_some_and(|proof| proof.authorized_assertions().contains(id))
            {
                return Err(WikiError::invalid(format!(
                    "assertion identity cannot be silently retargeted: {id}"
                )));
            }
            if row.record.string("wiki_status") == Some("accepted")
                && (baseline
                    .records
                    .get(id)
                    .is_none_or(|old| old.record.string("wiki_status") != Some("accepted"))
                    || inverse
                        .as_ref()
                        .is_some_and(|proof| proof.restored_accepted().contains(id))
                    || review
                        .as_ref()
                        .is_some_and(|proof| proof.accepted_assertions().contains(id))
                    || review_inverse
                        .as_ref()
                        .is_some_and(|proof| proof.restored_accepted().contains(id)))
                && row.eligibility != Eligibility::Current
            {
                return Err(WikiError::invalid(format!(
                    "acceptance requires intact current supporting evidence: {id}"
                )));
            }
        }
        if row.eligibility == Eligibility::Invalid
            && baseline.records.get(id).is_none_or(|old| {
                old.eligibility != Eligibility::Invalid || old.reasons != row.reasons
            })
        {
            return Err(WikiError::invalid(format!(
                "proposal invalidates typed record {id}: {}",
                row.reasons.join(",")
            )));
        }
    }
    if let Some(proof) = &inverse {
        for id in proof.restored_accepted() {
            if proposed.records.get(id).is_none_or(|row| {
                row.record.string("wiki_status") != Some("accepted")
                    || row.eligibility != Eligibility::Current
            }) {
                return Err(WikiError::invalid(format!(
                    "inverse acceptance requires intact current supporting evidence: {id}"
                )));
            }
        }
    }
    for ids in [
        review.as_ref().map(|proof| proof.accepted_assertions()),
        review_inverse
            .as_ref()
            .map(|proof| proof.restored_accepted()),
    ]
    .into_iter()
    .flatten()
    {
        for id in ids {
            if proposed.records.get(id).is_none_or(|row| {
                row.record.string("wiki_status") != Some("accepted")
                    || row.eligibility != Eligibility::Current
            }) {
                return Err(WikiError::invalid(format!(
                    "review acceptance requires intact current supporting evidence: {id}"
                )));
            }
        }
    }
    for diagnostic in &proposed.diagnostics {
        if diagnostic.code == ErrorCode::ReferenceAmbiguous
            && !baseline.diagnostics.contains(diagnostic)
        {
            return Err(WikiError::new(
                ErrorCode::ReferenceAmbiguous,
                format!(
                    "proposal introduces ambiguous identity at {}",
                    diagnostic.path
                ),
            ));
        }
    }
    Ok(ValidatedGraph {
        parser_fingerprint: proposed.parser_fingerprint,
        control_manifest: proposed.control_manifest,
        dependencies: proposed.dependencies,
    })
}

fn adopted_marker(note: &ParsedNote) -> bool {
    readable_id(note).is_some()
        || isolated_fields(note)
            .iter()
            .any(|fields| fields.keys().any(|key| key.starts_with("wiki_")))
}

fn proposition(record: &CanonicalRecord) -> BTreeMap<String, serde_json::Value> {
    let mut fields = BTreeMap::new();
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
        if let Some(value) = record.field(key) {
            fields.insert(key.into(), value.clone());
        }
    }
    fields.insert(
        "wiki_negated".into(),
        record
            .field("wiki_negated")
            .cloned()
            .unwrap_or(false.into()),
    );
    fields.insert(
        "wiki_modality".into(),
        record
            .field("wiki_modality")
            .cloned()
            .unwrap_or("asserted".into()),
    );
    fields
}

pub(crate) use super::eligibility_rules::{OppositionKey, eligible_opposition, opposition_key};

pub(crate) fn references(
    record: &CanonicalRecord,
) -> Vec<(&'static str, RecordKind, Option<&'static str>)> {
    use RecordKind::*;
    match record.kind() {
        Entity => vec![("wiki_superseded_by_id", Entity, Some("wiki_superseded_by"))],
        Assertion => vec![
            ("wiki_subject_id", Entity, Some("wiki_subject")),
            ("wiki_object_id", Entity, Some("wiki_object")),
        ],
        Evidence => vec![
            ("wiki_assertion_id", Assertion, Some("wiki_assertion")),
            ("wiki_source_id", Source, Some("wiki_source")),
            ("wiki_source_revision", Revision, Some("wiki_revision")),
            ("wiki_supersedes_id", Evidence, Some("wiki_supersedes")),
            ("wiki_extraction_id", Extraction, Some("wiki_extraction")),
        ],
        Source => vec![("wiki_current_revision", Revision, Some("wiki_revision"))],
        Revision => vec![("wiki_source_id", Source, Some("wiki_source"))],
        ExtractionPacket => vec![
            ("wiki_source_id", Source, Some("wiki_source")),
            ("wiki_source_revision", Revision, Some("wiki_revision")),
        ],
        Extraction => vec![("wiki_packet_id", ExtractionPacket, Some("wiki_packet"))],
        Decision => vec![
            ("wiki_supersedes_id", Decision, Some("wiki_supersedes")),
            ("wiki_extraction_id", Extraction, Some("wiki_extraction")),
        ],
        Run => vec![(
            "wiki_checkpoint_event_id",
            RunEvent,
            Some("wiki_checkpoint_event"),
        )],
        RunEvent => vec![("wiki_run_id", Run, Some("wiki_run"))],
        _ => vec![],
    }
}

pub(crate) fn compute(
    view: &SourceView<'_>,
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    records: &mut BTreeMap<RecordId, RecordRow>,
    diagnostics: &mut Vec<CatalogDiagnostic>,
) -> Result<()> {
    compute_inner(view, notes, records, diagnostics, None)
}

/// Full normalized rebuild entry point. The input remains a complete graph;
/// only proof representation differs from legacy compute.
pub(crate) fn compute_normalized(
    view: &SourceView<'_>,
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    records: &mut BTreeMap<RecordId, RecordRow>,
    diagnostics: &mut Vec<CatalogDiagnostic>,
) -> Result<NormalizedEligibilityFacts> {
    let mut facts = NormalizedEligibilityFacts::new();
    compute_inner(view, notes, records, diagnostics, Some(&mut facts))?;
    Ok(facts)
}

fn compute_inner(
    view: &SourceView<'_>,
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    records: &mut BTreeMap<RecordId, RecordRow>,
    diagnostics: &mut Vec<CatalogDiagnostic>,
    mut facts: Option<&mut NormalizedEligibilityFacts>,
) -> Result<()> {
    let mut recorder = StructuralRecorder::new(facts.is_some());
    let registry = IndexedRegistry::new(
        records
            .values()
            .map(|r| RegistryEntry {
                id: r.record.id().clone(),
                kind: r.record.kind(),
                path: r.path.clone(),
                aliases: list(&r.record, "aliases"),
            })
            .collect(),
    );
    let snapshot = records.clone();
    let mut edges: BTreeMap<RecordId, BTreeSet<RecordId>> = BTreeMap::new();
    let mut declared: BTreeMap<RecordId, BTreeSet<RecordId>> = BTreeMap::new();
    let mut supersession: BTreeMap<RecordId, BTreeSet<RecordId>> = BTreeMap::new();
    for (id, row) in records.iter_mut() {
        let record = row.record.clone();
        if record.kind() == RecordKind::Assertion {
            for link in list(&record, "wiki_evidence") {
                let resolution = registry.resolve_untyped(&link);
                let target = match &resolution {
                    LinkResolution::Resolved { id, .. } => snapshot.get(id).map(|row| &row.record),
                    _ => None,
                };
                let reason =
                    super::eligibility_rules::evidence_navigation_reason(id, &resolution, target);
                if let Some(reason) = reason {
                    diagnostics.push(diagnostic(
                        &row.path,
                        Some(id),
                        ErrorCode::RecordInvalid,
                        serde_json::json!({"reason":reason,"link":link}),
                    ));
                }
            }
        }
        let result = structural_rules::evaluate_references(
            row,
            &registry,
            &structural_rules::ReferenceBoundary::complete(&snapshot),
            diagnostics,
            &mut recorder,
        )?;
        edges.insert(id.clone(), result.targets);
        if !result.declared.is_empty() {
            declared.insert(id.clone(), result.declared);
        }
        if !result.supersession.is_empty() {
            supersession.insert(id.clone(), result.supersession);
        }
        if let Some(facts) = facts.as_deref_mut() {
            facts.edges.extend(result.semantic);
        }
    }
    recorder.phase(StructuralStage::Invariant);
    let decision_policy = match crate::graph::remap::verify_decision_policy(notes) {
        Ok(policy) => policy,
        Err(error) => {
            for id in crate::graph::remap::relevant_decision_ids(notes) {
                if let Some(row) = records.get_mut(&id) {
                    mark_invalid(
                        row,
                        "entity_decision_receipt_invalid",
                        diagnostics,
                        serde_json::json!({"error":error}),
                        &mut recorder,
                    );
                }
            }
            None
        }
    };
    let review_policy = match crate::graph::review::verify_review_policy(notes) {
        Ok(policy) => policy,
        Err(error) => {
            for id in crate::graph::review::relevant_decision_ids(notes) {
                if let Some(row) = records.get_mut(&id) {
                    mark_invalid(
                        row,
                        "review_receipt_invalid",
                        diagnostics,
                        serde_json::json!({"error":error}),
                        &mut recorder,
                    );
                }
            }
            None
        }
    };
    let review_edges = review_policy
        .as_ref()
        .map(|p| p.supersession_edges().clone())
        .unwrap_or_default();
    for (predecessor, successor) in &review_edges {
        if let Some(facts) = facts.as_deref_mut() {
            facts.edge(successor, predecessor, EligibilityRole::PolicySupersession);
        }
        supersession
            .entry(successor.clone())
            .or_default()
            .insert(predecessor.clone());
    }
    if let Some(policy) = &decision_policy {
        for (predecessor, successor) in policy.supersession_edges() {
            if let Some(facts) = facts.as_deref_mut() {
                facts.edge(successor, predecessor, EligibilityRole::PolicySupersession);
            }
            supersession
                .entry(successor.clone())
                .or_default()
                .insert(predecessor.clone());
        }
    }
    for id in cycle_members(&declared) {
        if let Some(row) = records.get_mut(&id) {
            mark_invalid(
                row,
                "dependency_cycle",
                diagnostics,
                serde_json::Value::Null,
                &mut recorder,
            );
        }
    }
    for id in cycle_members(&supersession) {
        if let Some(row) = records.get_mut(&id) {
            mark_invalid(
                row,
                "supersession_cycle",
                diagnostics,
                serde_json::Value::Null,
                &mut recorder,
            );
        }
    }
    for id in overlong_supersession_roots(&supersession) {
        if let Some(row) = records.get_mut(&id) {
            mark_invalid(
                row,
                "supersession_chain_over_limit",
                diagnostics,
                serde_json::Value::Null,
                &mut recorder,
            );
        }
    }
    recorder.phase(StructuralStage::Decision);
    apply_decisions(
        notes,
        records,
        diagnostics,
        decision_policy.as_ref(),
        &review_edges,
        &mut recorder,
    )?;

    recorder.phase(StructuralStage::RevisionIntegrity);
    // Verify all original assets, including unsupported captures; failed checks retain
    // dependencies read before failure so later verification cannot overlook tampering.
    let snapshot = records.clone();
    for row in records
        .values_mut()
        .filter(|r| r.record.kind() == RecordKind::Revision)
    {
        let record = row.record.clone();
        let source_id = RecordId::new(record.string("wiki_source_id").expect("source"))?;
        let mut deps = BTreeMap::new();
        let integrity = (|| -> Result<()> {
            if let Some((parent, _)) = row.path.as_str().rsplit_once('/') {
                for field in ["wiki_original_path", "wiki_content_path"] {
                    if let Some(payload) = record.string(field) {
                        let path = VaultRelativePath::new(format!("{parent}/{payload}"))?;
                        // Unprovided closed assets make this revision unavailable,
                        // as a failed payload read does. They cannot abort unrelated
                        // proposal validation or acquire a physical absence guard.
                        let expected = view.expected_state(&path)?;
                        deps.insert(path, expected);
                    }
                }
            }
            if record.string("wiki_extraction_status") == Some("complete") {
                view.revision_content(&source_id, record.id(), &mut deps)?;
            } else {
                let (parent, _) = row
                    .path
                    .as_str()
                    .rsplit_once('/')
                    .ok_or_else(|| WikiError::invalid("revision lacks directory"))?;
                let original_path = VaultRelativePath::new(format!(
                    "{parent}/{}",
                    record.string("wiki_original_path").expect("path")
                ))?;
                let bytes = view.read(&original_path, &mut deps)?;
                if Blake3Hash::digest(bytes).as_str()
                    != record.string("wiki_original_hash").expect("hash")
                {
                    return Err(WikiError::new(
                        ErrorCode::SourceIntegrity,
                        "original snapshot hash mismatch",
                    ));
                }
                let source = snapshot
                    .get(&source_id)
                    .ok_or_else(|| WikiError::invalid("missing source"))?;
                if !list(&source.record, "wiki_revisions")
                    .iter()
                    .any(|id| id == record.id().as_str())
                {
                    return Err(WikiError::invalid("revision not retained by source"));
                }
            }
            Ok(())
        })();
        row.dependencies.extend(
            deps.into_iter()
                .map(|(path, expected)| ReadDependency { path, expected }),
        );
        if let Err(error) = integrity {
            if error.code == ErrorCode::BudgetExceeded {
                return Err(error);
            }
            mark_invalid(
                row,
                "revision_integrity",
                diagnostics,
                serde_json::json!({"error":error}),
                &mut recorder,
            );
        }
    }
    // Structural errors follow typed references, independently from support freshness.
    recorder.phase(StructuralStage::Propagation);
    let missing = edges
        .values()
        .flatten()
        .filter(|id| !records.contains_key(*id))
        .map(|id| (id.clone(), false))
        .collect();
    structural_rules::propagate_invalid(records, &edges, diagnostics, &missing, &mut recorder)?;
    if let Some(facts) = facts.as_deref_mut() {
        facts.capture_baseline(records);
    }
    let snapshot = records.clone();
    for row in records.values_mut() {
        if row.eligibility == Eligibility::Invalid {
            continue;
        }
        let generation = if row.record.kind() == RecordKind::RunEvent
            && row.record.string("wiki_event_type") == Some("generation_output")
        {
            // Body/run/fingerprint binding is structural authority, not a
            // dynamic lifecycle rule. Preserve its exact legacy verification.
            let record = &row.record;
            let output = notes
                .get(&row.path)
                .and_then(|note| {
                    crate::graph::packet::fenced_json(
                        note,
                        "lwiki-api-extraction-output-v1",
                        crate::graph::MAX_ARTIFACT_BYTES,
                    )
                    .ok()
                })
                .and_then(|json| {
                    crate::graph::packet::decode::<
                            crate::graph::generation_cache::GenerationOutput,
                        >(json, crate::graph::MAX_ARTIFACT_BYTES)
                        .ok()
                });
            let packet = output.as_ref().and_then(|out| snapshot.get(&out.packet_id));
            let bound = output.as_ref().zip(packet).filter(|(out, packet)| {
                out.version == 1
                    && out.attempt.run_id.as_str()
                        == record.string("wiki_run_id").unwrap_or_default()
                    && out.attempt.task_key == out.task_key
                    && Blake3Hash::digest(out.response.as_bytes()) == out.response_hash
                    && packet.record.kind() == RecordKind::ExtractionPacket
                    && packet.record.string("wiki_packet_fingerprint")
                        == Some(out.packet_fingerprint.as_str())
            });
            if let Some((_, packet)) = bound {
                if let Some(facts) = facts.as_deref_mut() {
                    facts.edge(
                        row.record.id(),
                        packet.record.id(),
                        EligibilityRole::GenerationPacket,
                    );
                }
                let source = packet
                    .record
                    .string("wiki_source_id")
                    .and_then(|id| RecordId::new(id).ok())
                    .and_then(|id| snapshot.get(&id))
                    .filter(|source| source.record.kind() == RecordKind::Source)
                    .map(|source| &source.record);
                GenerationContext::Bound {
                    packet: &packet.record,
                    source,
                }
            } else {
                GenerationContext::Unbound
            }
        } else {
            GenerationContext::NotGeneration
        };
        if let Some(diagnostic) = rules::lifecycle(row, &snapshot, generation)? {
            diagnostics.push(diagnostic);
        }
    }
    let snapshot = records.clone();
    recorder.phase(StructuralStage::EvidenceIntegrity);
    for (id, row) in records
        .iter_mut()
        .filter(|(_, r)| r.record.kind() == RecordKind::Evidence)
    {
        let reference = evidence_reference(&row.record)?;
        if let Some(facts) = facts.as_deref_mut()
            && snapshot
                .get(&reference.assertion_id)
                .is_some_and(|row| row.record.kind() == RecordKind::Assertion)
        {
            // The inverse support relation belongs to an adopted record. A
            // dangling assertion remains represented by Evidence's typed edge.
            facts.edge(
                &reference.assertion_id,
                id,
                EligibilityRole::AssertionEvidence,
            );
        }
        edges
            .entry(reference.assertion_id.clone())
            .or_default()
            .insert(id.clone());
        let mut deps = BTreeMap::new();
        // Collect partial reads on errors as well as complete verification reads.
        let _ = view.revision_content(&reference.source_id, &reference.source_revision, &mut deps);
        row.dependencies.extend(
            deps.into_iter()
                .map(|(path, expected)| ReadDependency { path, expected }),
        );
        match view.verify(
            &CitationRef::Assertion(reference.clone()),
            CitationScope::Historical,
        ) {
            Ok(verified) => {
                row.dependencies.extend(verified.dependencies);
            }
            Err(error) => {
                if error.code == ErrorCode::BudgetExceeded {
                    return Err(error);
                }
                if let Some(facts) = facts.as_deref_mut() {
                    facts.evidence_integrity_failure(id);
                }
                mark_invalid(
                    row,
                    "evidence_integrity",
                    diagnostics,
                    serde_json::json!({"error":error}),
                    &mut recorder,
                );
                continue;
            }
        }
        if row.eligibility == Eligibility::Invalid {
            continue;
        }
        let source = snapshot.get(&reference.source_id).ok_or_else(|| {
            WikiError::new(
                ErrorCode::IndexCorrupt,
                "missing evidence lifecycle source boundary",
            )
        })?;
        rules::evidence_lifecycle(row, &source.record)?;
    }
    // Invalid evidence suppresses its own support; one damaged historical/support
    // record must not poison an assertion that still has intact current support.
    let evidence: Vec<_> = records
        .values()
        .filter(|r| r.record.kind() == RecordKind::Evidence)
        .cloned()
        .collect();
    for row in records
        .values_mut()
        .filter(|r| r.record.kind() == RecordKind::Assertion)
    {
        if row.eligibility == Eligibility::Invalid
            || row.record.string("wiki_status") != Some("accepted")
        {
            continue;
        }
        let associated: Vec<_> = evidence
            .iter()
            .filter(|r| r.record.string("wiki_assertion_id") == Some(row.record.id().as_str()))
            .collect();
        let expected = associated
            .iter()
            .map(|row| row.record.id().clone())
            .collect();
        rules::assertion_support(row, &expected, &associated)?;
    }
    apply_dependency_eligibility(records, &declared)?;
    let mut opposing: BTreeMap<OppositionKey, [Vec<RecordId>; 2]> = BTreeMap::new();
    for row in records.values().filter(|row| eligible_opposition(row)) {
        if let Some((key, negated)) = opposition_key(&row.record) {
            opposing.entry(key).or_default()[usize::from(negated)].push(row.record.id().clone());
        }
    }
    for sides in opposing
        .values()
        .filter(|sides| !sides[0].is_empty() && !sides[1].is_empty())
    {
        for id in sides[0].iter().chain(&sides[1]) {
            let row = records.get_mut(id).expect("grouped assertion");
            rules::mark_opposition(row);
        }
    }
    if let Some(facts) = facts.as_deref_mut() {
        facts.install_structural(recorder.finish())?;
        facts.capture_direct(records)?;
    } else {
        // All related bytes form an over-approximated closure. This intentionally
        // includes complete decisions and all evidence membership for a reviewed claim.
        let decision_deps: Vec<_> = records
            .values()
            .filter(|r| r.record.kind() == RecordKind::Decision)
            .flat_map(|r| r.dependencies.clone())
            .collect();
        let own = records.clone();
        for (id, row) in records.iter_mut() {
            let mut reachable = BTreeSet::new();
            let mut pending = vec![id.clone()];
            while let Some(next) = pending.pop() {
                if reachable.insert(next.clone()) {
                    pending.extend(edges.get(&next).into_iter().flatten().cloned());
                }
            }
            let mut deps: BTreeMap<_, _> = decision_deps
                .iter()
                .map(|d| (d.path.clone(), d.expected.clone()))
                .collect();
            for target in reachable {
                if let Some(target) = own.get(&target) {
                    deps.extend(
                        target
                            .dependencies
                            .iter()
                            .map(|d| (d.path.clone(), d.expected.clone())),
                    );
                }
            }
            row.dependencies = deps
                .into_iter()
                .map(|(path, expected)| ReadDependency { path, expected })
                .collect();
        }
    }
    for row in records.values_mut() {
        rules::finalize(row);
    }
    // Retain unused read-only argument as a reminder that eligibility does not infer prose.
    let _ = notes;
    Ok(())
}

fn apply_dependency_eligibility(
    records: &mut BTreeMap<RecordId, RecordRow>,
    declared: &BTreeMap<RecordId, BTreeSet<RecordId>>,
) -> Result<()> {
    // Preserve full-compute snapshot rounds. The bounded projector can use the
    // same single-row rule with its explicitly complete selected boundaries.
    for _ in 0..records.len() {
        let snapshot = records.clone();
        let mut changed = false;
        for (id, row) in records.iter_mut() {
            if matches!(
                row.eligibility,
                Eligibility::Invalid | Eligibility::Historical
            ) {
                continue;
            }
            let Some(targets) = declared.get(id) else {
                continue;
            };
            let boundary = targets
                .iter()
                .map(|target| {
                    (
                        target.clone(),
                        snapshot.get(target).map(|row| row.eligibility),
                    )
                })
                .collect();
            changed |= rules::declared_dependencies(row, &boundary)?;
        }
        if !changed {
            break;
        }
    }
    Ok(())
}

// Supersession maps successor to predecessors. Bound the combined ordinary,
// entity-decision and review graph independently of ID order or receipt type.
fn overlong_supersession_roots(
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

fn cycle_members(edges: &BTreeMap<RecordId, BTreeSet<RecordId>>) -> BTreeSet<RecordId> {
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

fn apply_decisions(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    records: &mut BTreeMap<RecordId, RecordRow>,
    diagnostics: &mut Vec<CatalogDiagnostic>,
    decision_policy: Option<&crate::graph::remap::VerifiedDecisionPolicy>,
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
