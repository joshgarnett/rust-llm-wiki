//! Disposable-fixture integration checks for the private evidence-set seam.
//! These qualify binding and admission mechanics, not retrieval answer quality.
use super::*;
use crate::{
    app::{OfflineApp, OperationOptions, offline},
    catalog::{Catalog, DocumentRow, ReaderSnapshot},
    retrieval::{
        ExcerptLabel,
        context_types::ContextSemanticCue,
        context_units::{UnitDocument, select_units},
        render::TargetKind,
        spaces::{EmbeddingSettings, RENDER_VERSION},
    },
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
    vault::{VaultFs, VaultRoot},
};
use std::time::Duration;

struct Fixture {
    _temp: tempfile::TempDir,
    reader: ReaderSnapshot,
    document: DocumentRow,
}
impl Fixture {
    fn new(raw: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        offline::init(&root, "Evidence-set fixture", OperationOptions::default()).unwrap();
        let app = OfflineApp::new(
            VaultFs::new(VaultRoot::explicit(&root).unwrap()),
            OperationOptions::default(),
        )
        .unwrap();
        app.source_add(CaptureRequest {
            title: "Synthetic evidence-set source".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "synthetic.md".into(),
            original: raw.as_bytes().to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: Some("text/markdown".into()),
        })
        .unwrap();
        let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
        let reader = catalog.verified_snapshot(None).unwrap();
        let document = reader
            .projection()
            .documents
            .iter()
            .find(|document| document.owner_revision.is_some())
            .unwrap()
            .clone();
        assert_eq!(document.raw_text, raw);
        Self {
            _temp: temp,
            reader,
            document,
        }
    }

    fn passage(&self, span: ByteSpan) -> ContextPassage {
        let text = span.slice(&self.document.raw_text).unwrap().to_owned();
        ContextPassage {
            locator: DocumentLocator {
                record: Some(RecordRef {
                    vault_id: self.reader.projection().vault_id.clone(),
                    record_id: self.document.owner_revision.clone().unwrap(),
                    expected_kind: RecordKind::Revision,
                }),
                path: self.document.path.clone(),
                observed_hash: self.document.hash.clone(),
            },
            text: text.clone(),
            span,
            label: ExcerptLabel::CapturedSource,
            eligibility: self.document.eligibility,
            citations: vec![CitationRef::Source(SourceSpanRef {
                source_id: self.document.source_id.clone().unwrap(),
                source_revision: self.document.owner_revision.clone().unwrap(),
                span,
                quote_hash: Blake3Hash::digest(text.as_bytes()),
            })],
            contributors: vec![],
            rank_contributions: vec![],
            support_group: Some(self.document.hash.clone()),
        }
    }

    fn unit(&self, origin: ByteSpan) -> RenderedUnit {
        let utf8 = format!(
            "Original embedding input: {}",
            origin.slice(&self.document.raw_text).unwrap()
        );
        RenderedUnit {
            unit_id: Blake3Hash::digest(format!("unit:{}:{}", origin.start(), origin.end())),
            owner: self.document.path.clone(),
            target: TargetKind::Document,
            target_id: self.document.owner_revision.clone(),
            source_hash: self.document.hash.clone(),
            source_span: Some(origin),
            dependency_fingerprint: Blake3Hash::digest("synthetic dependency"),
            input_hash: Blake3Hash::digest(utf8.as_bytes()),
            utf8,
        }
    }

    fn packet(
        &self,
        key: &str,
        origin: ByteSpan,
        core: ByteSpan,
        parent: ByteSpan,
        cosine: f64,
    ) -> Packet {
        Packet {
            passages: vec![self.passage(parent)],
            bundle: None,
            navigation: None,
            key: key.into(),
            score: cosine,
            selection_ordinal: None,
            selection: None,
            unit_score: Some(cosine),
            unit_origin: Some((origin, cosine)),
            fallback: Some(self.passage(core)),
            unit_clipped: core != origin,
        }
    }
}

