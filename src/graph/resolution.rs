//! Explicit guarded Pending mention decisions; proposals never imply acceptance.
use super::{extraction_types::*, import, mention_state::*, packet::*, resolution_types::*, wire};
use crate::{
    changes::*,
    domain::*,
    records::parse_note,
    sources::{
        SourceView,
        evidence::exact_quote_body,
        revision::{common, record_bytes, timestamp},
    },
    vault::{ExpectedState, WriterPermit},
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn conflict(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}

pub fn validate_resolution(view: &SourceView<'_>, bytes: &[u8]) -> Result<ValidatedResolution> {
    let request = normalize(decode::<ResolutionRequest>(bytes, MAX_RESOLUTION_BYTES)?)?;
    let (task_id, request_hash) = identity(&request)?;
    let extraction = import::load_extraction(view, &request.extraction_id)?;
    if let Some(restored) =
        load_resolution_receipt_scoped(view, &task_id, Some(&request.extraction_id))?
    {
        if restored.receipt.request != request || restored.receipt.request_hash != request_hash {
            return Err(conflict(
                "different resolution already canonical for extraction base",
            ));
        }
        return Ok(ValidatedResolution {
            request,
            request_hash,
            task_id,
            extraction: restored.extraction,
            dependencies: restored.dependencies,
            restored_receipt: Some(restored.receipt),
        });
    }
    if extraction.locator.observed_hash != request.expected_hash {
        return Err(conflict("extraction expected hash differs"));
    }
    let mut map = dependency_map(&extraction.dependencies)?;
    for mapping in &request.mappings {
        if extraction.artifact.bindings.get(mention(mapping)) != Some(&MentionBinding::Pending) {
            return Err(conflict(
                "resolution requires an existing Pending mention; remaps require explicit decisions",
            ));
        }
        if let ResolutionMapping::BindMention {
            entity_id,
            expected_entity_hash,
            ..
        } = mapping
        {
            let (path, note) = view.resolve(entity_id, RecordKind::Entity, None)?;
            if note.source_hash != *expected_entity_hash {
                return Err(conflict("entity expected hash differs"));
            }
            if note
                .canonical
                .as_ref()
                .and_then(|r| r.string("wiki_status"))
                != Some("active")
            {
                return Err(invalid("binding target must be active entity"));
            }
            SourceView::note_dependency(path, note, &mut map);
        }
    }
    Ok(ValidatedResolution {
        request,
        request_hash,
        task_id,
        extraction,
        dependencies: dependencies(map),
        restored_receipt: None,
    })
}

fn ready(
    response: &ExtractionResponse,
    bindings: &BTreeMap<PacketLocalId, MentionBinding>,
    materialized: &[PacketLocalId],
) -> Vec<PacketLocalId> {
    let is_resolved =
        |id: &PacketLocalId| matches!(bindings.get(id), Some(MentionBinding::Resolved { .. }));
    let mut ids: Vec<_> = response
        .assertions
        .iter()
        .filter(|a| {
            !materialized.contains(&a.id)
                && is_resolved(&a.subject)
                && match &a.object {
                    ExtractionObject::Mention { mention_id } => is_resolved(mention_id),
                    ExtractionObject::Literal { .. } => true,
                }
        })
        .map(|a| a.id.clone())
        .collect();
    ids.sort();
    ids
}
fn summary(validated: &ValidatedResolution) -> ResolutionSummary {
    let mut bindings = validated.extraction.artifact.bindings.clone();
    // Read-only logical endpoints; no durable ID allocation occurs here.
    if validated.restored_receipt.is_none() {
        for mapping in &validated.request.mappings {
            let local = mention(mapping);
            let decision_id = validated.task_id.clone();
            let state = match mapping {
                ResolutionMapping::BindMention { entity_id, .. } => MentionBinding::Resolved {
                    entity_id: entity_id.clone(),
                    decision_id,
                },
                ResolutionMapping::CreateEntity { .. } => MentionBinding::Resolved {
                    entity_id: validated.task_id.clone(),
                    decision_id,
                },
                ResolutionMapping::RejectMention { .. } => MentionBinding::Rejected { decision_id },
            };
            bindings.insert(local.clone(), state);
        }
    }
    ResolutionSummary {
        pending_mentions: bindings
            .values()
            .filter(|b| matches!(b, MentionBinding::Pending))
            .count(),
        resolved_mentions: bindings
            .values()
            .filter(|b| matches!(b, MentionBinding::Resolved { .. }))
            .count(),
        rejected_mentions: bindings
            .values()
            .filter(|b| matches!(b, MentionBinding::Rejected { .. }))
            .count(),
        create_entities: validated
            .request
            .mappings
            .iter()
            .filter(|m| matches!(m, ResolutionMapping::CreateEntity { .. }))
            .count(),
        create_decisions: validated.request.mappings.len(),
        materialize_assertions: validated.restored_receipt.as_ref().map_or_else(
            || {
                ready(
                    &validated.extraction.response,
                    &bindings,
                    &validated.extraction.artifact.materialized_assertions,
                )
            },
            |r| r.materialized_assertions.clone(),
        ),
    }
}
pub fn plan_resolution(validated: &ValidatedResolution) -> Result<ResolutionPlan> {
    Ok(ResolutionPlan {
        validated: validated.clone(),
        summary: summary(validated),
    })
}

fn path(kind: RecordKind, id: &RecordId) -> Result<VaultRelativePath> {
    let directory = match kind {
        RecordKind::Entity => "entities",
        RecordKind::Assertion => "assertions",
        RecordKind::Evidence => "evidence",
        RecordKind::Decision => "decisions",
        _ => return Err(invalid("unsupported resolution record kind")),
    };
    VaultRelativePath::new(format!("knowledge/{directory}/{id}.md"))
}
fn entity_bytes(id: &RecordId, title: &str, kind: &str) -> Result<Vec<u8>> {
    let mut fields = common(id, RecordKind::Entity, title);
    fields.insert("wiki_status".into(), "active".into());
    fields.insert("wiki_entity_type".into(), kind.into());
    record_bytes(
        CanonicalRecord::new(fields)?,
        b"\nIdentity created by an explicit source-local mention decision.\n",
    )
}
fn bound<'a>(artifact: &'a ExtractionArtifactV1, id: &PacketLocalId) -> Result<&'a RecordId> {
    match artifact.bindings.get(id) {
        Some(MentionBinding::Resolved { entity_id, .. }) => Ok(entity_id),
        _ => Err(invalid("assertion endpoint is not explicitly resolved")),
    }
}
fn materialized_bytes(
    view: &SourceView<'_>,
    artifact: &ExtractionArtifactV1,
    response: &ExtractionResponse,
    local: &PacketLocalId,
    saved_paths: Option<&BTreeMap<RecordId, VaultRelativePath>>,
) -> Result<Vec<(VaultRelativePath, Vec<u8>)>> {
    let assertion = response
        .assertions
        .iter()
        .find(|a| &a.id == local)
        .ok_or_else(|| invalid("unknown materialized assertion"))?;
    let id = &artifact.allocations.assertions[local];
    let subject = bound(artifact, &assertion.subject)?;
    let object = match &assertion.object {
        ExtractionObject::Mention { mention_id } => Some(bound(artifact, mention_id)?),
        ExtractionObject::Literal { .. } => None,
    };
    let record = wire::proposition(assertion, subject, object, id)?;
    let mut fields = record.fields().clone();
    let (subject_path, _) = view.resolve(subject, RecordKind::Entity, None)?;
    let subject_path = original_path(saved_paths, subject, subject_path)?;
    fields.insert("wiki_subject".into(), format!("[[{subject_path}]]").into());
    if let Some(object) = object {
        let (object_path, _) = view.resolve(object, RecordKind::Entity, None)?;
        let object_path = original_path(saved_paths, object, object_path)?;
        fields.insert("wiki_object".into(), format!("[[{object_path}]]").into());
    }
    let assertion_path = original_path(saved_paths, id, &path(RecordKind::Assertion, id)?)?;
    let mut writes=vec![(assertion_path.clone(),record_bytes(CanonicalRecord::new(fields)?,b"\nSource-local proposition materialized by explicit endpoint decisions; factual status remains proposed.\n")?)];
    let (source_path, _) = view.resolve(&artifact.source_id, RecordKind::Source, None)?;
    let (revision_path, _) = view.resolve(&artifact.source_revision, RecordKind::Revision, None)?;
    let source_path = original_path(saved_paths, &artifact.source_id, source_path)?;
    let revision_path = original_path(saved_paths, &artifact.source_revision, revision_path)?;
    for (i, evidence) in assertion.evidence.iter().enumerate() {
        let id = &artifact.allocations.evidence[local][i];
        let trace = &artifact.evidence_spans[local][i];
        let mut fields = common(id, RecordKind::Evidence, "Source-local assertion evidence");
        for (key, value) in [
            ("wiki_status", "active"),
            (
                "wiki_assertion_id",
                artifact.allocations.assertions[local].as_str(),
            ),
            ("wiki_source_id", artifact.source_id.as_str()),
            ("wiki_source_revision", artifact.source_revision.as_str()),
            ("wiki_stance", evidence.stance.as_str()),
            ("wiki_locator_kind", "utf8-bytes"),
            ("wiki_quote_hash", trace.quote_hash.as_str()),
            ("wiki_extraction_id", artifact.extraction_id.as_str()),
        ] {
            fields.insert(key.into(), value.into());
        }
        fields.insert("wiki_span_start".into(), trace.span.start().into());
        fields.insert("wiki_span_end".into(), trace.span.end().into());
        for (key, path) in [
            ("wiki_assertion", &assertion_path),
            ("wiki_source", &source_path),
            ("wiki_revision", &revision_path),
        ] {
            fields.insert(key.into(), format!("[[{path}]]").into());
        }
        let body = exact_quote_body(
            evidence.quote.as_bytes(),
            "\n",
            "Original source quotation retained by explicit resolution.",
        )?;
        writes.push((
            original_path(saved_paths, id, &path(RecordKind::Evidence, id)?)?,
            record_bytes(CanonicalRecord::new(fields)?, &body)?,
        ));
    }
    Ok(writes)
}

