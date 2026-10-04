//! Normalized host selection keeps the same closed selected canonical proof.
use super::*;
use crate::{
    changes::ChangeDraft,
    records::{edit_note, parse_note},
    retrieval::context_selection_packet::{
        SelectionAction, SelectionCard, SelectionPacket, SelectionReply,
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
            query: Some(query),
            signals: &ContextSelectionSignals::default(),
            selection_action: &SelectionAction::Automatic,
            hits: &hits,
            graph: None,
            dependency_fingerprint: proof.fingerprint.clone(),
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