fn span(start: usize, end: usize) -> ByteSpan {
    ByteSpan::new(start as u64, end as u64).unwrap()
}
fn request() -> ContextRequest {
    let mut request = ContextRequest::default();
    request.scope = super::super::context_types::ContextScope::IndexedDocuments;
    request.documents.limits.excerpt_bytes = 128;
    request
}
fn state() -> vectors::SpaceState {
    let spec = SpaceSpec {
        version: 1,
        endpoint_fingerprint: Blake3Hash::digest("synthetic endpoint"),
        profile_id: "synthetic".into(),
        service_id: "synthetic".into(),
        model: "synthetic-two-dimensional".into(),
        revision: None,
        dimensions: Some(2),
        metric: "cosine".into(),
        normalization: "float64-l2-to-f32-le-v1".into(),
        render_version: RENDER_VERSION.into(),
        settings: EmbeddingSettings::default(),
    };
    vectors::SpaceState {
        id: spec.id().unwrap(),
        spec,
        actual_dimensions: Some(2),
        active: true,
    }
}
fn inputs(arm: Arm) -> Inputs {
    Inputs::new(arm, Instant::now() + Duration::from_secs(30), &state()).unwrap()
}
fn assert_quote(passage: &ContextPassage, fixture: &Fixture) {
    assert_eq!(
        passage.text,
        passage.span.slice(&fixture.document.raw_text).unwrap()
    );
    assert_eq!(passage.locator.observed_hash, fixture.document.hash);
    assert_eq!(passage.citations.len(), 1);
    let CitationRef::Source(reference) = &passage.citations[0] else {
        panic!("expected a direct-source citation");
    };
    assert_eq!(
        reference.source_id,
        fixture.document.source_id.clone().unwrap()
    );
    assert_eq!(
        reference.source_revision,
        fixture.document.owner_revision.clone().unwrap()
    );
    assert_eq!(reference.span, passage.span);
    assert_eq!(
        reference.quote_hash,
        Blake3Hash::digest(passage.text.as_bytes())
    );
}

#[test]
fn original_vector_binding_survives_actual_focus_and_distinct_parent_expansion() {
    let broad = format!(
        "{} needle café 東京 {}",
        "ordinary ".repeat(80),
        "ordinary ".repeat(20)
    );
    let raw = format!(
        "# Broad\n\n{broad}\n\n# Other\n\nThe amber permit authorizes the second operation.\n"
    );
    let fixture = Fixture::new(&raw);
    let broad_start = raw.find(&broad).unwrap();
    let broad_origin = span(broad_start, broad_start + broad.len());
    let amber_start = raw.find("amber").unwrap();
    let amber_origin = span(amber_start, amber_start + "amber".len());
    let cues = [(broad_origin, 0.8), (amber_origin, 0.7)]
        .into_iter()
        .map(|(span, cosine)| ContextSemanticCue {
            owner: fixture.document.path.clone(),
            observed_hash: fixture.document.hash.clone(),
            span,
            cosine,
        })
        .collect::<Vec<_>>();
    let selected = select_units(
        &fixture.reader,
        "needle",
        &[UnitDocument {
            owner_index: 0,
            document: &fixture.document,
        }],
        &cues,
        128,
        80,
    )
    .unwrap();
    assert_eq!(selected.candidates.len(), 2);
    let focused = selected
        .candidates
        .iter()
        .find(|candidate| candidate.origin_span == broad_origin)
        .unwrap();
    assert_ne!(focused.child_span, focused.origin_span);
    assert_eq!(focused.child_span, focused.parent_span);
    let expanded = selected
        .candidates
        .iter()
        .find(|candidate| candidate.origin_span == amber_origin)
        .unwrap();
    assert_ne!(expanded.child_span, expanded.parent_span);
    let units = [fixture.unit(broad_origin), fixture.unit(amber_origin)];
    let mut input = inputs(Arm::F0);
    input.retain(&units[0], 0.8, vec![1.0, 0.0]).unwrap();
    input.retain(&units[1], 0.7, vec![0.0, 1.0]).unwrap();
    let packets = selected
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            fixture.packet(
                &format!("origin-{index}"),
                candidate.origin_span,
                candidate.child_span,
                candidate.parent_span,
                candidate.origin_cosine,
            )
        })
        .collect::<Vec<_>>();
    let (result, summary) =
        with_summary_for_test(|| allocate(&fixture.reader, &request(), &packets, "needle", input));
    let Outcome::Selected {
        choices,
        state: Some(trial),
        ..
    } = result.unwrap()
    else {
        panic!("both orthogonal original vectors should be admitted");
    };
    assert_eq!(choices.len(), 2);
    assert_eq!(
        choices
            .iter()
            .filter(|choice| choice.variant == Variant::Parent)
            .count(),
        1
    );
    assert_eq!(
        choices
            .iter()
            .map(|choice| choice.origin)
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    assert_eq!(trial.passages.len(), 2);
    assert_eq!(trial.reason, None);
    for passage in &trial.passages {
        assert_quote(passage, &fixture);
    }
    let summary = summary.unwrap();
    let identities = summary["selection"]["selected_origins"].as_array().unwrap();
    for unit in &units {
        let identity = identities
            .iter()
            .find(|identity| identity["unit_id"] == serde_json::to_value(&unit.unit_id).unwrap())
            .unwrap();
        assert_eq!(
            identity["input_hash"],
            serde_json::to_value(&unit.input_hash).unwrap()
        );
        assert_eq!(
            identity["source_span"],
            serde_json::to_value(unit.source_span.unwrap()).unwrap()
        );
    }
    assert_eq!(summary["selection"]["pair_cosines"], 1);
}