fn original_path(
    saved: Option<&BTreeMap<RecordId, VaultRelativePath>>,
    id: &RecordId,
    current: &VaultRelativePath,
) -> Result<VaultRelativePath> {
    saved.map_or_else(
        || Ok(current.clone()),
        |paths| {
            paths
                .get(id)
                .cloned()
                .ok_or_else(|| invalid("missing immutable original record path"))
        },
    )
}

fn guarded_projection<'a>(
    view: &SourceView<'a>,
    deps: &[ReadDependency],
    writes: &[ExpectedWrite],
) -> Result<SourceView<'a>> {
    let mut notes = view.notes.clone();
    let mut overlay = BTreeMap::new();
    // Capture only proven inputs under the payload ceiling, then close all fallback reads.
    for dep in deps {
        if !notes.contains_key(&dep.path) {
            let bytes = crate::changes::prepare::read_bounded(view.fs, &dep.path, SOURCE_CAP)?;
            let state = bytes.as_ref().map_or(ExpectedState::Absent, |b| {
                ExpectedState::Hash(Blake3Hash::digest(b))
            });
            if state != dep.expected {
                return Err(conflict(
                    "source proof input changed while capturing projection",
                ));
            }
            overlay.insert(dep.path.clone(), bytes);
        }
    }
    for write in writes {
        match &write.proposed {
            Some(bytes) => {
                notes.insert(write.target.clone(), parse_note(bytes));
            }
            None => {
                notes.remove(&write.target);
            }
        }
        overlay.insert(write.target.clone(), write.proposed.clone());
    }
    if notes.len() > 4096
        || notes
            .values()
            .try_fold(0usize, |sum, n| sum.checked_add(n.raw.len()))
            .is_none_or(|n| n > SOURCE_CAP)
    {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "proposed canonical view exceeds source view ceiling",
        ));
    }
    Ok(SourceView {
        fs: view.fs,
        notes,
        overlay,
        closed: true,
    })
}

