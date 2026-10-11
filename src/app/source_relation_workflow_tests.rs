//! Whole local workflow using the existing fictional preparation harness.
//! These are provider-free mechanics gates, not live compatibility or relevance acceptance.
use super::source_relation_preparation::tests::{Setup, run_id, setup, until};
use crate::{
    catalog::{
        query_types::{QueryCatalog, QueryReadLimits},
        source_relations::Candidate,
    },
    domain::*,
    providers::types::RemoteOperation,
    records::parse_note,
    retrieval::{
        ContextOptions, ContextRequest, ContextResult, ContextScope, SearchFilters, SearchMode,
        source_relation_packet as packet,
        source_relations::{AdmittedOriginal, SourceBinding},
        verification,
    },
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, revision::record_bytes},
};
use serde_json::json;
use std::{collections::BTreeSet, fs, sync::atomic::Ordering};

const QUERY: &str = "harbor permit";
const RULE: &str = "The authorization is conditional on the following procedure.";
const REQUIREMENT: &str = "Lattice escrow must be delivered before departure.";
const EXCEPTION: &str = "A cobalt exemption applies when the dock is closed.";
fn long_source() -> String {
    format!(
        "Harbor permit instructions begin here.\n\n{}\n\n{RULE}\n\n{}\n\n{REQUIREMENT}\n\n{}\n\n{EXCEPTION}\n",
        "Fictional background archive records routine weather and cargo.\n".repeat(180),
        "Unrelated interval observations remain merely contextual.\n".repeat(30),
        "Other historical inventory observations remain contextual.\n".repeat(30)
    )
}
/// Keep the existing source fixture and mock provider; replace only its complete
/// immutable input before any manifest is frozen or paid mock work is admitted.
fn scenario() -> (Setup, String) {
    let mut s = setup(1);
    let text = long_source();
    let (binding, captured) = &mut s.inputs[0];
    let original = captured
        .keys()
        .find(|p| p.as_str().ends_with("original.bin"))
        .unwrap()
        .clone();
    captured.insert(binding.content_path.clone(), text.as_bytes().to_vec());
    captured.insert(original, text.as_bytes().to_vec());
    let mut fields = parse_note(&captured[&binding.revision_path])
        .fields
        .unwrap();
    let hash = Blake3Hash::digest(&text);
    fields.insert("wiki_original_hash".into(), json!(hash));
    fields.insert("wiki_content_hash".into(), json!(hash));
    captured.insert(
        binding.revision_path.clone(),
        record_bytes(CanonicalRecord::new(fields).unwrap(), b"").unwrap(),
    );
    binding.content_hash = hash;
    binding.revision_hash = Blake3Hash::digest(&captured[&binding.revision_path]);
    for (path, bytes) in captured {
        fs::write(s.source.fs.root().resolve(path).unwrap(), bytes).unwrap();
    }
    s.app.index_rebuild_normalized().unwrap();
    let binding = &s.inputs[0].0;
    let source = SourceBinding {
        source_id: binding.source_id.clone(),
        source_revision: binding.revision_id.clone(),
        raw_hash: binding.content_hash.clone(),
    };
    let input = packet::build(
        AdmittedOriginal {
            binding: &source,
            text: &text,
        },
        1024,
        until(),
    )
    .unwrap();
    let RemoteOperation::Generate { data, .. } = &input.remote_input().operation else {
        unreachable!()
    };
    let data: serde_json::Value = serde_json::from_str(data).unwrap();
    let fragment = |needle: &str| {
        data["fragments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["text"].as_str().unwrap().contains(needle))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let rule = fragment(RULE);
    let requirement = fragment(REQUIREMENT);
    let exception = fragment(EXCEPTION);
    assert_eq!(
        [&rule, &requirement, &exception]
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    *s.mock.response_override.lock().unwrap()=Some(serde_json::to_string(&json!({
        "schema":packet::RAW_SCHEMA,
        "nodes":[{"id":"departure","kind":"rule","scope":"harbor permit","span_id":rule},
            {"id":"escrow","kind":"requirement","scope":"lattice escrow prerequisite","span_id":requirement},
            {"id":"closure","kind":"exception","scope":"cobalt closed dock exemption","span_id":exception}],
        "edges":[{"from":"departure","to":"escrow","kind":"requires","evidence_span_id":requirement},
            {"from":"departure","to":"closure","kind":"excepts","evidence_span_id":exception}]
    })).unwrap());
    (s, text)
}
fn request(s: &Setup) -> ContextRequest {
    let mut request = ContextRequest {
        scope: ContextScope::IndexedDocuments,
        ..ContextRequest::default()
    };
    request.documents.mode = SearchMode::Lexical;
    // The frozen relation workflow uses 1024-byte excerpts for both arms;
    // ContextRequest's inherited 240-byte default is a different input.
    request.documents.limits.excerpt_bytes = 1024;
    request.documents.filters = SearchFilters {
        source_ids: vec![s.inputs[0].0.source_id.clone()],
        ..SearchFilters::default()
    };
    request.verification_budget.max_elapsed_ms = 10_000;
    request
}
fn context(s: &Setup, experimental: bool, request: &ContextRequest) -> ContextResult {
    verification::context_with_options(
        &s.app.catalog(),
        None,
        QUERY,
        request,
        &ContextOptions {
            experimental_source_relations: experimental,
            ..ContextOptions::default()
        },
    )
    .unwrap()
}
fn candidates(s: &Setup, owner: &VaultRelativePath) -> Vec<Candidate> {
    let catalog = s.app.catalog();
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let owner = reader.document(owner).unwrap().unwrap();
    reader.source_relation_candidates(&owner, 80).unwrap()
}
fn prepare(s: &Setup) -> Candidate {
    let bytes = s
        .app
        .prepare_source_relation_manifest(
            std::slice::from_ref(&s.inputs[0].0.source_id),
            &s.runtime,
            1024,
            until(),
        )
        .unwrap();
    assert_eq!(
        s.mock.calls.load(Ordering::SeqCst),
        0,
        "frontend preflight must not dispatch"
    );
    let report = s
        .app
        .run_source_relation_preparation(&bytes, &run_id(), &s.runtime, until(), 1)
        .unwrap();
    assert_eq!((report.completed, report.failed, report.pending), (1, 0, 0));
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 1);
    let found = candidates(s, &s.inputs[0].0.content_path);
    assert_eq!(
        found.len(),
        1,
        "actual publication must install normalized owner hint"
    );
    found.into_iter().next().unwrap()
}
fn exact_originals(result: &ContextResult, text: &str, source: &RecordId, revision: &RecordId) {
    assert!(!result.network_used);
    assert!(result.passages().len() <= 4);
    for passage in result.passages() {
        assert_eq!(passage.text, passage.span.slice(text).unwrap());
        assert!(!passage.citations.is_empty());
        for citation in &passage.citations {
            let CitationRef::Source(reference) = citation else {
                panic!("source workflow emitted a non-source citation")
            };
            assert_eq!(
                (&reference.source_id, &reference.source_revision),
                (source, revision)
            );
            assert_eq!(reference.span, passage.span);
            assert_eq!(reference.quote_hash, Blake3Hash::digest(&passage.text));
        }
    }
}
/// Publication and cache reconstruction change the discovery snapshot identity.
/// Permit only its generation number in this exact indexed-documents header;
/// every qualifier, remaining rendered byte and complete passage stays equal.
fn assert_same_evidence_across_publication(before: &ContextResult, after: &ContextResult) {
    assert_ne!(before.snapshot(), after.snapshot());
    let header = |result: &ContextResult| {
        format!(
            "[context indexed_documents; discovery generation {}; selected document dependencies verified; global membership, identity uniqueness and completeness not verified]\n\n",
            result.snapshot().generation
        )
    };
    let before_header = header(before);
    let after_header = header(after);
    let before_remainder = before
        .text()
        .strip_prefix(&before_header)
        .expect("before render must contain its exact snapshot generation header");
    let after_remainder = after
        .text()
        .strip_prefix(&after_header)
        .expect("after render must contain its exact snapshot generation header");
    assert_eq!(before_remainder.as_bytes(), after_remainder.as_bytes());
    assert_eq!(before.passages(), after.passages());
}
fn has(result: &ContextResult, needle: &str) -> bool {
    result.passages().iter().any(|p| p.text.contains(needle))
}
#[test]
fn frontend_publication_distant_dependencies_and_offline_rebuild_preserve_original_evidence() {
    let (mut s, text) = scenario();
    let request = request(&s);
    let baseline = context(&s, false, &request);
    let absent = context(&s, true, &request);
    assert_eq!(baseline.text(), absent.text());
    assert_eq!(baseline.passages(), absent.passages());
    assert!(!has(&baseline, REQUIREMENT));
    assert!(!has(&baseline, EXCEPTION));
    let chosen = prepare(&s);
    let disabled = context(&s, false, &request);
    assert_same_evidence_across_publication(&baseline, &disabled);
    let prepared = context(&s, true, &request);
    for marker in [RULE, REQUIREMENT, EXCEPTION] {
        assert!(
            has(&prepared, marker),
            "missing distant dependency {marker}"
        );
    }
    exact_originals(
        &prepared,
        &text,
        &s.inputs[0].0.source_id,
        &s.inputs[0].0.revision_id,
    );
    assert!(prepared.warnings().iter().any(|w| w.contains("unassessed")));
    let mut small = request.clone();
    let group_text_bytes: usize = prepared
        .passages()
        .iter()
        .filter(|p| {
            [RULE, REQUIREMENT, EXCEPTION]
                .iter()
                .any(|marker| p.text.contains(marker))
        })
        .map(|p| p.text.len())
        .sum();
    small.budget.max_bytes = group_text_bytes.saturating_sub(1);
    let limited = context(&s, true, &small);
    for marker in [RULE, REQUIREMENT, EXCEPTION] {
        assert!(
            !has(&limited, marker),
            "over-budget relation group must be omitted whole"
        );
    }
    // This removes descriptor, normalized catalog and all other disposable caches.
    // Offline index reconstruction receives no runtime/service/provider argument.
    let annotation =
        fs::read(s.source.fs.root().resolve(&chosen.annotation.path).unwrap()).unwrap();
    let raw = fs::read(s.source.fs.root().resolve(&chosen.raw.path).unwrap()).unwrap();
    fs::remove_dir_all(s.source.fs.root().path().join(".wiki/cache")).unwrap();
    s.app.options.offline = true;
    s.app.index_rebuild_normalized().unwrap();
    let rebuilt = context(&s, true, &request);
    assert_same_evidence_across_publication(&prepared, &rebuilt);
    assert_eq!(candidates(&s, &s.inputs[0].0.content_path)[0], chosen);
    assert_eq!(
        fs::read(s.source.fs.root().resolve(&chosen.annotation.path).unwrap()).unwrap(),
        annotation
    );
    assert_eq!(
        fs::read(s.source.fs.root().resolve(&chosen.raw.path).unwrap()).unwrap(),
        raw
    );
    assert_eq!(
        s.mock.calls.load(Ordering::SeqCst),
        1,
        "query and offline rebuild must not dispatch"
    );
}
#[test]
fn refresh_and_withdraw_invalidate_navigation_while_retaining_historical_preparation_bytes() {
    let (mut s, _) = scenario();
    let chosen = prepare(&s);
    let old_owner = s.inputs[0].0.content_path.clone();
    let source = s.inputs[0].0.source_id.clone();
    let old_raw = fs::read(s.source.fs.root().resolve(&chosen.raw.path).unwrap()).unwrap();
    let old_annotation =
        fs::read(s.source.fs.root().resolve(&chosen.annotation.path).unwrap()).unwrap();
    s.app.options.offline = true;
    let updated = "Harbor permit now requires a fresh amber certificate.\n";
    let refreshed = s
        .app
        .source_refresh(
            source.clone(),
            CaptureRequest {
                title: "Fictional refreshed permit".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "fiction.txt".into(),
                original: updated.as_bytes().to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: None,
            },
        )
        .unwrap();
    let revision = &refreshed.allocated_ids["revision"];
    let owner = VaultRelativePath::new(format!("sources/{source}/revisions/{revision}/content.md"))
        .unwrap();
    assert!(candidates(&s, &old_owner).is_empty());
    assert!(candidates(&s, &owner).is_empty());
    let request = request(&s);
    let ordinary = context(&s, false, &request);
    let experimental = context(&s, true, &request);
    assert_eq!(ordinary.text(), experimental.text());
    assert_eq!(ordinary.passages(), experimental.passages());
    assert!(has(&experimental, "fresh amber certificate"));
    exact_originals(&experimental, updated, &source, revision);
    s.app
        .source_withdraw(source.clone(), "Fictional permit retired")
        .unwrap();
    assert!(candidates(&s, &owner).is_empty());
    let withdrawn = context(&s, true, &request);
    assert!(withdrawn.passages().is_empty());
    assert_eq!(
        fs::read(s.source.fs.root().resolve(&chosen.raw.path).unwrap()).unwrap(),
        old_raw
    );
    assert_eq!(
        fs::read(s.source.fs.root().resolve(&chosen.annotation.path).unwrap()).unwrap(),
        old_annotation
    );
    assert_eq!(s.mock.calls.load(Ordering::SeqCst), 1);
}
