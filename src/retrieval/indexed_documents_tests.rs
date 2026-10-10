//! Normalized host selection keeps the same closed selected canonical proof.
#[path = "indexed_documents_location_experiment.rs"]
mod location_experiment;
use super::*;
use crate::{
    changes::ChangeDraft,
    records::{edit_note, parse_note},
    retrieval::{
        SearchMode,
        context_selection_packet::{
            SelectionAction, SelectionCard, SelectionPacket, SelectionReply,
        },
    },
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore, revision::record_bytes},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc, time::Duration};

const QUERY: &str = "selectionprobe";
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn note(kind: &str, name: &str, extra: Value, body: &[u8]) -> Vec<u8> {
    let mut fields = BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_id".into(), json!(name)),
        ("wiki_kind".into(), json!(kind)),
        ("title".into(), json!(name)),
    ]);
    fields.extend(
        extra
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    record_bytes(CanonicalRecord::new(fields).unwrap(), body).unwrap()
}
fn request() -> ContextRequest {
    let mut request = ContextRequest {
        scope: ContextScope::IndexedDocuments,
        ..Default::default()
    };
    request.documents.limits.hits = 5;
    request.documents.limits.candidates = 80;
    request.documents.limits.excerpt_bytes = 1024;
    request.budget.max_bytes = 6000;
    request.budget.max_tokens = 1500;
    request.verification_budget.max_elapsed_ms = 30_000;
    request
}
fn capture(body: &[u8]) -> CaptureRequest {
    CaptureRequest {
        title: "Captured exception".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "fixture.txt".into(),
        original: body.to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: Some("text/plain".into()),
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    catalog: Catalog,
    source: RecordId,
    content: VaultRelativePath,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            note("vault", "vault_selected_host", json!({}), b""),
        )
        .unwrap();
        let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let body =
            "selectionprobe: Café 東京 captured exception requires the violet permit.\n".as_bytes();
        let plan = SourceStore::new(handle.clone())
            .plan_capture(capture(body))
            .unwrap();
        let source = plan.source_id.clone();
        let revision = plan.revision_id.clone();
        let content = path(&format!("sources/{source}/revisions/{revision}/content.md"));
        Self::seed(&handle, plan.draft.unwrap());
        for (file,bytes) in [
            ("entity.md",note("entity","entity_host",json!({"wiki_status":"active","wiki_entity_type":"component"}),b"")),
            ("claim.md",note("assertion","assertion_host",json!({"wiki_status":"accepted","wiki_subject_id":"entity_host","wiki_object_id":"entity_host","wiki_predicate":"uses"}),b"Fixture support.")),
            ("decision.md",note("decision","decision_host",json!({"wiki_status":"active","wiki_action":"accept","wiki_created_at":"2026-10-04T00:00:00Z","wiki_input_ids":["assertion_host"],"wiki_output_ids":["assertion_host"]}),b"")),
            ("evidence.md",note("evidence","evidence_host",json!({"wiki_status":"active","wiki_assertion_id":"assertion_host","wiki_source_id":source,"wiki_source_revision":revision,"wiki_stance":"supports","wiki_locator_kind":"utf8-bytes","wiki_span_start":0,"wiki_span_end":body.len(),"wiki_quote_hash":Blake3Hash::digest(body)}),&crate::sources::evidence::exact_quote_body(body,"\n","Support").unwrap())),
            ("page.md",note("page","page_host",json!({"wiki_status":"reviewed","wiki_depends_on_ids":["assertion_host"]}),"# Authored setup\n\nselectionprobe: Café 東京 authored setup keeps exact prose.\n".as_bytes())),
            ("unselected.md",note("page","page_unselected_host",json!({"wiki_status":"reviewed"}),b"Unrelated orchard details.\n")),
        ] { fs::write(temp.path().join(file), bytes).unwrap(); }
        let catalog = Catalog::new(handle, id("vault_selected_host"));
        let writer = WriterPermit::acquire(catalog.fs().root(), Duration::from_secs(1)).unwrap();
        catalog.rebuild_normalized(&writer).unwrap();
        drop(writer);
        Self {
            _temp: temp,
            catalog,
            source,
            content,
        }
    }
    fn seed(handle: &VaultFs, draft: ChangeDraft) {
        for operation in draft.operations {
            let target = handle.root().path().join(operation.target.as_str());
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, operation.proposed.unwrap()).unwrap();
        }
    }
    fn prepare(&self) -> ContextResult {
        context(
            &self.catalog,
            QUERY,
            &request(),
            &ContextOptions {
                selection: SelectionAction::Prepare,
                ..Default::default()
            },
        )
        .unwrap()
    }
    fn reply(prepared: &ContextResult) -> SelectionReply {
        let packet = prepared.selection_packet().unwrap();
        let page = packet
            .cards
            .iter()
            .find(|card| card.passage.locator.path == path("page.md"))
            .unwrap();
        let source = packet
            .cards
            .iter()
            .find(|card| !card.passage.citations.is_empty())
            .unwrap();
        SelectionReply {
            packet_fingerprint: packet.fingerprint.clone(),
            ordered_ids: vec![page.id.clone(), source.id.clone()],
        }
    }
    fn apply(&self, reply: SelectionReply) -> Result<ContextResult> {
        context(
            &self.catalog,
            QUERY,
            &request(),
            &ContextOptions {
                selection: SelectionAction::Apply(reply),
                ..Default::default()
            },
        )
    }
    fn edit(&self, file: &str, field: &str, value: Value) {
        let target = self.catalog.fs().root().path().join(file);
        let parsed = parse_note(&fs::read(&target).unwrap());
        let bytes = edit_note(
            &parsed,
            &BTreeMap::from([(field.into(), value)]),
            None,
            &parsed.source_hash,
        )
        .unwrap();
        fs::write(target, bytes).unwrap();
    }
    fn republish(&self) {
        let writer =
            WriterPermit::acquire(self.catalog.fs().root(), Duration::from_secs(1)).unwrap();
        self.catalog.sync_normalized(&writer).unwrap();
    }
}

#[test]
fn mixed_authored_and_captured_prepare_apply_preserves_exact_selected_evidence() {
    let fixture = Fixture::new();
    let page_before = fs::read(fixture.catalog.fs().root().path().join("page.md")).unwrap();
    let source_before = fs::read(
        fixture
            .catalog
            .fs()
            .root()
            .path()
            .join(fixture.content.as_str()),
    )
    .unwrap();
    let prepared = fixture.prepare();
    assert!(prepared.text().is_empty());
    assert!(prepared.passages().is_empty());
    assert!(!prepared.network_used);
    let packet = prepared.selection_packet().unwrap();
    assert!(
        packet
            .cards
            .iter()
            .any(|card| card.passage.locator.path == path("page.md"))
    );
    assert!(
        packet
            .cards
            .iter()
            .any(|card| !card.passage.citations.is_empty())
    );
    assert!(packet.cards.len() <= 80 && packet.input_bytes <= 130_048);
    let repeated = fixture.prepare();
    assert_eq!(prepared.selection_packet(), repeated.selection_packet());
    let reply = Fixture::reply(&prepared);
    let selected_ids = reply.ordered_ids.clone();
    let selected = fixture.apply(reply).unwrap();
    assert_eq!(selected.passages().len(), 2);
    assert!(selected.text().contains("Café 東京 authored setup"));
    assert!(selected.text().contains("violet permit"));
    assert!(selected.usage().rendered_bytes <= 6000 && selected.usage().estimated_tokens <= 1500);
    assert!(selected.usage().verification_files > 0);
    assert!(!selected.network_used);
    for (passage, selected_id) in selected.passages().iter().zip(selected_ids) {
        let card = packet
            .cards
            .iter()
            .find(|card| card.id == selected_id)
            .unwrap();
        assert_eq!(passage.text, card.passage.text);
        assert_eq!(passage.span, card.passage.span);
        assert_eq!(passage.locator, card.passage.locator);
        assert_eq!(passage.citations, card.passage.citations);
        let original = fs::read(
            fixture
                .catalog
                .fs()
                .root()
                .path()
                .join(passage.locator.path.as_str()),
        )
        .unwrap();
        assert_eq!(
            passage.text.as_bytes(),
            &original[passage.span.start() as usize..passage.span.end() as usize]
        );
        for citation in &passage.citations {
            let CitationRef::Source(reference) = citation else {
                panic!("captured source citation")
            };
            assert_eq!(reference.source_id, fixture.source);
            assert_eq!(reference.span, passage.span);
            assert_eq!(
                reference.quote_hash,
                Blake3Hash::digest(passage.text.as_bytes())
            );
        }
    }
    assert_eq!(
        fs::read(fixture.catalog.fs().root().path().join("page.md")).unwrap(),
        page_before
    );
    assert_eq!(
        fs::read(
            fixture
                .catalog
                .fs()
                .root()
                .path()
                .join(fixture.content.as_str())
        )
        .unwrap(),
        source_before
    );
    assert!(
        matches!(selected.verification(),SnapshotVerification::IndexedEvidence {evidence_domain,global_membership_verified:false,..} if evidence_domain=="selected_documents")
    );
}

#[test]
fn reply_binds_query_request_and_exact_supplied_ids() {
    let fixture = Fixture::new();
    let prepared = fixture.prepare();
    let reply = Fixture::reply(&prepared);
    let options = ContextOptions {
        selection: SelectionAction::Apply(reply.clone()),
        ..Default::default()
    };
    assert_eq!(
        context(
            &fixture.catalog,
            "selectionprobe setup",
            &request(),
            &options
        )
        .unwrap_err()
        .code,
        ErrorCode::FreshnessConflict
    );
    let mut changed = request();
    changed.budget.max_bytes = 5900;
    assert_eq!(
        context(&fixture.catalog, QUERY, &changed, &options)
            .unwrap_err()
            .code,
        ErrorCode::FreshnessConflict
    );
    let mut unknown = reply.clone();
    unknown.ordered_ids = vec!["missing_card".into()];
    assert_eq!(fixture.apply(unknown).unwrap_err().code, ErrorCode::Usage);
    let mut duplicate = reply;
    duplicate.ordered_ids = vec![duplicate.ordered_ids[0].clone(); 2];
    assert_eq!(fixture.apply(duplicate).unwrap_err().code, ErrorCode::Usage);
}

#[test]
fn source_and_selected_eligibility_tamper_reject_old_selection_before_emission() {
    for target in ["page.md", "evidence.md", "claim.md", "WIKI.md", "content"] {
        let fixture = Fixture::new();
        let reply = Fixture::reply(&fixture.prepare());
        let target = if target == "content" {
            fixture.content.clone()
        } else {
            path(target)
        };
        let full = fixture.catalog.fs().root().path().join(target.as_str());
        let mut bytes = fs::read(&full).unwrap();
        bytes.extend(b"Preserve this external edit.\n");
        fs::write(&full, &bytes).unwrap();
        assert_eq!(
            fixture.apply(reply).unwrap_err().code,
            ErrorCode::FreshnessConflict,
            "target {target}"
        );
        assert_eq!(fs::read(&full).unwrap(), bytes);
    }
}

struct EditBeforeEmission {
    path: PathBuf,
    bytes: Vec<u8>,
}
impl ContextFault for EditBeforeEmission {
    fn check(&self, checkpoint: ContextCheckpoint) -> Result<()> {
        assert_eq!(
            checkpoint,
            ContextCheckpoint::BeforeFinalVerification { attempt: 0 }
        );
        fs::write(&self.path, &self.bytes).unwrap();
        Ok(())
    }
}