pub(crate) fn verify_operation_proofs(
    view: &SourceView<'_>,
    receipt: &ResolutionReceiptV1,
    extraction: &VerifiedExtractionArtifact,
) -> Result<()> {
    let historical = historical_state(view, receipt, &extraction.artifact)?;
    let prior_ready = ready(&extraction.response, &receipt.prior_bindings, &[]);
    if receipt
        .prior_materialized_assertions
        .iter()
        .any(|id| !prior_ready.contains(id))
    {
        return Err(invalid(
            "prior materialized proposition lacks prior explicit endpoints",
        ));
    }
    if ready(
        &extraction.response,
        &historical.bindings,
        &receipt.prior_materialized_assertions,
    ) != receipt.materialized_assertions
    {
        return Err(invalid(
            "receipt does not materialize exactly the ready proposals",
        ));
    }
    let extraction_path = receipt
        .record_paths
        .get(&historical.extraction_id)
        .ok_or_else(|| invalid("missing original extraction path"))?;
    let mut expected = BTreeMap::from([(
        extraction_path.clone(),
        ExpectedState::Hash(receipt.request.expected_hash.clone()),
    )]);
    for mapping in &receipt.request.mappings {
        if let ResolutionMapping::CreateEntity {
            title, entity_type, ..
        } = mapping
        {
            let id = &receipt.allocations.entities[mention(mapping)];
            let target = &receipt.record_paths[id];
            expected.insert(target.clone(), ExpectedState::Absent);
            let proof = receipt
                .operations
                .iter()
                .find(|p| &p.target == target)
                .ok_or_else(|| invalid("missing created entity operation proof"))?;
            if proof.after
                != ExpectedState::Hash(Blake3Hash::digest(entity_bytes(id, title, entity_type)?))
            {
                return Err(invalid("created entity operation hash proof mismatch"));
            }
        }
    }
    for local in &receipt.materialized_assertions {
        for (target, bytes) in materialized_bytes(
            view,
            &historical,
            &extraction.response,
            local,
            Some(&receipt.record_paths),
        )? {
            expected.insert(target.clone(), ExpectedState::Absent);
            let proof = receipt
                .operations
                .iter()
                .find(|p| p.target == target)
                .ok_or_else(|| invalid("missing materialization operation proof"))?;
            if proof.after != ExpectedState::Hash(Blake3Hash::digest(bytes)) {
                return Err(invalid("materialization operation hash proof mismatch"));
            }
        }
    }
    if expected.len() != receipt.operations.len()
        || receipt
            .operations
            .iter()
            .any(|p| expected.get(&p.target) != Some(&p.before))
    {
        return Err(invalid(
            "resolution operation proof set/before-state mismatch",
        ));
    }
    // Extraction whole-note before/after hashes are original claims only in
    // canonical acknowledgement. Author prose/title/path edits are independent
    // of immutable trace and explicit semantic decisions. Only an actual retained
    // manifest can bind those claims to original payload bytes; no reconstruction
    // or reapplication of missing historical note bytes occurs here.
    Ok(())
}