#[test]
fn duplicate_representations_dedup_only_when_every_retained_field_matches() {
    let fixture = Fixture::new("alpha beta gamma delta\n");
    let origin = span(0, 10);
    let packets = [fixture.packet("origin", origin, origin, origin, 0.8)];
    let mut input = inputs(Arm::F0);
    input
        .retain(&fixture.unit(origin), 0.8, vec![1.0, 0.0])
        .unwrap();
    input
        .retain(&fixture.unit(origin), 0.8, vec![1.0, 0.0])
        .unwrap();
    let Outcome::Selected { choices, .. } =
        allocate(&fixture.reader, &request(), &packets, "alpha", input).unwrap()
    else {
        panic!("identical retained duplicate should not create another vote");
    };
    assert_eq!(choices.len(), 1);
    for changed_field in 0..6 {
        let mut input = inputs(Arm::F0);
        input
            .retain(&fixture.unit(origin), 0.8, vec![1.0, 0.0])
            .unwrap();
        input
            .retain(&fixture.unit(origin), 0.8, vec![1.0, 0.0])
            .unwrap();
        let duplicate = &mut input.representations[1];
        match changed_field {
            0 => duplicate.unit_id = Blake3Hash::digest("conflicting unit identity"),
            1 => duplicate.input_hash = Blake3Hash::digest("conflicting input identity"),
            2 => duplicate.vector = Some(vec![0.0, 1.0]),
            3 => duplicate.cosine = 0.7,
            4 => duplicate.source_hash = Blake3Hash::digest("conflicting source hash"),
            5 => duplicate.space = Blake3Hash::digest("conflicting Space"),
            _ => unreachable!(),
        }
        assert!(
            allocate(&fixture.reader, &request(), &packets, "alpha", input).is_err(),
            "conflicting retained field {changed_field} must reject the whole allocation"
        );
    }
}

#[test]
fn proposal_binding_rejects_changed_hash_cosine_space_and_lost_original_span() {
    let fixture = Fixture::new("alpha beta gamma delta\n");
    let origin = span(0, 10);
    for changed_field in 0..4 {
        let mut input = inputs(Arm::F1);
        input
            .retain(&fixture.unit(origin), 0.8, vec![1.0, 0.0])
            .unwrap();
        let mut packet = fixture.packet("origin", origin, span(1, 9), span(0, 20), 0.8);
        match changed_field {
            0 => packet.passages[0].locator.observed_hash = Blake3Hash::digest("stale proposal"),
            1 => packet.unit_origin = Some((origin, 0.7)),
            2 => input.representations[0].space = Blake3Hash::digest("another Space"),
            3 => packet.unit_origin = Some((span(1, 9), 0.8)),
            _ => unreachable!(),
        }
        assert!(
            allocate(&fixture.reader, &request(), &[packet], "alpha", input).is_err(),
            "proposal binding mismatch {changed_field} must reject"
        );
    }
}