#[test]
fn final_selected_proof_recheck_covers_prepare_and_apply_dependencies() {
    for apply in [false, true] {
        let fixture = Fixture::new();
        let prepared = fixture.prepare();
        let target = fixture.catalog.fs().root().path().join("decision.md");
        let original = fs::read(&target).unwrap();
        let changed = String::from_utf8(original)
            .unwrap()
            .replace("\"accept\"", "\"reject\"")
            .into_bytes();
        let options = ContextOptions {
            selection: if apply {
                SelectionAction::Apply(Fixture::reply(&prepared))
            } else {
                SelectionAction::Prepare
            },
            fault: Some(Arc::new(EditBeforeEmission {
                path: target.clone(),
                bytes: changed.clone(),
            })),
        };
        assert_eq!(
            context(&fixture.catalog, QUERY, &request(), &options)
                .unwrap_err()
                .code,
            ErrorCode::FreshnessConflict
        );
        assert_eq!(fs::read(target).unwrap(), changed);
    }
}

#[test]
fn selected_adapter_cannot_fall_back_and_unselected_growth_does_not_change_packet() {
    let fixture = Fixture::new();
    let prepared = fixture.prepare();
    let reader = fixture
        .catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let proof = selected_documents::authenticate(
        &fixture.catalog,
        &reader,
        &[path("page.md"), fixture.content.clone()],
        &request().verification_budget,
    )
    .unwrap();
    let selected = SelectedCatalog {
        reader: &reader,
        proof: &proof,
    };
    let before = reader.usage();
    assert!(
        selected
            .record(&id("page_unselected_host"))
            .unwrap()
            .is_none()
    );
    assert!(selected.document(&path("unselected.md")).unwrap().is_none());
    assert_eq!(
        selected.diagnostics(&BTreeSet::new()).unwrap_err().code,
        ErrorCode::FreshnessConflict
    );
    let decode = reader
        .connection()
        .query_row("SELECT 1", [], |row| Ok(selected.decode_document(row, 0)))
        .unwrap();
    assert_eq!(decode.unwrap_err().code, ErrorCode::FreshnessConflict);
    assert_eq!(reader.usage().rows, before.rows);
    assert_eq!(reader.usage().bytes, before.bytes);
    drop(proof);
    drop(reader);
    fs::write(
        fixture.catalog.fs().root().path().join("unselected.md"),
        b"Unpublished unrelated edited bytes.\n",
    )
    .unwrap();
    fs::write(
        fixture.catalog.fs().root().path().join("new-unselected.md"),
        b"New unrelated unpublished member.\n",
    )
    .unwrap();
    let repeated = fixture.prepare();
    assert_eq!(prepared.selection_packet(), repeated.selection_packet());
    fixture.apply(Fixture::reply(&prepared)).unwrap();
}

#[test]
fn refreshed_withdrawn_or_republished_page_invalidates_old_bound_reply() {
    for mutation in ["refresh", "withdraw", "page"] {
        let fixture = Fixture::new();
        let reply = Fixture::reply(&fixture.prepare());
        let store = SourceStore::new(fixture.catalog.fs().clone());
        match mutation {
            "refresh" => {
                let plan = store
                    .plan_refresh(
                        &fixture.source,
                        capture(b"selectionprobe: changed captured exception.\n"),
                    )
                    .unwrap();
                Fixture::seed(fixture.catalog.fs(), plan.draft.unwrap());
            }
            "withdraw" => {
                let plan = store
                    .plan_withdraw(&fixture.source, "Superseded by author")
                    .unwrap();
                Fixture::seed(fixture.catalog.fs(), plan.draft.unwrap());
            }
            "page" => fixture.edit("page.md", "title", json!("Changed selected authored title")),
            _ => unreachable!(),
        }
        fixture.republish();
        assert_eq!(
            fixture.apply(reply).unwrap_err().code,
            ErrorCode::FreshnessConflict,
            "{mutation}"
        );
    }
}