fn decision_bytes(mapping: &ResolutionMapping, receipt: &ResolutionReceiptV1) -> Result<Vec<u8>> {
    let local = mention(mapping);
    let id = &receipt.allocations.decisions[local];
    let mut fields = common(
        id,
        RecordKind::Decision,
        "Explicit source-local mention resolution",
    );
    for (key, value) in [
        ("wiki_status", "active"),
        ("wiki_action", action(mapping)),
        ("wiki_extraction_id", receipt.request.extraction_id.as_str()),
    ] {
        fields.insert(key.into(), value.into());
    }
    fields.insert("wiki_created_at".into(), timestamp()?.into());
    fields.insert(
        "wiki_input_ids".into(),
        json!([receipt.request.extraction_id]),
    );
    fields.insert("wiki_mention_ids".into(), json!([local]));
    fields.insert(
        "wiki_output_ids".into(),
        match &receipt.transitions[local].after {
            MentionBinding::Resolved { entity_id, .. } => json!([entity_id]),
            MentionBinding::Rejected { .. } => json!([]),
            MentionBinding::Pending => return Err(invalid("pending decision outcome")),
        },
    );
    let mut body = format!(
        "\nExplicit {} for source-local mention {}.\n\nRationale: {}\n\n",
        action(mapping),
        local,
        serde_json::to_string(reason(mapping)).map_err(|e| invalid(e.to_string()))?
    )
    .into_bytes();
    body.extend(render_fence(
        receipt,
        RESOLUTION_FENCE,
        MAX_RESOLUTION_RECEIPT_BYTES,
    )?);
    record_bytes(CanonicalRecord::new(fields)?, &body)
}
fn allocation_map(allocations: &ResolutionAllocations) -> BTreeMap<String, RecordId> {
    allocations
        .decisions
        .iter()
        .map(|(local, id)| (format!("decision:{local}"), id.clone()))
        .chain(
            allocations
                .entities
                .iter()
                .map(|(local, id)| (format!("entity:{local}"), id.clone())),
        )
        .collect()
}
fn capture_dependencies(
    view: &SourceView<'_>,
    original: &[ReadDependency],
    source_id: &RecordId,
) -> Result<Vec<ReadDependency>> {
    let mut items = original.to_vec();
    items.extend(view.notes.iter().map(|(path, note)| ReadDependency {
        path: path.clone(),
        expected: ExpectedState::Hash(note.source_hash.clone()),
    }));
    // The closed catalog projection sees the source's complete revision manifest.
    // Capture every retained revision's assets as well: leaving a sibling payload
    // out makes that revision appear corrupt and invalidates the source through
    // its typed references, including evidence from the selected revision.
    let (_, source_note) = view.resolve(source_id, RecordKind::Source, None)?;
    let source = source_note.canonical.as_ref().expect("resolved source");
    let revisions = source
        .field("wiki_revisions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid("source revision manifest is invalid"))?;
    let mut assets = BTreeMap::new();
    let mut captured = 0usize;
    for value in revisions {
        let revision_id = RecordId::new(
            value
                .as_str()
                .ok_or_else(|| invalid("source revision manifest is invalid"))?,
        )?;
        let (revision_path, revision_note) =
            view.resolve(&revision_id, RecordKind::Revision, None)?;
        let revision = revision_note.canonical.as_ref().expect("resolved revision");
        if revision.string("wiki_source_id") != Some(source_id.as_str()) {
            return Err(invalid("retained revision belongs to another source"));
        }
        let parent = revision_path
            .as_str()
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .ok_or_else(|| invalid("retained revision has no directory"))?;
        for field in ["wiki_original_path", "wiki_content_path"] {
            if let Some(name) = revision.string(field) {
                let path = VaultRelativePath::new(format!("{parent}/{name}"))?;
                if assets.contains_key(&path) {
                    continue;
                }
                let remaining = SOURCE_CAP
                    .checked_sub(captured)
                    .filter(|n| *n > 0)
                    .ok_or_else(|| {
                        WikiError::new(
                            ErrorCode::BudgetExceeded,
                            "retained revision assets exceed resolution projection ceiling",
                        )
                    })?;
                let bytes = view.read_bounded(&path, &mut assets, remaining)?;
                captured = captured
                    .checked_add(bytes.len())
                    .filter(|n| *n <= SOURCE_CAP)
                    .ok_or_else(|| {
                        WikiError::new(
                            ErrorCode::BudgetExceeded,
                            "retained revision assets exceed resolution projection ceiling",
                        )
                    })?;
            }
        }
    }
    items.extend(dependencies(assets));
    Ok(dependencies(dependency_map(&items)?))
}

fn capture_record_paths(
    view: &SourceView<'_>,
    before: &ExtractionArtifactV1,
    after: &ExtractionArtifactV1,
    allocations: &ResolutionAllocations,
) -> Result<BTreeMap<RecordId, VaultRelativePath>> {
    let mut paths = BTreeMap::new();
    for (id, kind) in [
        (&after.extraction_id, RecordKind::Extraction),
        (&after.packet_id, RecordKind::ExtractionPacket),
        (&after.source_id, RecordKind::Source),
        (&after.source_revision, RecordKind::Revision),
    ] {
        let (path, _) = view.resolve(id, kind, None)?;
        paths.insert(id.clone(), path.clone());
    }
    for (local, id) in &after.allocations.assertions {
        let materialized = before.materialized_assertions.contains(local);
        let assertion = if materialized {
            view.resolve(id, RecordKind::Assertion, None)?.0.clone()
        } else {
            path(RecordKind::Assertion, id)?
        };
        paths.insert(id.clone(), assertion);
        for id in &after.allocations.evidence[local] {
            let evidence = if materialized {
                view.resolve(id, RecordKind::Evidence, None)?.0.clone()
            } else {
                path(RecordKind::Evidence, id)?
            };
            paths.insert(id.clone(), evidence);
        }
    }
    for binding in after.bindings.values() {
        let decision = match binding {
            MentionBinding::Resolved {
                entity_id,
                decision_id,
            } => {
                let (path, _) = view.resolve(entity_id, RecordKind::Entity, None)?;
                paths.insert(entity_id.clone(), path.clone());
                decision_id
            }
            MentionBinding::Rejected { decision_id } => decision_id,
            MentionBinding::Pending => continue,
        };
        let decision_path = if allocations.decisions.values().any(|id| id == decision) {
            path(RecordKind::Decision, decision)?
        } else {
            view.resolve(decision, RecordKind::Decision, None)?
                .0
                .clone()
        };
        paths.insert(decision.clone(), decision_path);
    }
    if paths.len() > 772 {
        return Err(invalid("historical record path count exceeds ceiling"));
    }
    Ok(paths)
}
fn build_draft(view: &SourceView<'_>, validated: &ValidatedResolution) -> Result<ChangeDraft> {
    let before = &validated.extraction.artifact;
    let planned = summary(validated);
    let materialized_evidence: usize = validated
        .extraction
        .response
        .assertions
        .iter()
        .filter(|a| planned.materialize_assertions.contains(&a.id))
        .map(|a| a.evidence.len())
        .sum();
    let count = 1
        + planned.create_entities
        + planned.create_decisions
        + planned.materialize_assertions.len()
        + materialized_evidence;
    if count > crate::changes::prepare::MAX_OPS || count - planned.create_decisions > 705 {
        return Err(invalid(
            "resolution operation count exceeds retained/receipt ceiling",
        ));
    }
    let mut allocations = ResolutionAllocations {
        decisions: BTreeMap::new(),
        entities: BTreeMap::new(),
    };
    let mut writes = vec![];
    for mapping in &validated.request.mappings {
        let local = mention(mapping).clone();
        allocations
            .decisions
            .insert(local.clone(), RecordId::generate(RecordKind::Decision)?);
        if let ResolutionMapping::CreateEntity {
            title, entity_type, ..
        } = mapping
        {
            let id = RecordId::generate(RecordKind::Entity)?;
            writes.push(ExpectedWrite {
                target: path(RecordKind::Entity, &id)?,
                expected: ExpectedState::Absent,
                proposed: Some(entity_bytes(&id, title, entity_type)?),
                apply_after: vec![],
            });
            allocations.entities.insert(local, id);
        }
    }
    let mut after = before.clone();
    let mut transitions = BTreeMap::new();
    for mapping in &validated.request.mappings {
        let binding = expected_binding(mapping, &allocations)?;
        let local = mention(mapping).clone();
        transitions.insert(
            local.clone(),
            BindingTransition {
                before: MentionBinding::Pending,
                after: binding.clone(),
            },
        );
        after.bindings.insert(local, binding);
    }
    let materialized = ready(
        &validated.extraction.response,
        &after.bindings,
        &before.materialized_assertions,
    );
    // Explicitly constructed entities become visible to proposition rendering only.
    let rendering = guarded_projection(view, &validated.dependencies, &writes)?;
    let record_paths = capture_record_paths(&rendering, before, &after, &allocations)?;
    for local in &materialized {
        for (target, bytes) in materialized_bytes(
            &rendering,
            &after,
            &validated.extraction.response,
            local,
            Some(&record_paths),
        )? {
            writes.push(ExpectedWrite {
                target,
                expected: ExpectedState::Absent,
                proposed: Some(bytes),
                apply_after: vec![],
            });
        }
    }
    after.materialized_assertions.extend(materialized.clone());
    after.materialized_assertions.sort();
    import::verify_resolution_transition(before, &after)?;
    let (extraction_path, note) =
        view.resolve(&before.extraction_id, RecordKind::Extraction, None)?;
    writes.push(ExpectedWrite {
        target: extraction_path.clone(),
        expected: ExpectedState::Hash(validated.request.expected_hash.clone()),
        proposed: Some(import::edit_extraction_artifact(note, &after)?),
        apply_after: vec![],
    });
    writes.sort_by(|a, b| a.target.cmp(&b.target));
    let receipt = ResolutionReceiptV1 {
        schema: RESOLUTION_RECEIPT_SCHEMA.into(),
        task_id: validated.task_id.clone(),
        request: validated.request.clone(),
        request_hash: validated.request_hash.clone(),
        immutable_extraction_hash: immutable_hash(before)?,
        allocations: allocations.clone(),
        prior_bindings: before.bindings.clone(),
        prior_materialized_assertions: {
            let mut ids = before.materialized_assertions.clone();
            ids.sort();
            ids
        },
        record_paths,
        transitions,
        materialized_assertions: materialized,
        operations: writes
            .iter()
            .map(|op| ResolutionWriteProof {
                target: op.target.clone(),
                before: op.expected.clone(),
                after: ExpectedState::Hash(Blake3Hash::digest(
                    op.proposed.as_ref().expect("resolution never deletes"),
                )),
            })
            .collect(),
    };
    for mapping in &validated.request.mappings {
        writes.push(ExpectedWrite {
            target: path(
                RecordKind::Decision,
                &allocations.decisions[mention(mapping)],
            )?,
            expected: ExpectedState::Absent,
            proposed: Some(decision_bytes(mapping, &receipt)?),
            apply_after: vec![],
        });
    }
    // Full canonical scan guards prevent another author changing unrelated identity
    // or decisions between projection validation and application.
    let read_preconditions =
        capture_dependencies(view, &validated.dependencies, &before.source_id)?;
    let projected = guarded_projection(view, &read_preconditions, &writes)?;
    let (_, note) = projected.resolve(&before.extraction_id, RecordKind::Extraction, None)?;
    import::verify_extraction_note(&projected, extraction_path, note)?;
    load_resolution_receipt_scoped(&projected, &validated.task_id, Some(&before.extraction_id))?
        .ok_or_else(|| invalid("new resolution receipt cannot be restored"))?;
    let mut documents: Vec<_> = view
        .notes
        .iter()
        .map(|(path, note)| ScanDocument {
            path: path.clone(),
            bytes: note.raw.clone(),
            hash: note.source_hash.clone(),
        })
        .collect();
    for (path, bytes) in &projected.overlay {
        if !view.notes.contains_key(path)
            && !writes.iter().any(|w| &w.target == path)
            && let Some(bytes) = bytes
        {
            documents.push(ScanDocument {
                path: path.clone(),
                bytes: bytes.clone(),
                hash: Blake3Hash::digest(bytes),
            });
        }
    }
    let input = ValidationInput {
        vault_id: vault_id(view)?,
        documents,
        overlay: writes
            .iter()
            .map(|op| ProposedTarget {
                path: op.target.clone(),
                bytes: op.proposed.clone(),
            })
            .collect(),
    };
    crate::catalog::CatalogGraphValidator.validate_closed(view.fs, &input)?;
    Ok(ChangeDraft {
        title: "Resolve explicit source-local mentions".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: allocation_map(&allocations),
        read_preconditions,
        operations: writes,
    })
}

fn retained_receipt(
    engine: &ChangeEngine,
    view: &SourceView<'_>,
    change: &ChangeInspection,
    task: &RecordId,
) -> Result<VerifiedResolutionReceipt> {
    let mut writes = vec![];
    for (i, op) in change.manifest.operations.iter().enumerate() {
        let bytes = engine.verify_payload(
            &change.manifest.change_id,
            i,
            "proposed",
            &op.target,
            &op.after,
            &op.after_payload,
        )?;
        writes.push(ExpectedWrite {
            target: op.target.clone(),
            expected: op.before.clone(),
            proposed: bytes,
            apply_after: vec![],
        });
    }
    let projected = guarded_projection(view, &change.manifest.read_preconditions, &writes)?;
    let verified = load_resolution_receipt(&projected, task)?
        .ok_or_else(|| invalid("retained resolution lacks receipt"))?;
    if change.manifest.allocated_ids != allocation_map(&verified.receipt.allocations) {
        return Err(invalid(
            "retained resolution allocations disagree with receipt",
        ));
    }
    let decision_targets: BTreeSet<_> = verified
        .receipt
        .allocations
        .decisions
        .values()
        .map(|id| {
            verified
                .receipt
                .record_paths
                .get(id)
                .cloned()
                .ok_or_else(|| invalid("missing original decision path"))
        })
        .collect::<Result<_>>()?;
    let actual: Vec<_> = change
        .manifest
        .operations
        .iter()
        .filter(|op| !decision_targets.contains(&op.target))
        .map(|op| ResolutionWriteProof {
            target: op.target.clone(),
            before: op.before.clone(),
            after: op.after.clone(),
        })
        .collect();
    if actual != verified.receipt.operations
        || change.manifest.operations.len() != actual.len() + decision_targets.len()
    {
        return Err(invalid(
            "retained manifest disagrees with exact receipt operations",
        ));
    }
    Ok(verified)
}

fn verify_retained_canonical(
    view: &SourceView<'_>,
    engine: &ChangeEngine,
    change: &ChangeInspection,
    canonical: &ResolutionReceiptV1,
    current: &VerifiedExtractionArtifact,
) -> Result<()> {
    if change.manifest.allocated_ids != allocation_map(&canonical.allocations) {
        return Err(invalid(
            "retained resolution allocations disagree with canonical receipt",
        ));
    }
    let decision_targets: BTreeMap<_, _> = canonical
        .request
        .mappings
        .iter()
        .map(|mapping| {
            Ok((
                canonical
                    .record_paths
                    .get(&canonical.allocations.decisions[mention(mapping)])
                    .cloned()
                    .ok_or_else(|| invalid("missing original decision path"))?,
                mapping,
            ))
        })
        .collect::<Result<_>>()?;
    let mut actual = vec![];
    let mut decisions = 0;
    let mut extraction = false;
    for (i, op) in change.manifest.operations.iter().enumerate() {
        let bytes = engine
            .verify_payload(
                &change.manifest.change_id,
                i,
                "proposed",
                &op.target,
                &op.after,
                &op.after_payload,
            )?
            .ok_or_else(|| invalid("resolution cannot retain deletion"))?;
        if let Some(mapping) = decision_targets.get(&op.target) {
            if op.before != ExpectedState::Absent {
                return Err(invalid("retained decision is not an allocated create"));
            }
            verify_decision_note(&parse_note(&bytes), mapping, canonical)?;
            decisions += 1;
        } else {
            actual.push(ResolutionWriteProof {
                target: op.target.clone(),
                before: op.before.clone(),
                after: op.after.clone(),
            });
            if canonical.record_paths.get(&current.artifact.extraction_id) == Some(&op.target) {
                let note = parse_note(&bytes);
                let record = note
                    .canonical
                    .as_ref()
                    .ok_or_else(|| invalid("invalid retained extraction envelope"))?;
                let artifact: ExtractionArtifactV1 = decode(
                    fenced_json(&note, import::ARTIFACT_FENCE, MAX_ARTIFACT_BYTES)?,
                    MAX_ARTIFACT_BYTES,
                )?;
                if record.kind() != RecordKind::Extraction
                    || record.id() != &artifact.extraction_id
                    || record.string("wiki_packet_id") != Some(artifact.packet_id.as_str())
                    || record.string("wiki_input_hash")
                        != Some(artifact.packet_fingerprint.as_str())
                    || record.field("wiki_source_ids") != Some(&json!([artifact.source_id]))
                    || record.field("wiki_source_revision_ids")
                        != Some(&json!([artifact.source_revision]))
                {
                    return Err(invalid("retained extraction envelope identity differs"));
                }
                if artifact != historical_state(view, canonical, &current.artifact)? {
                    return Err(invalid(
                        "retained extraction disagrees with complete prior/after semantic state",
                    ));
                }
                for (local, transition) in &canonical.transitions {
                    if artifact.bindings.get(local) != Some(&transition.after) {
                        return Err(invalid(
                            "retained extraction disagrees with receipt transitions",
                        ));
                    }
                }
                extraction = true;
            }
        }
    }
    if actual != canonical.operations || decisions != decision_targets.len() || !extraction {
        return Err(invalid(
            "canonical and retained resolution operation/identity proofs differ",
        ));
    }
    Ok(())
}

pub fn stage_resolution(
    engine: &ChangeEngine,
    writer: &WriterPermit,
    validated: &ValidatedResolution,
) -> Result<ResolutionOutcome> {
    writer.require_root(engine.fs().root())?;
    let guard = ChangeDraft {
        title: "Recheck resolution authority".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: validated.dependencies.clone(),
        operations: vec![],
    };
    engine.plan(&guard)?;
    let view = SourceView::from_fs_bounded(engine.fs(), SOURCE_CAP, 4096)?;
    let current = validate_resolution(&view, &canonical_json(&validated.request)?)?;
    let origin = ChangeOrigin {
        operation: OriginOperation::GraphResolve,
        packet_id: validated.task_id.clone(),
        response_hash: validated.request_hash.clone(),
    };
    if let Some(canonical) = &current.restored_receipt {
        let mut retained = None;
        for id in engine.change_ids()? {
            let inspection = engine.inspect(&id)?;
            if inspection
                .manifest
                .origin
                .as_ref()
                .is_some_and(|o| o.operation == origin.operation && o.packet_id == origin.packet_id)
            {
                if inspection.manifest.origin.as_ref() != Some(&origin) {
                    return Err(conflict(
                        "different retained resolution for canonical task scope",
                    ));
                }
                verify_retained_canonical(
                    &view,
                    engine,
                    &inspection,
                    canonical,
                    &current.extraction,
                )?;
                if retained.replace(inspection).is_some() {
                    return Err(invalid("duplicate retained resolution origins"));
                }
            }
        }
        return Ok(ResolutionOutcome {
            extraction: current.extraction.locator.clone(),
            prepared: retained.as_ref().map(|c| c.prepared.clone()),
            status: retained.as_ref().map(|c| c.status),
            disposition: if retained.is_some() {
                ResolutionDisposition::RetainedChange
            } else {
                ResolutionDisposition::CanonicalRestored
            },
            allocations: canonical.allocations.clone(),
            summary: summary(&current),
            reused: true,
        });
    }
    // The FnOnce is the sole durable-ID allocation authority.
    let prepared =
        engine.prepare_or_reuse(writer, origin, OriginPolicy::ReuseOrConflict, || {
            build_draft(&view, &current)
        })?;
    let proposal = retained_receipt(engine, &view, &prepared.change, &validated.task_id)?;
    if proposal.receipt.request != validated.request
        || proposal.receipt.request_hash != validated.request_hash
    {
        return Err(invalid("retained resolution request mismatch"));
    }
    Ok(ResolutionOutcome {
        extraction: proposal.extraction.locator,
        prepared: Some(prepared.change.prepared),
        status: Some(prepared.change.status),
        disposition: ResolutionDisposition::RetainedChange,
        allocations: proposal.receipt.allocations,
        summary: summary(&current),
        reused: prepared.reused,
    })
}