#[test]
fn exchange_reconstruction_preserves_the_overlapping_surviving_origin() {
    // Equal lexical weights create a deterministic admission/exchange path.
    // Actual canonical rendering, not a synthetic cardinality cap, admits it.
    let raw = format!(
        "{}alpha     beta      gamma     {}alpha     delta     ",
        " ".repeat(100),
        " ".repeat(10)
    );
    let fixture = Fixture::new(&raw);
    let passages = [
        fixture.passage(span(100, 120)),
        fixture.passage(span(110, 130)),
        fixture.passage(span(140, 160)),
    ];
    let mut request = request();
    let replacement = context::document_trial(
        &fixture.reader,
        &request,
        &[],
        &[passages[1].clone(), passages[2].clone()],
    )
    .unwrap();
    request.budget.max_bytes = replacement.text.len();
    request.budget.max_tokens = replacement.text.len().div_ceil(4);
    let original = context::document_trial(&fixture.reader, &request, &[], &passages[..2]).unwrap();
    assert_eq!(original.passages.len(), 1);
    assert_eq!(original.passages[0].span, span(100, 130));
    assert_eq!(original.reason, None);
    assert!(
        context::document_trial(&fixture.reader, &request, &[], &passages)
            .unwrap()
            .reason
            .is_some()
    );
    let candidates = ["a", "b", "c"].map(|stable_key| Candidate {
        stable_key,
        relevance: 1.0,
        has_parent: false,
    });
    let terms = [[0b0011, 0, 0, 0], [0b0110, 0, 0, 0], [0b1001, 0, 0, 0]];
    let outcome = sets::select(
        &candidates,
        Objective::Lexical {
            term_weights: &[1.0; 4],
            core_terms: &terms,
        },
        Arm::L,
        || Ok::<_, WikiError>(()),
        |choices| {
            let proposals = choices
                .iter()
                .map(|choice| passages[choice.origin].clone())
                .collect::<Vec<_>>();
            let trial = context::document_trial(&fixture.reader, &request, &[], &proposals)?;
            Ok(trial.reason.is_none().then_some(trial))
        },
    )
    .unwrap();
    let Outcome::Selected {
        choices,
        state: Some(trial),
        decisions,
        ..
    } = outcome
    else {
        panic!("expected a selected replacement");
    };
    assert!(
        decisions
            .iter()
            .any(|decision| decision.phase == sets::Phase::Exchange
                && decision.origin == 0
                && decision.reason == sets::Reason::RemovedByExchange)
    );
    assert_eq!(
        choices
            .iter()
            .map(|choice| choice.origin)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(trial.reason, None);
    assert_eq!(trial.passages.len(), 2);
    assert_eq!(trial.passages[0].span, passages[1].span);
    assert_eq!(trial.passages[0].text, passages[1].text);
    assert!(
        !trial
            .passages
            .iter()
            .any(|passage| passage.span.start() == 100)
    );
    for passage in &trial.passages {
        assert_quote(passage, &fixture);
    }
}

#[test]
fn late_overlap_bridge_coalesces_exact_bytes_and_rebinds_one_citation() {
    let fixture = Fixture::new("αβγ café 東京 evidence bridge retains canonical text.\n");
    let raw = &fixture.document.raw_text;
    let left_end = raw.find("evidence").unwrap() + "evidence".len();
    let right_start = raw.find("retains").unwrap();
    let bridge_start = raw.find("東京").unwrap();
    let bridge_end = right_start + "retains".len();
    let passages = [
        fixture.passage(span(0, left_end)),
        fixture.passage(span(right_start, raw.len())),
        fixture.passage(span(bridge_start, bridge_end)),
    ];
    let trial = context::document_trial(&fixture.reader, &request(), &[], &passages).unwrap();
    assert_eq!(trial.reason, None);
    assert_eq!(trial.passages.len(), 1);
    assert_eq!(trial.passages[0].span, span(0, raw.len()));
    assert_eq!(trial.passages[0].text, *raw);
    assert_eq!(trial.counts.values().copied().collect::<Vec<_>>(), vec![1]);
    assert_quote(&trial.passages[0], &fixture);
    assert_eq!(trial.text.matches("Citation (").count(), 1);
    assert!(trial.text.contains(raw));
}

#[test]
fn canonical_admission_applies_merged_excerpt_and_four_passage_owner_caps() {
    let fixture = Fixture::new("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789\n");
    let mut request = request();
    request.documents.limits.excerpt_bytes = 15;
    let overlapping = [fixture.passage(span(0, 15)), fixture.passage(span(10, 25))];
    let trial = context::document_trial(&fixture.reader, &request, &[], &overlapping).unwrap();
    assert_eq!(trial.passages.len(), 1);
    assert_eq!(trial.reason, Some("merged_passage_exceeds_excerpt_bound"));
    let disjoint = (0..5)
        .map(|index| fixture.passage(span(index * 10, index * 10 + 5)))
        .collect::<Vec<_>>();
    let four = context::document_trial(&fixture.reader, &request, &[], &disjoint[..4]).unwrap();
    assert_eq!(four.reason, None);
    assert_eq!(four.counts.values().copied().collect::<Vec<_>>(), vec![4]);
    let five = context::document_trial(&fixture.reader, &request, &[], &disjoint).unwrap();
    assert_eq!(five.passages.len(), 5);
    assert_eq!(five.reason, Some("document_passage_cap"));
}

#[test]
fn byte_token_and_reservation_caps_use_the_exact_citation_rendering() {
    let fixture = Fixture::new("canonical citation text\n");
    let passage = fixture.passage(span(0, fixture.document.raw_text.len()));
    let mut request = request();
    let initial = context::document_trial(
        &fixture.reader,
        &request,
        &[],
        std::slice::from_ref(&passage),
    )
    .unwrap();
    let bytes = initial.text.len();
    let tokens = bytes.div_ceil(4);
    request.budget.instruction_bytes = 17;
    request.budget.output_bytes = 23;
    request.budget.instruction_tokens = 3;
    request.budget.output_tokens = 5;
    request.budget.max_bytes = bytes + 40;
    request.budget.max_tokens = tokens + 8;
    let fits = context::document_trial(
        &fixture.reader,
        &request,
        &[],
        std::slice::from_ref(&passage),
    )
    .unwrap();
    assert_eq!(fits.reason, None);
    assert_eq!(fits.text, initial.text);
    request.budget.max_bytes -= 1;
    assert_eq!(
        context::document_trial(
            &fixture.reader,
            &request,
            &[],
            std::slice::from_ref(&passage)
        )
        .unwrap()
        .reason,
        Some("required_bundle_or_passage_does_not_fit")
    );
    request.budget.max_bytes += 1;
    request.budget.max_tokens -= 1;
    assert_eq!(
        context::document_trial(
            &fixture.reader,
            &request,
            &[],
            std::slice::from_ref(&passage)
        )
        .unwrap()
        .reason,
        Some("required_bundle_or_passage_does_not_fit")
    );
    request.budget.max_tokens = tokens + 8;
    request.budget.output_bytes = request.budget.max_bytes;
    assert!(
        context::document_trial(
            &fixture.reader,
            &request,
            &[],
            std::slice::from_ref(&passage)
        )
        .is_err()
    );
    request.budget.output_bytes = 23;
    request.budget.output_tokens = request.budget.max_tokens;
    assert!(context::document_trial(&fixture.reader, &request, &[], &[passage]).is_err());
}

#[test]
fn expired_shared_absolute_deadline_aborts_without_rendering_a_partial_packet() {
    let fixture = Fixture::new("alpha retained origin\n");
    let origin = span(0, 10);
    let packet = fixture.packet("origin", origin, origin, origin, 0.8);
    for arm in [Arm::L, Arm::F0, Arm::F1] {
        let mut input = inputs(arm);
        input
            .retain(&fixture.unit(origin), 0.8, vec![1.0, 0.0])
            .unwrap();
        input.deadline = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        let (result, counts) = context::with_context_render_counts_for_test(1, || {
            allocate(
                &fixture.reader,
                &request(),
                std::slice::from_ref(&packet),
                "alpha",
                input,
            )
        });
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("expired deadline must not return a partial selection"),
        };
        assert_eq!(error.code, ErrorCode::BudgetExceeded);
        assert_eq!(counts.calls, 0);
        assert_eq!(counts.bytes, 0);
    }
    assert_eq!(
        Inputs::new(Arm::F0, Instant::now(), &state())
            .err()
            .unwrap()
            .code,
        ErrorCode::BudgetExceeded
    );
}