#[test]
fn preparation_and_selection_share_the_existing_finite_proof_budget() {
    let fixture = Fixture::new();
    let prepared = fixture.prepare();
    let mut exhausted = request();
    exhausted.verification_budget.max_files = 1;
    for selection in [
        SelectionAction::Prepare,
        SelectionAction::Apply(Fixture::reply(&prepared)),
    ] {
        let options = ContextOptions {
            selection,
            ..Default::default()
        };
        assert_eq!(
            context(&fixture.catalog, QUERY, &exhausted, &options)
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PilotCase {
    id: String,
    query: String,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TracePassage {
    locator: DocumentLocator,
    text: String,
    span: ByteSpan,
    label: crate::retrieval::ExcerptLabel,
    eligibility: Eligibility,
    citations: Vec<CitationRef>,
    contributors: Vec<Value>,
    rank_contributions: Vec<crate::retrieval::RankContribution>,
    support_group: Option<Blake3Hash>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceCard {
    id: String,
    title: String,
    passage: TracePassage,
    child_span: Option<ByteSpan>,
    rendered_bytes: usize,
}
impl TraceCard {
    fn into_card(self) -> SelectionCard {
        assert!(
            self.passage.contributors.is_empty(),
            "lexical document cards have no graph contributors"
        );
        SelectionCard {
            id: self.id,
            title: self.title,
            child_span: self.child_span,
            rendered_bytes: self.rendered_bytes,
            passage: ContextPassage {
                locator: self.passage.locator,
                text: self.passage.text,
                span: self.passage.span,
                label: self.passage.label,
                eligibility: self.passage.eligibility,
                citations: self.passage.citations,
                contributors: Vec::new(),
                rank_contributions: self.passage.rank_contributions,
                support_group: self.passage.support_group,
            },
        }
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceCandidate {
    owner_index: usize,
    span: ByteSpan,
    covered_terms: Vec<usize>,
    local_relevance: u64,
    seed_overlap: bool,
    clipped: bool,
    semantic_affinity: Option<f64>,
}
impl TraceCandidate {
    fn into_candidate(self) -> crate::retrieval::context_selection::SelectionCandidate {
        crate::retrieval::context_selection::SelectionCandidate {
            owner_index: self.owner_index,
            span: self.span,
            covered_terms: self.covered_terms,
            local_relevance: self.local_relevance,
            seed_overlap: self.seed_overlap,
            clipped: self.clipped,
            semantic_affinity: self.semantic_affinity,
        }
    }
}
fn trace_stage<'a>(trace: &'a [Value], stage: &str) -> &'a Value {
    &trace
        .iter()
        .find(|value| value["stage"] == stage)
        .expect("required ordering trace stage")["rows"]
}
fn trace_cards(trace: &[Value]) -> Vec<SelectionCard> {
    trace_stage(trace, "pre_card_cap")
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            serde_json::from_value::<TraceCard>(row["authenticated_card"].clone())
                .unwrap()
                .into_card()
        })
        .collect()
}
fn legacy_same_pool_packet(packet: &SelectionPacket, trace: &[Value]) -> SelectionPacket {
    let cards = trace_cards(trace)
        .into_iter()
        .map(|card| (card.id.clone(), card))
        .collect::<BTreeMap<_, _>>();
    let sorted = trace_stage(trace, "sorted_packets").as_array().unwrap();
    let by_key = sorted
        .iter()
        .enumerate()
        .map(|(index, row)| {
            (
                row["key"].as_str().unwrap(),
                cards[&format!("c{index:04}")].clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let old = trace_stage(trace, "candidate_pool")["legacy_order"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, key)| {
            let mut card = by_key[key.as_str().unwrap()].clone();
            card.id = format!("c{index:04}");
            card
        })
        .collect();
    let task: Value = serde_json::from_str(&packet.selector_input).unwrap();
    crate::retrieval::context_selection_packet::build_packet(
        task["payload"]["binding"].clone(),
        crate::retrieval::context_selection_packet::interleave_by_owner(old),
    )
    .unwrap()
}
fn same_packet_automatic(
    catalog: &Catalog,
    query: &str,
    request: &ContextRequest,
    packet: &SelectionPacket,
    trace: &[Value],
) -> Result<ContextResult> {
    // This is the existing deterministic utility policy restricted to the exact
    // displayed cards, not a new public mode or a different scoring formula.
    let meter = Meter::new(&request.verification_budget);
    let request = context::validate_request(query, request)?;
    context::validate_selection_action(&request, &SelectionAction::Prepare)?;
    meter.check()?;
    catalog.guard_query()?;
    meter.check()?;
    let reader = catalog.cached_query_snapshot(QueryReadLimits {
        max_elapsed_ms: meter.remaining_ms(),
        ..QueryReadLimits::default()
    })?;
    let mut hits = lexical::search_context_catalog(&reader, query, &request.documents, false)?;
    meter.check()?;
    let paths = hits
        .hits
        .iter()
        .map(|hit| hit.locator.path.clone())
        .collect::<Vec<_>>();
    let mut budget = request.verification_budget.clone();
    budget.max_elapsed_ms = meter.remaining_ms();
    let mut proof = selected_documents::authenticate(catalog, &reader, &paths, &budget)?;
    hits.dependency_fingerprint = proof.fingerprint.clone();
    let selected = SelectedCatalog {
        reader: &reader,
        proof: &proof,
    };
    let task: Value = serde_json::from_str(&packet.selector_input).unwrap();
    assert_eq!(task["payload"]["binding"]["query"], json!(query));
    assert_eq!(
        task["payload"]["binding"]["request"],
        serde_json::to_value(&request).unwrap()
    );
    assert_eq!(
        task["payload"]["binding"]["snapshot"],
        serde_json::to_value(reader.snapshot()).unwrap()
    );
    assert_eq!(
        task["payload"]["binding"]["dependency_fingerprint"],
        serde_json::to_value(&proof.fingerprint).unwrap()
    );
    let rows = trace_stage(trace, "sorted_packets").as_array().unwrap();
    let packets = packet
        .cards
        .iter()
        .map(|card| {
            let index = card.id.strip_prefix('c').unwrap().parse::<usize>().unwrap();
            let row = &rows[index];
            let candidate =
                serde_json::from_value::<TraceCandidate>(row["lexical_candidate"].clone())
                    .unwrap()
                    .into_candidate();
            assert_eq!(candidate.span, card.passage.span);
            context::Packet {
                passages: vec![card.passage.clone()],
                bundle: None,
                navigation: None,
                key: row["key"].as_str().unwrap().into(),
                score: row["score"].as_f64().unwrap(),
                selection_ordinal: row["ordinal"].as_u64().map(|ordinal| ordinal as usize),
                selection: Some(candidate),
                unit_score: None,
                unit_origin: None,
                fallback: None,
                unit_clipped: false,
            }
        })
        .collect();
    let term_weights = trace_stage(trace, "candidate_pool")["term_weights"]
        .as_array()
        .unwrap()
        .iter()
        .map(|weight| weight.as_u64().unwrap())
        .collect();
    let draft = context::pack(
        &selected,
        &request,
        context::PackingInput {
            packets,
            omissions: Vec::new(),
            term_weights,
            selection_warnings: vec![
                "evaluation-only deterministic control restricted to exactly the displayed packet"
                    .into(),
            ],
            source_aware: true,
            native_lexical_units: false,
            query: Some(query),
            signals: &ContextSelectionSignals::default(),
            selection_action: &SelectionAction::Automatic,
            hits: &hits,
            graph: None,
            dependency_fingerprint: proof.fingerprint.clone(),
            evidence_sets: None,
        },
    )?;
    proof.recheck(catalog, &reader)?;
    let verification = verification(&reader)?;
    meter.check()?;
    Ok(seal(draft, verification, proof.meter()))
}
fn bounded_json(path: &std::path::Path, maximum: usize) -> Value {
    assert!(
        fs::symlink_metadata(path).unwrap().is_file(),
        "regular explicit input required"
    );
    assert!(
        fs::metadata(path).unwrap().len() <= maximum as u64,
        "bounded input exceeded"
    );
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
#[ignore = "explicit six-case development vault/query/artifact environment required; no model calls"]
fn normalized_selection_six_case_development_lineage_pilot() {
    // This harness is opt-in and accepts only owned development locations. It
    // never finds questions automatically, opens a frozen set or grades labels.
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .unwrap();
    let owned = repo.join(".artifacts/context-ordering-001");
    fs::create_dir_all(&owned).unwrap();
    let owned = owned.canonicalize().unwrap();
    let development = repo
        .join(".artifacts/context-development-first-loss-001")
        .canonicalize()
        .unwrap();
    let vault = PathBuf::from(
        std::env::var("LWIKI_CONTEXT_PILOT_VAULT").expect("explicit owned normalized vault"),
    )
    .canonicalize()
    .unwrap();
    let queries = PathBuf::from(
        std::env::var("LWIKI_CONTEXT_PILOT_CASES").expect("explicit six-case query file"),
    )
    .canonicalize()
    .unwrap();
    let output = PathBuf::from(
        std::env::var("LWIKI_CONTEXT_PILOT_ARTIFACTS").expect("explicit fresh artifact directory"),
    );
    assert!(vault.starts_with(&owned) || vault.starts_with(&development));
    assert!(queries.starts_with(&owned));
    assert!(
        output
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .starts_with(&owned)
    );
    let cases: Vec<PilotCase> = serde_json::from_value(bounded_json(&queries, 32 * 1024)).unwrap();
    assert_eq!(cases.len(), 6, "finite six-case protocol");
    let allowed = bounded_json(&development.join("manifest.json"), 128 * 1024);
    let allowed_cases = allowed["cases"].as_array().unwrap();
    assert_eq!(allowed_cases.len(), 6);
    let mut names = BTreeSet::new();
    for case in &cases {
        assert!(
            case.id.len() <= 64
                && case
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        );
        assert!(names.insert(case.id.clone()), "unique development case");
        assert!(case.query.len() <= 4096);
        assert!(
            allowed_cases
                .iter()
                .any(|allowed| allowed["id"] == case.id && allowed["query"] == case.query),
            "case must match declared development manifest"
        );
    }
    let handle = VaultFs::new(VaultRoot::explicit(&vault).unwrap());
    let marker = parse_note(&fs::read(vault.join("WIKI.md")).unwrap());
    let catalog = Catalog::new(handle, marker.canonical.as_ref().unwrap().id().clone());
    assert!(
        catalog.operation_state().unwrap().is_some(),
        "explicit normalized activation required; harness does not rebuild"
    );
    fs::create_dir(&output).expect("artifact directory must be new; preserve earlier attempts");
    let mut request = request();
    request.verification_budget.max_elapsed_ms = 5000;
    request.verification_budget.max_entries = 65_536;
    let mut written = 0usize;
    let mut write = |case: &str, kind: &str, value: Value| {
        let bytes = serde_json::to_vec_pretty(&value).unwrap();
        assert!(
            bytes.len() <= 16 * 1024 * 1024,
            "bounded per-artifact bytes"
        );
        written += bytes.len();
        assert!(
            written <= 128 * 1024 * 1024,
            "bounded six-case artifact bytes"
        );
        fs::write(output.join(format!("{case}-{kind}.json")), bytes).unwrap();
    };
    for case in cases {
        let automatic_started = std::time::Instant::now();
        let automatic = context(&catalog, &case.query, &request, &ContextOptions::default());
        write(
            &case.id,
            "automatic",
            json!({"result":automatic,"elapsed_ms":automatic_started.elapsed().as_secs_f64()*1000.0}),
        );
        let prepare_started = std::time::Instant::now();
        let (prepared, trace) = context::with_candidate_ordering_trace(|| {
            context(
                &catalog,
                &case.query,
                &request,
                &ContextOptions {
                    selection: SelectionAction::Prepare,
                    ..Default::default()
                },
            )
        });
        write(
            &case.id,
            "prepare",
            json!({"result":prepared,"elapsed_ms":prepare_started.elapsed().as_secs_f64()*1000.0}),
        );
        write(
            &case.id,
            "lineage",
            json!({"trace":trace,"request":request,"query":case.query,
            "scope":"retained per-owner pool and old/restored ordering; filtered proposals and scan gaps are not traced; no quality grade or selector calls"}),
        );
        if let Ok(prepared) = prepared {
            let packet = prepared.selection_packet().unwrap();
            write(
                &case.id,
                "legacy-same-pool-packet",
                json!(legacy_same_pool_packet(packet, &trace)),
            );
            let same_packet_started = std::time::Instant::now();
            let same_packet =
                same_packet_automatic(&catalog, &case.query, &request, packet, &trace);
            write(
                &case.id,
                "same-packet-automatic",
                json!({"result":same_packet,"elapsed_ms":same_packet_started.elapsed().as_secs_f64()*1000.0,
                "scope":"B1 diagnostic: unchanged Automatic utility over exactly the displayed candidate set; harness preparation overhead is included"}),
            );
            let chosen = packet
                .cards
                .iter()
                .take(5)
                .map(|card| card.id.clone())
                .collect::<Vec<_>>();
            let reply = SelectionReply {
                packet_fingerprint: packet.fingerprint.clone(),
                ordered_ids: chosen,
            };
            let apply_started = std::time::Instant::now();
            let (applied, apply_trace) = context::with_candidate_ordering_trace(|| {
                context(
                    &catalog,
                    &case.query,
                    &request,
                    &ContextOptions {
                        selection: SelectionAction::Apply(reply.clone()),
                        ..Default::default()
                    },
                )
            });
            write(
                &case.id,
                "mechanical-apply",
                json!({"reply":reply,"result":applied,"trace":apply_trace,
                "elapsed_ms":apply_started.elapsed().as_secs_f64()*1000.0,
                "scope":"mechanical first-five-displayed-ID replay, not a fact-aware oracle, model selector or completeness claim"}),
            );
        }
    }
}

// One frozen development experiment. This code is in the cfg(test) leaf only;
// it neither changes public Automatic nor emits a second approximate renderer.
const COVERAGE_MAX_POOL: usize = 160;
const COVERAGE_MARKER: &str = "test_only_coverage_admission";
#[derive(Clone)]
struct IntervalCoverageCandidate {
    card: SelectionCard,
    lexical: crate::retrieval::context_selection::SelectionCandidate,
    key: String,
    ordinal: usize,
    owner: String,
    coordinate: String,
    relevance: f64,
    owner_score: f64,
    intervals: Vec<usize>,
}
struct CoverageInterval {
    coordinate: String,
    start: u64,
    end: u64,
}
struct IntervalCoveragePool {
    candidates: Vec<IntervalCoverageCandidate>,
    intervals: Vec<CoverageInterval>,
    term_weights: Vec<u64>,
}
#[derive(Default, serde::Serialize)]
struct CoverageDiagnostics {
    candidate_evaluations: usize,
    native_packet_calls: usize,
    full_pool_prevalidation_calls: usize,
    original_prepare_native_packet_calls: usize,
    trial_pack_calls: usize,
    exact_trial_output_bytes: u64,
    feasible_trials: usize,
    infeasible_trials: usize,
    zero_gain_candidates: usize,
    packing_errors: usize,
    internal_render_calls: Option<usize>,
    internal_render_bytes: Option<u64>,
    trials: Vec<Value>,
    choices: Vec<Value>,
    pool: Value,
}
impl CoverageDiagnostics {
    fn trial(&mut self, value: Value) {
        assert!(
            self.trials.len() < 13_000,
            "finite 160-candidate trial bound"
        );
        self.trials.push(value);
    }
}
fn coverage_relevance(
    score: f64,
    candidate: &crate::retrieval::context_selection::SelectionCandidate,
    weights: &[u64],
) -> f64 {
    assert!(score.is_finite() && score >= 0.0);
    assert!(
        candidate.semantic_affinity.is_none(),
        "frozen lexical experiment only"
    );
    let weight = weights.iter().copied().sum::<u64>().max(1) as f64;
    let terms = candidate
        .covered_terms
        .iter()
        .map(|&term| weights[term])
        .sum::<u64>() as f64
        / weight;
    let local = candidate.local_relevance as f64 / weight;
    score
        * (0.25 + 2.0 * terms + 0.5 * local + if candidate.seed_overlap { 0.05 } else { 0.0 })
        * if candidate.clipped { 0.7 } else { 1.0 }
}
fn coverage_coordinate(passage: &ContextPassage) -> String {
    // Includes exact path, record binding and observed full-document hash. A
    // captured path additionally contains its immutable revision identity.
    serde_json::to_string(&passage.locator).unwrap()
}
impl IntervalCoveragePool {
    fn from_trace(trace: &[Value]) -> Self {
        let cards = trace_cards(trace)
            .into_iter()
            .map(|card| (card.id.clone(), card))
            .collect::<BTreeMap<_, _>>();
        let weights = trace_stage(trace, "candidate_pool")["term_weights"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect::<Vec<_>>();
        let rows = trace_stage(trace, "sorted_packets").as_array().unwrap();
        assert!(
            rows.len() <= COVERAGE_MAX_POOL,
            "frozen five-owner lexical pool bound"
        );
        let mut candidates = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let card = cards[&format!("c{index:04}")].clone();
                let lexical =
                    serde_json::from_value::<TraceCandidate>(row["lexical_candidate"].clone())
                        .unwrap()
                        .into_candidate();
                assert_eq!(lexical.span, card.passage.span);
                assert_eq!(card.passage.text.len() as u64, lexical.span.len());
                assert!(lexical.span.len() > 0);
                assert!(
                    card.passage
                        .rank_contributions
                        .iter()
                        .all(|rank| rank.channel != COVERAGE_MARKER)
                );
                IntervalCoverageCandidate {
                    relevance: coverage_relevance(
                        row["score"].as_f64().unwrap(),
                        &lexical,
                        &weights,
                    ),
                    owner_score: row["score"].as_f64().unwrap(),
                    coordinate: coverage_coordinate(&card.passage),
                    owner: crate::retrieval::bundles::owner(&card.passage),
                    key: row["key"].as_str().unwrap().into(),
                    ordinal: row["ordinal"].as_u64().unwrap() as usize,
                    card,
                    lexical,
                    intervals: Vec::new(),
                }
            })
            .collect::<Vec<_>>();
        let mut boundaries = BTreeMap::<String, BTreeSet<u64>>::new();
        for candidate in &candidates {
            let ends = boundaries.entry(candidate.coordinate.clone()).or_default();
            ends.insert(candidate.lexical.span.start());
            ends.insert(candidate.lexical.span.end());
        }
        let intervals = boundaries
            .into_iter()
            .flat_map(|(coordinate, ends)| {
                let ends = ends.into_iter().collect::<Vec<_>>();
                ends.windows(2)
                    .map(|ends| CoverageInterval {
                        coordinate: coordinate.clone(),
                        start: ends[0],
                        end: ends[1],
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for candidate in &mut candidates {
            candidate.intervals = intervals
                .iter()
                .enumerate()
                .filter(|(_, interval)| {
                    interval.coordinate == candidate.coordinate
                        && candidate.lexical.span.start() <= interval.start
                        && interval.end <= candidate.lexical.span.end()
                })
                .map(|(index, _)| index)
                .collect();
        }
        Self {
            candidates,
            intervals,
            term_weights: weights,
        }
    }
    fn gain(&self, current: &[f64], candidate: usize) -> f64 {
        let candidate = &self.candidates[candidate];
        let density = candidate.relevance / candidate.lexical.span.len() as f64;
        candidate
            .intervals
            .iter()
            .map(|&index| {
                (density - current[index]).max(0.0)
                    * (self.intervals[index].end - self.intervals[index].start) as f64
            })
            .sum()
    }
    fn add(&self, current: &mut [f64], index: usize) {
        let candidate = &self.candidates[index];
        let density = candidate.relevance / candidate.lexical.span.len() as f64;
        for &interval in &candidate.intervals {
            current[interval] = current[interval].max(density);
        }
    }
    fn objective(&self, current: &[f64]) -> f64 {
        current
            .iter()
            .zip(&self.intervals)
            .map(|(density, interval)| density * (interval.end - interval.start) as f64)
            .sum()
    }
    fn tie_before(&self, left: usize, right: usize) -> bool {
        (self.candidates[left].ordinal, &self.candidates[left].key)
            < (self.candidates[right].ordinal, &self.candidates[right].key)
    }
    fn metadata(&self) -> Value {
        json!({"term_weights":self.term_weights,"interval_count":self.intervals.len(),
            "candidates":self.candidates.iter().map(|candidate| json!({
                "id":candidate.card.id,"key":candidate.key,"ordinal":candidate.ordinal,
                "owner":candidate.owner,"coordinate":candidate.coordinate,
                "covered_terms":candidate.lexical.covered_terms,"local_relevance":candidate.lexical.local_relevance,
                "seed_overlap":candidate.lexical.seed_overlap,"clipped":candidate.lexical.clipped,
                "packet_score":candidate.owner_score,"W":self.term_weights.iter().copied().sum::<u64>().max(1),
                "R":candidate.relevance,"density":candidate.relevance/candidate.lexical.span.len() as f64,
                "authenticated_card":candidate.card,
            })).collect::<Vec<_>>()})
    }
}
fn coverage_native_packet(
    binding: &Value,
    pool: &IntervalCoveragePool,
    chosen: &[usize],
    stats: &mut CoverageDiagnostics,
) -> Result<SelectionPacket> {
    stats.native_packet_calls += 1;
    let result = crate::retrieval::context_selection_packet::build_packet(
        binding.clone(),
        chosen
            .iter()
            .map(|&index| pool.candidates[index].card.clone())
            .collect(),
    );
    if let Err(error) = &result {
        stats.packing_errors += 1;
        stats.trial(json!({"stage":"deck","status":"error","error":error}));
    }
    result
}
fn coverage_deck_exhaustive_validated(
    binding: &Value,
    pool: &IntervalCoveragePool,
    meter: &Meter,
    stats: &mut CoverageDiagnostics,
) -> Result<SelectionPacket> {
    let mut chosen = Vec::new();
    let mut current = vec![0.0f64; pool.intervals.len()];
    let mut owners = Vec::new();
    for candidate in &pool.candidates {
        if !owners.contains(&candidate.owner) {
            owners.push(candidate.owner.clone());
        }
    }
    // Exactly one owner round. Remaining slots are global, without owner quotas.
    let floors = owners
        .into_iter()
        .map(Some)
        .chain(std::iter::repeat_n(None, 80));
    for owner in floors {
        meter.check()?;
        if chosen.len() == 80 {
            break;
        }
        let mut best: Option<(usize, f64)> = None;
        for index in 0..pool.candidates.len() {
            if chosen.contains(&index)
                || owner
                    .as_ref()
                    .is_some_and(|owner| owner != &pool.candidates[index].owner)
            {
                continue;
            }
            stats.candidate_evaluations += 1;
            meter.check()?;
            let gain = pool.gain(&current, index);
            if gain <= 0.0 && owner.is_none() {
                stats.zero_gain_candidates += 1;
                continue;
            }
            let mut trial = chosen.clone();
            trial.push(index);
            let packet = coverage_native_packet(binding, pool, &trial, stats)?;
            meter.check()?;
            if packet.cards.len() != trial.len() {
                stats.infeasible_trials += 1;
                stats.trial(json!({"candidate":pool.candidates[index].card.id,"stage":"deck","status":"native_input_cap","gain":gain,"input_bytes":packet.input_bytes}));
                continue;
            }
            stats.feasible_trials += 1;
            if best.is_none_or(|(old, old_gain)| {
                gain.total_cmp(&old_gain).is_gt()
                    || (gain.total_cmp(&old_gain).is_eq() && pool.tie_before(index, old))
            }) {
                best = Some((index, gain));
            }
        }
        let Some((index, gain)) = best else {
            if owner.is_some() {
                stats
                    .choices
                    .push(json!({"stage":"owner_floor","owner":owner,"status":"no_feasible_card"}));
                continue;
            }
            break;
        };
        chosen.push(index);
        pool.add(&mut current, index);
        stats.choices.push(json!({"stage":"deck","id":pool.candidates[index].card.id,"owner_floor":owner.is_some(),"gain":gain,"F":pool.objective(&current)}));
    }
    let packet = coverage_native_packet(binding, pool, &chosen, stats)?;
    assert_eq!(packet.cards.len(), chosen.len());
    assert!(packet.candidate_count <= 80 && packet.input_bytes <= 130_048);
    // Native omitted_candidates describes supplied selected cards only. The
    // full-pool exclusion count is separately explicit, never silently changed.
    stats.choices.push(json!({"stage":"deck_summary","full_pool":pool.candidates.len(),"selected":chosen.len(),"selection_omitted_candidates":pool.candidates.len()-chosen.len(),"F":pool.objective(&current)}));
    Ok(packet)
}
fn coverage_deck_ranked_validated(
    binding: &Value,
    pool: &IntervalCoveragePool,
    meter: &Meter,
    stats: &mut CoverageDiagnostics,
) -> Result<SelectionPacket> {
    let mut chosen = Vec::new();
    let mut current = vec![0.0f64; pool.intervals.len()];
    let mut owners = Vec::new();
    for candidate in &pool.candidates {
        if !owners.contains(&candidate.owner) {
            owners.push(candidate.owner.clone());
        }
    }
    // Exactly one owner round. Remaining slots are global, without owner quotas.
    let floors = owners
        .into_iter()
        .map(Some)
        .chain(std::iter::repeat_n(None, 80));
    for owner in floors {
        meter.check()?;
        if chosen.len() == 80 {
            break;
        }
        let mut ranked = Vec::new();
        for index in 0..pool.candidates.len() {
            if chosen.contains(&index)
                || owner
                    .as_ref()
                    .is_some_and(|owner| owner != &pool.candidates[index].owner)
            {
                continue;
            }
            stats.candidate_evaluations += 1;
            meter.check()?;
            let gain = pool.gain(&current, index);
            if gain <= 0.0 && owner.is_none() {
                stats.zero_gain_candidates += 1;
                continue;
            }
            ranked.push((index, gain));
        }
        ranked.sort_by(|&(left, left_gain), &(right, right_gain)| {
            right_gain
                .total_cmp(&left_gain)
                .then_with(|| {
                    pool.candidates[left]
                        .ordinal
                        .cmp(&pool.candidates[right].ordinal)
                })
                .then_with(|| pool.candidates[left].key.cmp(&pool.candidates[right].key))
                .then_with(|| left.cmp(&right))
        });
        let mut best = None;
        for (index, gain) in ranked {
            let mut trial = chosen.clone();
            trial.push(index);
            let packet = coverage_native_packet(binding, pool, &trial, stats)?;
            meter.check()?;
            if packet.cards.len() != trial.len() {
                stats.infeasible_trials += 1;
                stats.trial(json!({"candidate":pool.candidates[index].card.id,"stage":"deck","status":"native_input_cap","gain":gain,"input_bytes":packet.input_bytes}));
                continue;
            }
            stats.feasible_trials += 1;
            best = Some((index, gain));
            break;
        }
        let Some((index, gain)) = best else {
            if owner.is_some() {
                stats
                    .choices
                    .push(json!({"stage":"owner_floor","owner":owner,"status":"no_feasible_card"}));
                continue;
            }
            break;
        };
        chosen.push(index);
        pool.add(&mut current, index);
        stats.choices.push(json!({"stage":"deck","id":pool.candidates[index].card.id,"owner_floor":owner.is_some(),"gain":gain,"F":pool.objective(&current)}));
    }
    let packet = coverage_native_packet(binding, pool, &chosen, stats)?;
    assert_eq!(packet.cards.len(), chosen.len());
    assert!(packet.candidate_count <= 80 && packet.input_bytes <= 130_048);
    // Native omitted_candidates describes supplied selected cards only. The
    // full-pool exclusion count is separately explicit, never silently changed.
    stats.choices.push(json!({"stage":"deck_summary","full_pool":pool.candidates.len(),"selected":chosen.len(),"selection_omitted_candidates":pool.candidates.len()-chosen.len(),"F":pool.objective(&current)}));
    Ok(packet)
}
// The shortcut has a checked full-pool boundary even for direct helper tests.
// Prepare already validates every supplied card before truncation, but rebuilding
// once here also checks the exact reconstructed pool and binding without relying
// on a trace mutation convention. This extra native call is counted separately.
fn coverage_deck_with_order(
    binding: &Value,
    pool: &IntervalCoveragePool,
    meter: &Meter,
    stats: &mut CoverageDiagnostics,
    exhaustive: bool,
) -> Result<SelectionPacket> {
    meter.check()?;
    stats.full_pool_prevalidation_calls += 1;
    let all = (0..pool.candidates.len()).collect::<Vec<_>>();
    coverage_native_packet(binding, pool, &all, stats)?;
    meter.check()?;
    if exhaustive {
        coverage_deck_exhaustive_validated(binding, pool, meter, stats)
    } else {
        coverage_deck_ranked_validated(binding, pool, meter, stats)
    }
}
fn coverage_deck(
    binding: &Value,
    pool: &IntervalCoveragePool,
    meter: &Meter,
    stats: &mut CoverageDiagnostics,
) -> Result<SelectionPacket> {
    coverage_deck_with_order(binding, pool, meter, stats, false)
}
struct CoverageTrialContext<'a> {
    reader: &'a dyn QueryCatalog,
    request: &'a ContextRequest,
    query: &'a str,
    hits: &'a crate::retrieval::HitSet,
    fingerprint: Blake3Hash,
    omissions: Vec<crate::retrieval::ContextOmission>,
    warnings: Vec<String>,
}
fn coverage_trial_pack(
    env: &CoverageTrialContext<'_>,
    pool: &IntervalCoveragePool,
    chosen: &[usize],
    stats: &mut CoverageDiagnostics,
) -> Result<context::ContextDraft> {
    stats.trial_pack_calls += 1;
    let packets = chosen
        .iter()
        .enumerate()
        .map(|(rank, &index)| {
            let candidate = &pool.candidates[index];
            let mut passage = candidate.card.passage.clone();
            passage
                .rank_contributions
                .push(crate::retrieval::RankContribution {
                    channel: COVERAGE_MARKER.into(),
                    rank: index + 1,
                    score: None,
                });
            context::Packet {
                passages: vec![passage],
                bundle: None,
                navigation: None,
                key: candidate.key.clone(),
                score: 0.0,
                selection_ordinal: Some(candidate.ordinal),
                selection: Some(candidate.lexical.clone()),
                unit_score: Some(1.0 / (rank + 1) as f64),
                unit_origin: None,
                fallback: None,
                unit_clipped: false,
            }
        })
        .collect();
    let result = context::pack(
        env.reader,
        env.request,
        context::PackingInput {
            packets,
            omissions: env.omissions.clone(),
            term_weights: pool.term_weights.clone(),
            selection_warnings: env.warnings.clone(),
            source_aware: true,
            native_lexical_units: false,
            query: Some(env.query),
            signals: &ContextSelectionSignals::default(),
            selection_action: &SelectionAction::Automatic,
            hits: env.hits,
            graph: None,
            dependency_fingerprint: env.fingerprint.clone(),
            evidence_sets: None,
        },
    );
    match &result {
        Ok(draft) => stats.exact_trial_output_bytes += draft.text().len() as u64,
        Err(_) => stats.packing_errors += 1,
    }
    result
}
fn coverage_admitted(
    pool: &IntervalCoveragePool,
    draft: &context::ContextDraft,
) -> BTreeSet<usize> {
    draft
        .passages()
        .iter()
        .flat_map(|passage| {
            passage
                .rank_contributions
                .iter()
                .filter(|rank| rank.channel == COVERAGE_MARKER)
                .map(|rank| rank.rank - 1)
        })
        .filter(|&index| {
            let candidate = &pool.candidates[index];
            draft.passages().iter().any(|passage| {
                coverage_coordinate(passage) == candidate.coordinate
                    && passage.span.start() <= candidate.lexical.span.start()
                    && candidate.lexical.span.end() <= passage.span.end()
            })
        })
        .collect()
}
fn coverage_remove_markers(draft: &mut context::ContextDraft) {
    for passage in &mut draft.passages {
        passage
            .rank_contributions
            .retain(|rank| rank.channel != COVERAGE_MARKER);
    }
}
fn coverage_incremental_cost(new_bytes: usize, old_bytes: usize) -> usize {
    new_bytes.saturating_sub(old_bytes).max(1)
}
fn coverage_actual_scores(pool: &IntervalCoveragePool, draft: &context::ContextDraft) -> Vec<f64> {
    let mut scores = vec![0.0f64; pool.intervals.len()];
    for index in coverage_admitted(pool, draft) {
        pool.add(&mut scores, index);
    }
    scores
}
fn coverage_final(
    env: &CoverageTrialContext<'_>,
    pool: &IntervalCoveragePool,
    meter: &Meter,
    stats: &mut CoverageDiagnostics,
) -> Result<context::ContextDraft> {
    let mut chosen = Vec::new();
    let mut current = vec![0.0f64; pool.intervals.len()];
    let mut draft = coverage_trial_pack(env, pool, &chosen, stats)?;
    while chosen.len() < pool.candidates.len() {
        meter.check()?;
        let mut best: Option<(usize, f64, f64, usize, Vec<f64>, context::ContextDraft)> = None;
        for index in 0..pool.candidates.len() {
            if chosen.contains(&index) {
                continue;
            }
            stats.candidate_evaluations += 1;
            meter.check()?;
            let gain = pool.gain(&current, index);
            if gain <= 0.0 {
                stats.zero_gain_candidates += 1;
                continue;
            }
            let mut trial = chosen.clone();
            trial.push(index);
            let trial_draft = coverage_trial_pack(env, pool, &trial, stats)?;
            meter.check()?;
            let admitted = coverage_admitted(pool, &trial_draft);
            if !trial.iter().all(|index| admitted.contains(index)) {
                stats.infeasible_trials += 1;
                stats.trial(json!({"stage":"final","candidate":pool.candidates[index].card.id,"status":"not_admitted_at_exact_coordinates","gain":gain,"exact_rendered_bytes":trial_draft.text().len(),"admitted":admitted,"omissions":trial_draft.omissions}));
                continue;
            }
            stats.feasible_trials += 1;
            let actual_scores = coverage_actual_scores(pool, &trial_draft);
            let gain = actual_scores
                .iter()
                .zip(&current)
                .zip(&pool.intervals)
                .map(|((next, old), interval)| {
                    (next - old).max(0.0) * (interval.end - interval.start) as f64
                })
                .sum::<f64>();
            if gain <= 0.0 {
                stats.zero_gain_candidates += 1;
                continue;
            }
            let incremental =
                coverage_incremental_cost(trial_draft.text().len(), draft.text().len());
            let ratio = gain / incremental as f64;
            stats.trial(json!({"stage":"final","candidate":pool.candidates[index].card.id,"status":"feasible","gain":gain,"exact_rendered_bytes":trial_draft.text().len(),"old_rendered_bytes":draft.text().len(),"denominator":incremental,"ratio":ratio}));
            if best.as_ref().is_none_or(|(old, old_ratio, _, _, _, _)| {
                ratio.total_cmp(old_ratio).is_gt()
                    || (ratio.total_cmp(old_ratio).is_eq() && pool.tie_before(index, *old))
            }) {
                best = Some((index, ratio, gain, incremental, actual_scores, trial_draft));
            }
        }
        let Some((index, ratio, gain, incremental, actual_scores, winner)) = best else {
            break;
        };
        chosen.push(index);
        current = actual_scores;
        draft = winner;
        stats.choices.push(json!({"stage":"final","id":pool.candidates[index].card.id,"gain":gain,"denominator":incremental,"ratio":ratio,"F":pool.objective(&current),"exact_rendered_bytes":draft.text().len()}));
    }
    coverage_remove_markers(&mut draft);
    Ok(draft)
}
enum CoverageAction {
    Deck,
    Final,
    Apply(SelectionReply),
}
fn coverage_context(
    catalog: &Catalog,
    query: &str,
    request: &ContextRequest,
    action: CoverageAction,
    options: &ContextOptions,
    expected_pool: Option<&Value>,
    stats: &mut CoverageDiagnostics,
) -> Result<ContextResult> {
    // One outer elapsed meter and one selected proof for preparation, every pure
    // trial, final admission and final recheck. Never restart verification here.
    let meter = Meter::new(&request.verification_budget);
    let request = context::validate_request(query, request)?;
    context::validate_selection_action(&request, &SelectionAction::Prepare)?;
    assert_eq!(request.scope, ContextScope::IndexedDocuments);
    if catalog.operation_state()?.is_none() {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "coverage experiment requires an already normalized vault",
        ));
    }
    meter.check()?;
    catalog.guard_query()?;
    meter.check()?;
    let reader = catalog.cached_query_snapshot(QueryReadLimits {
        max_elapsed_ms: meter.remaining_ms(),
        ..QueryReadLimits::default()
    })?;
    let mut hits = lexical::search_context_catalog(&reader, query, &request.documents, false)?;
    meter.check()?;
    let paths = hits
        .hits
        .iter()
        .map(|hit| hit.locator.path.clone())
        .collect::<Vec<_>>();
    let mut budget = request.verification_budget.clone();
    budget.max_elapsed_ms = meter.remaining_ms();
    let mut proof = selected_documents::authenticate(catalog, &reader, &paths, &budget)?;
    hits.dependency_fingerprint = proof.fingerprint.clone();
    let selected = SelectedCatalog {
        reader: &reader,
        proof: &proof,
    };
    let (prepared, trace) = context::with_candidate_ordering_trace(|| {
        context::assemble_bounded_documents_with_selection_for_query(
            &selected,
            &request,
            &hits,
            query,
            &SelectionAction::Prepare,
        )
    });
    let mut prepared = prepared?;
    stats.original_prepare_native_packet_calls = 1;
    meter.check()?;
    if let Some(expected) = expected_pool {
        if trace_stage(&trace, "candidate_pool") != expected {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "development retained pool differs from the pinned control",
            ));
        }
    }
    let pool = IntervalCoveragePool::from_trace(&trace);
    stats.pool = pool.metadata();
    let original = prepared.selection_packet.as_ref().unwrap();
    let task: Value = serde_json::from_str(&original.selector_input).unwrap();
    let binding = &task["payload"]["binding"];
    assert_eq!(binding["query"], json!(query));
    assert_eq!(binding["request"], serde_json::to_value(&request).unwrap());
    assert_eq!(
        binding["snapshot"],
        serde_json::to_value(reader.snapshot()).unwrap()
    );
    assert_eq!(
        binding["dependency_fingerprint"],
        serde_json::to_value(&proof.fingerprint).unwrap()
    );
    let draft = match action {
        CoverageAction::Deck => {
            let deck = coverage_deck(binding, &pool, &meter, stats)?;
            prepared.truncated = hits.truncated
                || !prepared.omissions.is_empty()
                || deck.cards.len() < pool.candidates.len();
            prepared.selection_packet = Some(deck);
            prepared.warnings.push("test-only weighted interval deck; public Prepare is unchanged; native omitted count is over supplied selected cards, full-pool exclusions are in diagnostics".into());
            prepared
        }
        action => {
            let env = CoverageTrialContext {
                reader:&selected, request:&request, query, hits:&hits, fingerprint:proof.fingerprint.clone(),
                omissions:prepared.omissions.clone(),
                warnings:prepared.warnings.iter().filter(|warning| !warning.starts_with("candidate packet for one host selector invocation;")).cloned().chain(std::iter::once("test-only exact interval coverage prototype; public Automatic and host Apply are unchanged".into())).collect(),
            };
            match action {
                CoverageAction::Final => coverage_final(&env, &pool, &meter, stats)?,
                CoverageAction::Apply(reply) => {
                    // Oracle/host IDs never enter scoring: rebuild the same
                    // deck first, then native reply validation and exact packing.
                    let deck = coverage_deck(binding, &pool, &meter, stats)?;
                    let ids =
                        crate::retrieval::context_selection_packet::validate_reply(&deck, &reply)?;
                    let chosen = ids
                        .iter()
                        .map(|id| {
                            pool.candidates
                                .iter()
                                .position(|candidate| &candidate.card.id == id)
                                .unwrap()
                        })
                        .collect::<Vec<_>>();
                    let mut draft = coverage_trial_pack(&env, &pool, &chosen, stats)?;
                    coverage_remove_markers(&mut draft);
                    draft
                }
                CoverageAction::Deck => unreachable!(),
            }
        }
    };
    if let Some(fault) = &options.fault {
        fault.check(ContextCheckpoint::BeforeFinalVerification { attempt: 0 })?;
    }
    meter.check()?;
    proof.recheck(catalog, &reader)?;
    let verification = verification(&reader)?;
    meter.check()?;
    Ok(seal(draft, verification, proof.meter()))
}

#[test]
fn coverage_relevance_uses_frozen_weights_and_exact_coordinate_identity() {
    let candidate = crate::retrieval::context_selection::SelectionCandidate {
        owner_index: 0,
        span: ByteSpan::new(0, 10).unwrap(),
        covered_terms: vec![0],
        local_relevance: 2,
        seed_overlap: true,
        clipped: true,
        semantic_affinity: None,
    };
    assert!((coverage_relevance(0.2, &candidate, &[2, 3]) - 0.182).abs() < 1e-12);
    let fixture = Fixture::new();
    let prepared = fixture.prepare();
    let mut passage = prepared.selection_packet().unwrap().cards[0]
        .passage
        .clone();
    let original = coverage_coordinate(&passage);
    passage.locator.observed_hash = Blake3Hash::digest(b"different revision bytes");
    assert_ne!(coverage_coordinate(&passage), original);
    passage.locator.path = path("sources/other/revisions/different/content.md");
    assert_ne!(coverage_coordinate(&passage), original);
}
#[test]
fn coverage_native_trial_pack_keeps_exact_output_and_id_reply_order() {
    let fixture = Fixture::new();
    let mut stats = CoverageDiagnostics::default();
    let final_result = coverage_context(
        &fixture.catalog,
        QUERY,
        &request(),
        CoverageAction::Final,
        &ContextOptions::default(),
        None,
        &mut stats,
    )
    .unwrap();
    assert!(!final_result.passages().is_empty());
    assert!(final_result.text().len() <= 6000 && final_result.usage().estimated_tokens <= 1500);
    assert!(stats.trial_pack_calls > 1 && stats.exact_trial_output_bytes > 0);
    assert_eq!(stats.internal_render_calls, None);
    for passage in final_result.passages() {
        assert!(
            passage
                .rank_contributions
                .iter()
                .all(|rank| rank.channel != COVERAGE_MARKER)
        );
        let bytes = fs::read(
            fixture
                .catalog
                .fs()
                .root()
                .path()
                .join(passage.locator.path.as_str()),
        )
        .unwrap();
        assert_eq!(
            passage
                .span
                .slice(std::str::from_utf8(&bytes).unwrap())
                .unwrap(),
            passage.text
        );
    }
    let deck = coverage_context(
        &fixture.catalog,
        QUERY,
        &request(),
        CoverageAction::Deck,
        &ContextOptions::default(),
        None,
        &mut CoverageDiagnostics::default(),
    )
    .unwrap();
    let packet = deck.selection_packet().unwrap();
    assert!(packet.candidate_count <= 80 && packet.input_bytes <= 130048);
    let reply = SelectionReply {
        packet_fingerprint: packet.fingerprint.clone(),
        ordered_ids: packet
            .cards
            .iter()
            .take(5)
            .map(|card| card.id.clone())
            .collect(),
    };
    let applied = coverage_context(
        &fixture.catalog,
        QUERY,
        &request(),
        CoverageAction::Apply(reply),
        &ContextOptions::default(),
        None,
        &mut CoverageDiagnostics::default(),
    )
    .unwrap();
    assert!(!applied.passages().is_empty());
    assert!(applied.text().len() <= 6000);
}
#[test]
fn coverage_all_arms_recheck_selected_dependencies_before_emission() {
    for final_arm in [false, true] {
        let fixture = Fixture::new();
        let target = fixture.catalog.fs().root().path().join("decision.md");
        let changed = String::from_utf8(fs::read(&target).unwrap())
            .unwrap()
            .replace("\"accept\"", "\"reject\"")
            .into_bytes();
        let options = ContextOptions {
            selection: SelectionAction::Automatic,
            fault: Some(Arc::new(EditBeforeEmission {
                path: target.clone(),
                bytes: changed.clone(),
            })),
        };
        let error = coverage_context(
            &fixture.catalog,
            QUERY,
            &request(),
            if final_arm {
                CoverageAction::Final
            } else {
                CoverageAction::Deck
            },
            &options,
            None,
            &mut CoverageDiagnostics::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::FreshnessConflict);
        assert_eq!(fs::read(target).unwrap(), changed);
    }
}

#[test]
fn coverage_interval_gain_diminishes_only_on_exact_shared_coordinates() {
    let fixture = Fixture::new();
    let (_, trace) = context::with_candidate_ordering_trace(|| fixture.prepare());
    let actual = IntervalCoveragePool::from_trace(&trace);
    let mut first = actual.candidates[0].clone();
    first.lexical.span = ByteSpan::new(0, 10).unwrap();
    first.relevance = 10.0;
    first.intervals = vec![0, 1];
    let mut overlap = first.clone();
    overlap.lexical.span = ByteSpan::new(5, 15).unwrap();
    overlap.intervals = vec![1, 2];
    let mut distinct = first.clone();
    distinct.coordinate = "independent exact locator/revision/hash".into();
    distinct.intervals = vec![3, 4];
    // Synthetic interval metadata tests the objective only; emitted evidence
    // and exact packing are exercised separately on authenticated fixture bytes.
    let pool = IntervalCoveragePool {
        candidates: vec![first, overlap, distinct],
        intervals: vec![
            CoverageInterval {
                coordinate: "original".into(),
                start: 0,
                end: 5,
            },
            CoverageInterval {
                coordinate: "original".into(),
                start: 5,
                end: 10,
            },
            CoverageInterval {
                coordinate: "original".into(),
                start: 10,
                end: 15,
            },
            CoverageInterval {
                coordinate: "other".into(),
                start: 0,
                end: 5,
            },
            CoverageInterval {
                coordinate: "other".into(),
                start: 5,
                end: 10,
            },
        ],
        term_weights: vec![],
    };
    let mut scores = vec![0.0; 5];
    assert_eq!(pool.gain(&scores, 0), 10.0);
    pool.add(&mut scores, 0);
    assert_eq!(pool.gain(&scores, 0), 0.0);
    assert_eq!(pool.gain(&scores, 1), 5.0);
    assert_eq!(pool.gain(&scores, 2), 10.0);
    assert_eq!(coverage_incremental_cost(100, 100), 1);
    assert_eq!(coverage_incremental_cost(90, 100), 1);
    assert_eq!(coverage_incremental_cost(110, 100), 10);
}
#[test]
fn coverage_contained_native_skip_cannot_earn_a_weighted_vote() {
    let fixture = Fixture::new();
    let request = request();
    let reader = fixture
        .catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let mut hits =
        lexical::search_context_catalog(&reader, QUERY, &request.documents, false).unwrap();
    let paths = hits
        .hits
        .iter()
        .map(|hit| hit.locator.path.clone())
        .collect::<Vec<_>>();
    let mut proof = selected_documents::authenticate(
        &fixture.catalog,
        &reader,
        &paths,
        &request.verification_budget,
    )
    .unwrap();
    hits.dependency_fingerprint = proof.fingerprint.clone();
    let selected = SelectedCatalog {
        reader: &reader,
        proof: &proof,
    };
    let (_, trace) = context::with_candidate_ordering_trace(|| {
        context::assemble_bounded_documents_with_selection_for_query(
            &selected,
            &request,
            &hits,
            QUERY,
            &SelectionAction::Prepare,
        )
        .unwrap()
    });
    let mut pool = IntervalCoveragePool::from_trace(&trace);
    let mut duplicate = pool.candidates[0].clone();
    duplicate.card.id = "c9999".into();
    duplicate.relevance *= 2.0;
    let duplicate_index = pool.candidates.len();
    pool.candidates.push(duplicate);
    let env = CoverageTrialContext {
        reader: &selected,
        request: &request,
        query: QUERY,
        hits: &hits,
        fingerprint: proof.fingerprint.clone(),
        omissions: vec![],
        warnings: vec![],
    };
    let draft = coverage_trial_pack(
        &env,
        &pool,
        &[0, duplicate_index],
        &mut CoverageDiagnostics::default(),
    )
    .unwrap();
    let admitted = coverage_admitted(&pool, &draft);
    assert!(admitted.contains(&0));
    assert!(!admitted.contains(&duplicate_index));
    let actual = coverage_actual_scores(&pool, &draft);
    assert!((pool.objective(&actual) - pool.candidates[0].relevance).abs() < 1e-12);
    proof.recheck(&fixture.catalog, &reader).unwrap();
}

// Synthetic native-boundary fixtures are not canonical evidence or quality
// labels. Every complete pool is validated by the actual builder before either
// evaluation order, including cards the native cap will omit.
fn deck_cost_pool(count: usize) -> IntervalCoveragePool {
    let fixture = Fixture::new();
    let (_, trace) = context::with_candidate_ordering_trace(|| fixture.prepare());
    let original = IntervalCoveragePool::from_trace(&trace);
    let mut candidates = Vec::new();
    let mut intervals = Vec::new();
    for index in 0..count {
        let mut candidate = original.candidates[0].clone();
        candidate.card.id = format!("d{index:04}");
        candidate.card.title = format!("Café 東京 \\\" {index:03}");
        candidate.card.passage.text = "Café 東京 \\\"\n".into();
        candidate.card.passage.span =
            ByteSpan::new(0, candidate.card.passage.text.len() as u64).unwrap();
        candidate.card.passage.locator.path = path(&format!("fixture/owner{index:03}.md"));
        candidate.card.passage.citations.clear();
        candidate.card.child_span = None;
        candidate.card.rendered_bytes = candidate.card.passage.text.len() + 128;
        candidate.lexical.span = candidate.card.passage.span;
        candidate.coordinate = coverage_coordinate(&candidate.card.passage);
        candidate.owner = format!("owner{}", index % 3);
        candidate.ordinal = index;
        candidate.key = format!("key{index:03}");
        candidate.relevance = 1.0;
        candidate.intervals = vec![index];
        intervals.push(CoverageInterval {
            coordinate: candidate.coordinate.clone(),
            start: 0,
            end: candidate.lexical.span.end(),
        });
        candidates.push(candidate);
    }
    IntervalCoveragePool {
        candidates,
        intervals,
        term_weights: original.term_weights,
    }
}
fn deck_cost_equivalent(
    binding: &Value,
    pool: &IntervalCoveragePool,
) -> (SelectionPacket, CoverageDiagnostics) {
    let mut budget = request().verification_budget;
    budget.max_elapsed_ms = 5000;
    let mut old = CoverageDiagnostics::default();
    let previous =
        coverage_deck_with_order(binding, pool, &Meter::new(&budget), &mut old, true).unwrap();
    let mut new = CoverageDiagnostics::default();
    let packet = coverage_deck(binding, pool, &Meter::new(&budget), &mut new).unwrap();
    assert_eq!(
        packet, previous,
        "exact cards, task bytes, dictionary, commitments and counts"
    );
    assert_eq!(
        new.choices, old.choices,
        "ordered choices, objective and owner-floor trace"
    );
    assert_eq!(new.candidate_evaluations, old.candidate_evaluations);
    assert_eq!(new.zero_gain_candidates, old.zero_gain_candidates);
    assert_eq!(new.full_pool_prevalidation_calls, 1);
    assert!(new.native_packet_calls <= old.native_packet_calls);
    (packet, new)
}
#[test]
fn deck_cost_rank_first_matches_exhaustive_ties_overlap_and_zero_gain_floor() {
    let mut pool = deck_cost_pool(8);
    // Equal gain/ordinal uses key, not enumeration. A repeated exact coordinate
    // has zero gain after its first admission but still gets its owner round.
    pool.candidates[1].ordinal = 0;
    pool.candidates[1].key = "aaa".into();
    pool.candidates[1].owner = pool.candidates[0].owner.clone();
    pool.candidates[7].card.passage = pool.candidates[2].card.passage.clone();
    pool.candidates[7].lexical.span = pool.candidates[2].lexical.span;
    pool.candidates[7].coordinate = pool.candidates[2].coordinate.clone();
    pool.candidates[7].intervals = pool.candidates[2].intervals.clone();
    pool.candidates[7].owner = "last-zero-floor".into();
    let (packet, stats) = deck_cost_equivalent(&json!({"query":"native boundary"}), &pool);
    assert_eq!(packet.cards[0].id, "d0001");
    assert!(
        stats
            .choices
            .iter()
            .any(|choice| choice["owner_floor"] == true && choice["gain"] == 0.0)
    );
    let empty = deck_cost_pool(0);
    assert_eq!(deck_cost_equivalent(&json!({}), &empty).0.cards.len(), 0);
}
#[test]
fn deck_cost_rank_first_matches_partial_overlap_and_positive_gain_stop() {
    let mut pool = deck_cost_pool(3);
    let locator = pool.candidates[0].card.passage.locator.clone();
    let coordinate = coverage_coordinate(&pool.candidates[0].card.passage);
    let boundaries = [0, 5, 8, 10, 13, 15];
    pool.intervals = boundaries
        .windows(2)
        .map(|ends| CoverageInterval {
            coordinate: coordinate.clone(),
            start: ends[0],
            end: ends[1],
        })
        .collect();
    for (index, (start, end)) in [(0, 10), (5, 15), (8, 13)].into_iter().enumerate() {
        let candidate = &mut pool.candidates[index];
        candidate.owner = "overlapping-owner".into();
        candidate.coordinate = coordinate.clone();
        candidate.card.passage.locator = locator.clone();
        candidate.card.passage.span = ByteSpan::new(start, end).unwrap();
        candidate.card.passage.text = "x".repeat((end - start) as usize);
        candidate.lexical.span = candidate.card.passage.span;
        candidate.intervals = pool
            .intervals
            .iter()
            .enumerate()
            .filter(|(_, interval)| start <= interval.start && interval.end <= end)
            .map(|(index, _)| index)
            .collect();
    }
    let (packet, _) =
        deck_cost_equivalent(&json!({"query":"partial exact-coordinate overlap"}), &pool);
    assert_eq!(packet.cards.len(), 3);
    // All lower-density coordinates already covered by a higher-density card
    // stop the global round without backfilling zero-gain cards.
    pool.candidates[1].card.passage = pool.candidates[0].card.passage.clone();
    pool.candidates[1].lexical.span = pool.candidates[0].lexical.span;
    pool.candidates[1].intervals = pool.candidates[0].intervals.clone();
    pool.candidates[1].relevance = 0.1;
    assert_eq!(deck_cost_equivalent(&json!({}), &pool).0.cards.len(), 2);
}

#[test]
fn deck_cost_rank_first_matches_exact_eighty_card_boundary_and_repeat_bytes() {
    let pool = deck_cost_pool(84);
    let binding = json!({"query":"escaped UTF-8 dictionary indexes"});
    let (packet, stats) = deck_cost_equivalent(&binding, &pool);
    assert_eq!(packet.cards.len(), 80);
    assert_eq!(
        stats.native_packet_calls, 82,
        "prevalidation + 80 winning trials + final build"
    );
    assert_eq!(stats.infeasible_trials, 0);
    let mut again = CoverageDiagnostics::default();
    assert_eq!(
        coverage_deck(
            &binding,
            &pool,
            &Meter::new(&request().verification_budget),
            &mut again
        )
        .unwrap(),
        packet
    );
    let task: Value = serde_json::from_str(&packet.selector_input).unwrap();
    assert!(task["payload"]["sources"].as_array().unwrap().len() > 10);
}
#[test]
fn deck_cost_rank_first_retries_multiple_native_byte_cap_failures() {
    let mut pool = deck_cost_pool(4);
    for candidate in &mut pool.candidates {
        candidate.owner = "one-owner".into();
    }
    pool.candidates[0].relevance = 100.0;
    for (index, relevance) in [(1, 80.0), (2, 70.0)] {
        let candidate = &mut pool.candidates[index];
        candidate.relevance = relevance;
        candidate.card.title = "東京\\\"".repeat(512); // exactly 4096 UTF-8 bytes
        candidate.card.passage.text = "\\\"".repeat(1024); // exactly 2048 bytes, heavily escaped
        candidate.card.passage.span = ByteSpan::new(0, 2048).unwrap();
        candidate.card.rendered_bytes = 2200;
        candidate.lexical.span = candidate.card.passage.span;
        pool.intervals[index].end = 2048;
    }
    pool.candidates[3].relevance = 10.0;
    // Same already-admitted source metadata for the lower-ranked fitting card.
    pool.candidates[3].card.title = pool.candidates[0].card.title.clone();
    pool.candidates[3].card.passage.locator = pool.candidates[0].card.passage.locator.clone();
    pool.candidates[3].card.passage.locator.observed_hash =
        Blake3Hash::digest(b"second exact version, same source dictionary");
    pool.candidates[3].coordinate = coverage_coordinate(&pool.candidates[3].card.passage);
    pool.intervals[3].coordinate = pool.candidates[3].coordinate.clone();
    let binding = json!({"query":"native byte pressure","padding":"x".repeat(120_000)});
    let (packet, stats) = deck_cost_equivalent(&binding, &pool);
    assert_eq!(
        packet
            .cards
            .iter()
            .map(|card| card.id.as_str())
            .collect::<Vec<_>>(),
        vec!["d0000", "d0003"]
    );
    assert!(
        stats.infeasible_trials >= 4,
        "both rejected alternatives are reconsidered after the winner"
    );
    assert!(packet.input_bytes <= 130_048);
    // A separate owner whose entire round fails does not stop the global round.
    pool.candidates[1].owner = "unfittable-owner".into();
    pool.candidates[2].owner = "unfittable-owner".into();
    let (_, stats) = deck_cost_equivalent(&binding, &pool);
    assert!(
        stats
            .choices
            .iter()
            .any(|choice| choice["status"] == "no_feasible_card")
    );
}
#[test]
fn deck_cost_full_pool_validation_rejects_malformed_omitted_cards_and_binding() {
    let mut pool = deck_cost_pool(81);
    pool.candidates[80].relevance = 0.0;
    let valid = pool.candidates[80].card.clone();
    let binding = json!({});
    for invalid in 0..5 {
        pool.candidates[80].card = valid.clone();
        match invalid {
            0 => pool.candidates[80].card.id = "invalid id".into(),
            1 => pool.candidates[80].card.id = pool.candidates[0].card.id.clone(),
            2 => pool.candidates[80].card.passage.text.push('!'),
            3 => pool.candidates[80].card.title = "x".repeat(4097),
            _ => {
                let fixture = Fixture::new();
                let mut card = fixture
                    .prepare()
                    .selection_packet()
                    .unwrap()
                    .cards
                    .iter()
                    .find(|card| !card.passage.citations.is_empty())
                    .unwrap()
                    .clone();
                card.id = valid.id.clone();
                card.passage.text = "x".repeat(card.passage.text.len());
                pool.candidates[80].card = card;
            }
        }
        for exhaustive in [false, true] {
            let mut stats = CoverageDiagnostics::default();
            assert!(
                coverage_deck_with_order(
                    &binding,
                    &pool,
                    &Meter::new(&request().verification_budget),
                    &mut stats,
                    exhaustive
                )
                .is_err()
            );
            assert_eq!(stats.native_packet_calls, 1);
            assert_eq!(stats.packing_errors, 1);
            assert!(stats.choices.is_empty());
        }
    }
    pool.candidates[80].card = valid;
    let huge = json!({"padding":"x".repeat(130_049)});
    let mut stats = CoverageDiagnostics::default();
    assert!(
        coverage_deck(
            &huge,
            &pool,
            &Meter::new(&request().verification_budget),
            &mut stats
        )
        .is_err()
    );
    assert!(stats.choices.is_empty());
}
#[test]
fn deck_cost_deadline_returns_error_without_partial_packet() {
    let pool = deck_cost_pool(4);
    let mut budget = request().verification_budget;
    budget.max_elapsed_ms = 0;
    let mut stats = CoverageDiagnostics::default();
    assert!(coverage_deck(&json!({}), &pool, &Meter::new(&budget), &mut stats).is_err());
    assert!(stats.choices.is_empty());
    assert_eq!(stats.native_packet_calls, 0);
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CoveragePilotConfig {
    vault: PathBuf,
    cases: PathBuf,
    controls: PathBuf,
    artifacts: PathBuf,
    #[serde(default)]
    prior_decks: Option<PathBuf>,
}
#[test]
#[ignore = "explicit frozen six-case coverage config required; no models or public production mode"]
fn normalized_interval_coverage_six_case_development_pilot() {
    coverage_development_pilot(false);
}
#[test]
#[ignore = "explicit frozen six-case deck-cost config required; no models or production mode"]
fn normalized_interval_deck_cost_six_case_development_pilot() {
    coverage_development_pilot(true);
}
fn coverage_development_pilot(deck_cost: bool) {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .unwrap();
    let owned = repo
        .join(if deck_cost {
            ".artifacts/context-ordering-003"
        } else {
            ".artifacts/context-ordering-002"
        })
        .canonicalize()
        .unwrap();
    let previous = repo
        .join(".artifacts/context-ordering-001/pilot-inputs-001")
        .canonicalize()
        .unwrap();
    let config_path = PathBuf::from(
        std::env::var(if deck_cost {
            "LWIKI_DECK_COST_CONFIG"
        } else {
            "LWIKI_COVERAGE_CONFIG"
        })
        .expect("explicit owned configuration"),
    )
    .canonicalize()
    .unwrap();
    assert!(config_path.starts_with(&owned));
    let config: CoveragePilotConfig =
        serde_json::from_value(bounded_json(&config_path, 8192)).unwrap();
    let vault = config.vault.canonicalize().unwrap();
    let cases = config.cases.canonicalize().unwrap();
    let controls = config.controls.canonicalize().unwrap();
    assert!(vault.starts_with(&owned) || vault == previous.join("unit-vault"));
    assert!(cases.starts_with(&owned));
    assert_eq!(controls, previous.join("unit-results-001"));
    let prior_decks = config
        .prior_decks
        .as_ref()
        .map(|path| path.canonicalize().unwrap());
    if deck_cost {
        assert_eq!(
            prior_decks,
            Some(
                repo.join(".artifacts/context-ordering-002/results-001")
                    .canonicalize()
                    .unwrap()
            )
        );
    } else {
        assert!(prior_decks.is_none());
    }
    assert!(
        config
            .artifacts
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .starts_with(&owned)
    );
    let cases: Vec<PilotCase> = serde_json::from_value(bounded_json(&cases, 32 * 1024)).unwrap();
    let allowed = bounded_json(
        &repo.join(".artifacts/context-development-first-loss-001/manifest.json"),
        128 * 1024,
    );
    assert_eq!(cases.len(), 6);
    assert_eq!(allowed["cases"].as_array().unwrap().len(), 6);
    let mut seen = BTreeSet::new();
    for case in &cases {
        assert!(
            case.id.len() <= 64
                && case
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && seen.insert(case.id.clone())
        );
        assert!(
            case.query.len() <= 4096
                && allowed["cases"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|value| value["id"] == case.id && value["query"] == case.query)
        );
    }
    let apply_case = std::env::var("LWIKI_COVERAGE_APPLY_CASE").ok();
    let reply_path = std::env::var("LWIKI_COVERAGE_REPLY").ok();
    assert_eq!(
        apply_case.is_some(),
        reply_path.is_some(),
        "one explicit ID-replay case and reply, or neither"
    );
    let reply = reply_path.map(|path| {
        let path = PathBuf::from(path).canonicalize().unwrap();
        assert!(path.starts_with(&owned));
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert!(metadata.is_file() && metadata.len() <= 4096);
        crate::retrieval::context_selection_packet::parse_reply(&fs::read(&path).unwrap()).unwrap()
    });
    if let Some(case) = &apply_case {
        assert!(cases.iter().any(|candidate| &candidate.id == case));
    }
    let handle = VaultFs::new(VaultRoot::explicit(&vault).unwrap());
    let marker = parse_note(&fs::read(vault.join("WIKI.md")).unwrap());
    let catalog = Catalog::new(handle, marker.canonical.as_ref().unwrap().id().clone());
    assert!(
        catalog.operation_state().unwrap().is_some(),
        "no implicit activation/reconstruction"
    );
    fs::create_dir(&config.artifacts).expect("new attempt directory required");
    let mut request = request();
    request.verification_budget.max_elapsed_ms = 5000;
    request.verification_budget.max_entries = 65_536;
    let mut total = 0usize;
    let mut write = |name: &str, value: Value| {
        let bytes = serde_json::to_vec_pretty(&value).unwrap();
        assert!(bytes.len() <= 16 * 1024 * 1024);
        total += bytes.len();
        assert!(total <= 128 * 1024 * 1024);
        fs::write(config.artifacts.join(name), bytes).unwrap();
    };
    let mut statuses = cases
        .iter()
        .filter(|case| apply_case.as_ref().is_none_or(|id| id == &case.id))
        .flat_map(|case| {
            let arms = if apply_case.is_some() {
                vec!["exact-id-apply"]
            } else {
                vec!["weighted-deck", "weighted-final"]
            };
            arms.into_iter()
                .map(|arm| json!({"case":case.id,"arm":arm,"status":"UNRUN"}))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    write("status.json", json!(statuses));
    let mut comparison_failures = Vec::new();
    for case in cases {
        if apply_case.as_ref().is_some_and(|id| id != &case.id) {
            continue;
        }
        let control = bounded_json(
            &controls.join(format!("{}-lineage.json", case.id)),
            16 * 1024 * 1024,
        );
        assert_eq!(control["request"], json!(request));
        assert_eq!(control["query"], json!(case.query));
        let expected = trace_stage(control["trace"].as_array().unwrap(), "candidate_pool");
        write(
            &format!("{}-control-reference.json", case.id),
            json!({"directory":controls,"query":case.query,"request":request,"candidate_pool":expected,"scope":"immutable old/current/A1/B1/first-five controls remain archived; no critic labels or oracle replies are scoring inputs"}),
        );
        let actions = if let Some(reply) = &reply {
            vec![("exact-id-apply", CoverageAction::Apply(reply.clone()))]
        } else {
            vec![
                ("weighted-deck", CoverageAction::Deck),
                ("weighted-final", CoverageAction::Final),
            ]
        };
        for (name, action) in actions {
            let status = statuses
                .iter_mut()
                .find(|value| value["case"] == case.id && value["arm"] == name)
                .unwrap();
            status["status"] = json!("RUNNING");
            write("status.json", json!(statuses));
            let mut stats = CoverageDiagnostics::default();
            let started = std::time::Instant::now();
            let result = coverage_context(
                &catalog,
                &case.query,
                &request,
                action,
                &ContextOptions::default(),
                Some(expected),
                &mut stats,
            );
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            let mut comparisons = json!(null);
            if let Some(prior) = &prior_decks {
                let archived = bounded_json(
                    &prior.join(format!(
                        "{}-{}.json",
                        case.id,
                        if name == "exact-id-apply" {
                            "weighted-deck"
                        } else {
                            name
                        }
                    )),
                    16 * 1024 * 1024,
                );
                let actual = json!(&result);
                let mut checks = Vec::new();
                checks.push((
                    "retained_pool",
                    stats.pool == archived["diagnostics"]["pool"],
                ));
                if name == "weighted-deck" {
                    if archived["result"]["Ok"].is_object() {
                        checks.push((
                            "successful_deck_exact_packet",
                            actual["Ok"]["selection_packet"]
                                == archived["result"]["Ok"]["selection_packet"],
                        ));
                        checks.push((
                            "successful_deck_exact_choices",
                            json!(stats.choices) == archived["diagnostics"]["choices"],
                        ));
                    } else {
                        let prefix = archived["diagnostics"]["choices"].as_array().unwrap();
                        checks.push((
                            "failed_deck_accepted_choice_prefix",
                            stats.choices.starts_with(prefix),
                        ));
                    }
                } else if name == "weighted-final" {
                    // FullPoolFinal algorithm is unchanged. Exclude only the
                    // already-declared wall-clock verification timestamp.
                    let mut old = archived["result"].clone();
                    let mut new = actual.clone();
                    for value in [&mut old, &mut new] {
                        if let Some(verification) = value["Ok"]["verification"].as_object_mut() {
                            verification.remove("verified_at");
                        }
                    }
                    checks.push(("unchanged_final_except_verified_at", old == new));
                }
                for (check, matched) in &checks {
                    if !matched {
                        comparison_failures.push(format!("{} {name} {check}", case.id));
                    }
                }
                comparisons = json!({"archive":prior,"checks":checks,"exclude_only":"verification.verified_at for final; elapsed time and diagnostics compared separately"});
            }
            let outcome = if result.is_ok() { "OK" } else { "ERROR" };
            write(
                &format!("{}-{name}.json", case.id),
                json!({"result":result,"diagnostics":stats,"archive_comparisons":comparisons,"elapsed_ms":elapsed_ms,"scope":"test-only exact packing experiment; not public Automatic, host/model success, relevance/completeness or shipping performance qualification"}),
            );
            let status = statuses
                .iter_mut()
                .find(|value| value["case"] == case.id && value["arm"] == name)
                .unwrap();
            status["status"] = json!(outcome);
            write("status.json", json!(statuses));
        }
    }
    write(
        "archive-comparison-status.json",
        json!({"failures":comparison_failures}),
    );
    assert!(
        comparison_failures.is_empty(),
        "all twelve arms recorded before reporting archived-control mismatch: {comparison_failures:?}"
    );
}

// Disposable lifecycle fixtures exercise native automatic historical context.
fn historical_fixture() -> Fixture {
    let fixture = Fixture::new();
    let plan = SourceStore::new(fixture.catalog.fs().clone())
        .plan_refresh(
            &fixture.source,
            capture("selectionprobe: Café 東京 current permit is amber.\n".as_bytes()),
        )
        .unwrap();
    Fixture::seed(fixture.catalog.fs(), plan.draft.unwrap());
    fixture.republish();
    fixture
}
fn historical_request(mode: SearchMode) -> ContextRequest {
    let mut request = request();
    request.documents.mode = mode;
    request.documents.filters.include_historical = true;
    request
}
#[test]
fn historical_native_context_separates_revisions_and_exact_utf8_citations() {
    for mode in [SearchMode::Lexical, SearchMode::Literal] {
        let fixture = historical_fixture();
        let mut current = request();
        current.documents.mode = mode;
        current.documents.filters.source_ids = vec![fixture.source.clone()];
        let current = context(
            &fixture.catalog,
            QUERY,
            &current,
            &ContextOptions::default(),
        )
        .unwrap();
        assert!(!current.text().contains("violet permit"));
        assert!(current.text().contains("amber"));
        let mut history = historical_request(mode);
        let mixed = context(
            &fixture.catalog,
            QUERY,
            &history,
            &ContextOptions::default(),
        )
        .unwrap();
        assert!(
            mixed
                .passages()
                .iter()
                .any(|passage| passage.locator.path == path("page.md"))
        );
        assert!(mixed.text().contains("violet permit"));
        assert!(mixed.text().contains("amber"));
        assert!(mixed.usage().rendered_bytes <= history.budget.max_bytes);
        assert!(mixed.usage().estimated_tokens <= history.budget.max_tokens);
        history.documents.filters.source_ids = vec![fixture.source.clone()];
        let result = context(
            &fixture.catalog,
            QUERY,
            &history,
            &ContextOptions::default(),
        )
        .unwrap();
        assert!(result.text().contains("violet permit"));
        assert!(result.text().contains("amber"));
        assert!(result.text().contains("Citation (Historical)"));
        assert!(result.text().contains("Citation (Current)"));
        let mut revisions = BTreeSet::new();
        for passage in result.passages() {
            let original = fs::read(
                fixture
                    .catalog
                    .fs()
                    .root()
                    .path()
                    .join(passage.locator.path.as_str()),
            )
            .unwrap();
            assert_eq!(
                passage.text.as_bytes(),
                &original[passage.span.start() as usize..passage.span.end() as usize]
            );
            for citation in &passage.citations {
                let CitationRef::Source(reference) = citation else {
                    panic!("direct source citation")
                };
                assert_eq!(reference.source_id, fixture.source);
                assert_eq!(reference.span, passage.span);
                assert_eq!(
                    reference.quote_hash,
                    Blake3Hash::digest(passage.text.as_bytes())
                );
                revisions.insert(reference.source_revision.clone());
            }
        }
        assert_eq!(revisions.len(), 2);
        assert!(result.usage().rendered_bytes <= history.budget.max_bytes);
        assert!(result.usage().estimated_tokens <= history.budget.max_tokens);
    }
}
#[test]
fn historical_native_context_withdrawal_remains_authenticated_and_labeled() {
    let fixture = historical_fixture();
    let withdrawal = SourceStore::new(fixture.catalog.fs().clone())
        .plan_withdraw(&fixture.source, "Disposable historical fixture")
        .unwrap();
    Fixture::seed(fixture.catalog.fs(), withdrawal.draft.unwrap());
    fixture.republish();
    let mut request = historical_request(SearchMode::Lexical);
    request.documents.filters.source_ids = vec![fixture.source.clone()];
    let result = context(
        &fixture.catalog,
        QUERY,
        &request,
        &ContextOptions::default(),
    )
    .unwrap();
    assert!(result.text().contains("violet permit"));
    assert!(result.text().contains("Citation (Withdrawn)"));
    assert!(!result.text().contains("Citation (Current)"));
    request.documents.filters.include_historical = false;
    assert!(
        context(
            &fixture.catalog,
            QUERY,
            &request,
            &ContextOptions::default()
        )
        .unwrap()
        .passages()
        .is_empty()
    );
}
#[test]
fn historical_native_context_rejects_stale_dependencies_and_exhausted_proof() {
    for target in ["body", "source"] {
        let fixture = historical_fixture();
        let target = if target == "body" {
            fixture.content.clone()
        } else {
            path(&format!("sources/{}/source.md", fixture.source))
        };
        let full = fixture.catalog.fs().root().path().join(target.as_str());
        let mut bytes = fs::read(&full).unwrap();
        bytes.extend(b"External mutation retained.\n");
        let options = ContextOptions {
            fault: Some(Arc::new(EditBeforeEmission {
                path: full.clone(),
                bytes: bytes.clone(),
            })),
            ..Default::default()
        };
        assert_eq!(
            context(
                &fixture.catalog,
                QUERY,
                &historical_request(SearchMode::Lexical),
                &options
            )
            .unwrap_err()
            .code,
            ErrorCode::FreshnessConflict
        );
        assert_eq!(fs::read(full).unwrap(), bytes);
    }
    let fixture = historical_fixture();
    let mut request = historical_request(SearchMode::Lexical);
    request.verification_budget.max_files = 1;
    assert_eq!(
        context(
            &fixture.catalog,
            QUERY,
            &request,
            &ContextOptions::default()
        )
        .unwrap_err()
        .code,
        ErrorCode::BudgetExceeded
    );
}
#[test]
fn historical_native_context_rejects_vectors_host_selection_and_proposed() {
    for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
        assert_eq!(
            context::validate_request(QUERY, &historical_request(mode))
                .unwrap_err()
                .code,
            ErrorCode::Usage
        );
    }
    let fixture = Fixture::new();
    let reply = Fixture::reply(&fixture.prepare());
    for selection in [SelectionAction::Prepare, SelectionAction::Apply(reply)] {
        assert_eq!(
            context(
                &fixture.catalog,
                QUERY,
                &historical_request(SearchMode::Lexical),
                &ContextOptions {
                    selection,
                    ..Default::default()
                }
            )
            .unwrap_err()
            .code,
            ErrorCode::Usage
        );
    }
    let mut request = historical_request(SearchMode::Lexical);
    request.documents.filters.include_proposed = true;
    assert_eq!(
        context::validate_request(QUERY, &request).unwrap_err().code,
        ErrorCode::Usage
    );
}

#[test]
fn historical_native_context_filters_ineligible_owners_before_candidate_cap() {
    let fixture = historical_fixture();
    // These discovery matches must not consume the single context owner slot.
    for (file, kind, name, fields) in [
        (
            "draft-history.md",
            "page",
            "page_history_draft",
            json!({"wiki_status":"draft"}),
        ),
        (
            "invalid-history.md",
            "page",
            "page_history_invalid",
            json!({"wiki_status":"reviewed","wiki_depends_on_ids":["missing_history_dependency"]}),
        ),
        (
            "operational-history.md",
            "decision",
            "decision_history_noise",
            json!({"wiki_status":"active","wiki_action":"accept","wiki_created_at":"2026-10-04T00:00:00Z","wiki_input_ids":["assertion_host"],"wiki_output_ids":["assertion_host"]}),
        ),
        (
            "proposed-history.md",
            "assertion",
            "assertion_history_proposed",
            json!({"wiki_status":"proposed","wiki_subject_id":"entity_host","wiki_object_id":"entity_host","wiki_predicate":"uses"}),
        ),
    ] {
        fs::write(
            fixture.catalog.fs().root().path().join(file),
            note(
                kind,
                name,
                fields,
                b"historycapneedle historycapneedle historycapneedle\n",
            ),
        )
        .unwrap();
    }
    let plan = SourceStore::new(fixture.catalog.fs().clone())
        .plan_refresh(
            &fixture.source,
            capture(b"historycapneedle: eligible captured current answer.\n"),
        )
        .unwrap();
    Fixture::seed(fixture.catalog.fs(), plan.draft.unwrap());
    fixture.republish();
    for mode in [SearchMode::Literal, SearchMode::Lexical] {
        let mut request = historical_request(mode);
        request.documents.limits.candidates = 1;
        request.documents.limits.hits = 1;
        let result = context(
            &fixture.catalog,
            "historycapneedle",
            &request,
            &ContextOptions::default(),
        )
        .unwrap();
        assert!(result.text().contains("eligible captured current answer"));
        assert_eq!(result.passages().len(), 1);
        assert!(!result.passages()[0].citations.is_empty());
    }
}
