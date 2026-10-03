use lwiki as library;
use lwiki::{
    catalog::*,
    changes::*,
    domain::*,
    graph::{extraction_types::*, packet::*},
    retrieval::{lexical::lexical_expression, render, spaces::EmbeddingSettings, *},
    sources::{evidence::exact_quote_body, *},
    vault::*,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};

#[path = "fixtures/p18/common.rs"]
mod api_fixture;
#[allow(dead_code)]
#[path = "fixtures/p16c/common.rs"]
mod provider;

fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
fn relative(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn write(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn note(kind: &str, name: &str, title: &str, extra: Value, body: &str) -> Vec<u8> {
    let mut fields = BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_id".into(), json!(name)),
        ("wiki_kind".into(), json!(kind)),
        ("title".into(), json!(title)),
    ]);
    fields.extend(
        extra
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    CanonicalRecord::new(fields.clone()).unwrap();
    let mut bytes = b"---\n".to_vec();
    for (key, value) in fields {
        bytes.extend_from_slice(format!("{key}: {value}\n").as_bytes());
    }
    bytes.extend_from_slice(b"---\n");
    bytes.extend_from_slice(body.as_bytes());
    bytes
}
fn fixture() -> (tempfile::TempDir, VaultRoot, Catalog) {
    let temp = tempfile::tempdir().unwrap();
    write(
        temp.path(),
        "WIKI.md",
        &note("vault", "vault_search", "Search", json!({}), "Wiki\n"),
    );
    let root = VaultRoot::explicit(temp.path()).unwrap();
    let catalog = Catalog::new(VaultFs::new(root.clone()), id("vault_search"));
    (temp, root, catalog)
}
fn reader(root: &VaultRoot, catalog: &Catalog) -> ReaderSnapshot {
    let writer = WriterPermit::acquire(root, Duration::from_millis(200)).unwrap();
    catalog.sync(&writer).unwrap();
    catalog.verified_snapshot(None).unwrap()
}
fn page(root: &Path, path: &str, name: &str, title: &str, body: &str, tags: &[&str]) {
    write(
        root,
        path,
        &note(
            "page",
            name,
            title,
            json!({"wiki_status":"reviewed","tags":tags}),
            body,
        ),
    );
}
fn literal_plan() -> QueryPlan {
    QueryPlan {
        mode: SearchMode::Literal,
        ..Default::default()
    }
}

#[test]
fn literal_vec_t_e0308_symbols() {
    let (temp, root, catalog) = fixture();
    write(
        temp.path(),
        "symbols.md",
        "Vec<T> E0308 e\u{301} λ → :: * Foo".as_bytes(),
    );
    let r = reader(&root, &catalog);
    for query in ["Vec<T>", "E0308", "e\u{301}", "λ → :: *"] {
        let hits = search(&r, query, &literal_plan()).unwrap();
        assert_eq!(hits.hits.len(), 1);
        let hit = &hits.hits[0];
        assert_eq!(
            hit.excerpt
                .span
                .slice(&fs::read_to_string(temp.path().join("symbols.md")).unwrap())
                .unwrap(),
            hit.excerpt.text
        );
        assert!(hit.excerpt.citation.is_none());
        assert_eq!(hit.excerpt.label, ExcerptLabel::NoteText);
    }
    assert!(search(&r, "foo", &literal_plan()).unwrap().hits.is_empty());
}

#[test]
fn lexical_excerpt_prefers_full_name_after_many_partial_matches() {
    let (temp, root, catalog) = fixture();
    let body = format!(
        "{}\n## Hungry Howie's Pizza\nPizza for the whole family.\n",
        "Show up hungry at another restaurant.\n".repeat(100)
    );
    write(temp.path(), "restaurants.md", body.as_bytes());
    let r = reader(&root, &catalog);
    let hits = search(&r, "Hungry Howie", &QueryPlan::default()).unwrap();
    assert_eq!(hits.hits.len(), 1);
    let excerpt = &hits.hits[0].excerpt;
    assert!(excerpt.text.contains("Hungry Howie's Pizza"));
    assert_eq!(excerpt.span.slice(&body).unwrap(), excerpt.text);
    assert_eq!(excerpt.matched_spans.len(), 1);
    assert_eq!(
        excerpt.matched_spans[0].slice(&body).unwrap(),
        "Hungry Howie"
    );
}

#[test]
fn lexical_question_focuses_late_identifiers_after_frequent_first_words() {
    let (_temp, root, catalog) = fixture();
    let raw = format!(
        "# Intro\n\n{}\n## Subprocess fixtures\n\nFor CLI testing, assert_cmd checks a child process and assert_fs supplies test files.\n",
        "How a program runs: naïve λ introductory notes.\n".repeat(160)
    );
    let captured = capture(&root, &catalog, raw.as_bytes());
    let r = catalog.verified_snapshot(None).unwrap();
    let mut plan = QueryPlan::default();
    plan.filters.source_ids.push(captured.source_id.clone());
    let query = "How do assert_cmd and assert_fs help test a Rust command line program?";
    let hits = search(&r, query, &plan).unwrap();
    let hit = hits
        .hits
        .iter()
        .find(|hit| hit.owner_revision == Some(captured.revision_id.clone()))
        .unwrap();
    assert!(hit.excerpt.text.contains("assert_cmd"));
    assert!(hit.excerpt.text.contains("assert_fs"));
    assert_eq!(hit.excerpt.span.slice(&raw).unwrap(), hit.excerpt.text);
    assert!(hit.excerpt.text.len() <= plan.limits.excerpt_bytes);
    assert!(hit.excerpt.matched_spans.len() <= 64);
    for span in &hit.excerpt.matched_spans {
        assert!(span.start() >= hit.excerpt.span.start());
        assert!(span.end() <= hit.excerpt.span.end());
        span.slice(&raw).unwrap();
    }
    let citation = hit.excerpt.citation.as_ref().unwrap();
    let view = SourceView::from_fs(catalog.fs()).unwrap();
    assert_eq!(
        view.verify(citation, CitationScope::Current).unwrap().quote,
        hit.excerpt.text.as_bytes()
    );
    assert_eq!(
        search(&r, query, &plan).unwrap().hits[0].excerpt.span,
        hits.hits[0].excerpt.span
    );
}

#[test]
fn lexical_excerpt_weights_distinct_overlap_without_language_stopwords() {
    let (temp, root, catalog) = fixture();
    let raw = format!(
        "{}\n## Поздний раздел\n\nнастройка спутника проверяет маркер Ωmega.\n",
        "как работает общий пример как работает общий пример\n".repeat(150)
    );
    write(temp.path(), "manual.md", raw.as_bytes());
    let r = reader(&root, &catalog);
    let query = "как работает настройка спутника маркер Ωmega";
    let mut plan = QueryPlan::default();
    plan.limits.excerpt_bytes = 160;
    let hits = search(&r, query, &plan).unwrap();
    let excerpt = &hits.hits[0].excerpt;
    assert!(excerpt.text.contains("настройка спутника"));
    assert!(excerpt.text.contains("Ωmega"));
    assert!(excerpt.text.len() <= 160);
    assert_eq!(excerpt.span.slice(&raw).unwrap(), excerpt.text);
    assert!(excerpt.matched_spans.len() >= 4);
}

#[test]
fn lexical_excerpt_selects_late_cluster_after_more_than_64_repeats() {
    let (temp, root, catalog) = fixture();
    let raw = format!(
        "{}\n{}\n{}\nHow alpha and beta work with shared state.\n",
        "How shared introductory material works.\n".repeat(100),
        "alpha alone describes a different topic.\n".repeat(100),
        "beta alone describes another topic.\n".repeat(100)
    );
    write(temp.path(), "cluster.md", raw.as_bytes());
    let r = reader(&root, &catalog);
    let query = "How shared beta alpha";
    let hits = search(&r, query, &QueryPlan::default()).unwrap();
    assert!(
        hits.hits[0]
            .excerpt
            .text
            .contains("How alpha and beta work with shared state.")
    );
    assert_eq!(
        hits.hits[0].excerpt.span.slice(&raw).unwrap(),
        hits.hits[0].excerpt.text
    );
}

#[test]
fn lexical_focus_does_not_drop_literal_case_and_punctuation_match() {
    let (_temp, root, catalog) = fixture();
    let raw = format!(
        "{}\nExact identifier: Should_Panic() checks the requested behavior.\n",
        "how a common introductory word behaves naïvely\n".repeat(100)
    );
    let captured = capture(&root, &catalog, raw.as_bytes());
    let r = catalog.verified_snapshot(None).unwrap();
    let mut plan = literal_plan();
    plan.filters.source_ids.push(captured.source_id.clone());
    let hits = search(&r, "Should_Panic()", &plan).unwrap();
    let hit = hits.hits.first().unwrap();
    assert!(hit.excerpt.text.contains("Should_Panic()"));
    assert_eq!(hit.excerpt.matched_spans.len(), 1);
    assert_eq!(
        hit.excerpt.matched_spans[0].slice(&raw).unwrap(),
        "Should_Panic()"
    );
    assert_eq!(hit.excerpt.span.slice(&raw).unwrap(), hit.excerpt.text);
    assert!(search(&r, "should_panic()", &plan).unwrap().hits.is_empty());
    let view = SourceView::from_fs(catalog.fs()).unwrap();
    assert_eq!(
        view.verify(
            hit.excerpt.citation.as_ref().unwrap(),
            CitationScope::Current
        )
        .unwrap()
        .quote,
        hit.excerpt.text.as_bytes()
    );
}

#[test]
fn lexical_or_colon_star_quotes_are_data() {
    let (temp, root, catalog) = fixture();
    write(
        temp.path(),
        "syntax.md",
        b"OR foo:bar a* quoted\"word ordinary phrase\n",
    );
    write(temp.path(), "other.md", b"unrelated text\n");
    let r = reader(&root, &catalog);
    for query in [
        "OR",
        "foo:bar",
        "a*",
        "quoted\"word",
        "\"ordinary\"",
        "OR foo:bar a*",
    ] {
        let hits = search(&r, query, &QueryPlan::default()).unwrap();
        assert_eq!(hits.hits.len(), 1, "{query}");
        assert_eq!(hits.hits[0].locator.path, relative("syntax.md"));
    }
    assert_eq!(
        lexical_expression("OR foo:bar a* \"word\"").unwrap(),
        "\"OR\" OR \"foo:bar\" OR \"a*\" OR \"\"\"word\"\"\""
    );
    let error = search(&r, ":: ** →", &QueryPlan::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
    assert!(error.hint.unwrap().contains("literal"));
}

#[test]
fn filter_before_limit_and_stable_ties() {
    let (temp, root, catalog) = fixture();
    for n in 0..12 {
        page(
            temp.path(),
            &format!("excluded/{n}.md"),
            &format!("excluded_{n:02}"),
            "Needle needle",
            "needle needle needle",
            &["wrong"],
        );
    }
    for name in ["b", "a"] {
        page(
            temp.path(),
            &format!("allowed/{name}.md"),
            name,
            "Equal",
            "needle",
            &["keep", "both"],
        );
    }
    let r = reader(&root, &catalog);
    let mut plan = QueryPlan::default();
    plan.limits.hits = 1;
    plan.limits.candidates = 2;
    plan.filters = SearchFilters {
        kinds: vec![RecordKind::Page],
        tags: vec!["keep".into(), "both".into()],
        path_prefix: Some("allowed/".into()),
        authored_statuses: vec!["reviewed".into()],
        ..Default::default()
    };
    let first = search(&r, "needle", &plan).unwrap();
    assert_eq!(
        first.hits[0].locator.record.as_ref().unwrap().record_id,
        id("a")
    );
    assert!(first.truncated);
    plan.cursor = first.next_cursor;
    let second = search(&r, "needle", &plan).unwrap();
    assert_eq!(
        second.hits[0].locator.record.as_ref().unwrap().record_id,
        id("b")
    );
    assert!(!second.truncated);
}

#[test]
fn cursor_query_or_generation_change_is_stale() {
    let (temp, root, catalog) = fixture();
    for name in ["a", "b"] {
        page(
            temp.path(),
            &format!("{name}.md"),
            name,
            "Equal",
            "needle",
            &[],
        );
    }
    let r = reader(&root, &catalog);
    let mut plan = QueryPlan::default();
    plan.limits.hits = 1;
    plan.cursor = search(&r, "needle", &plan).unwrap().next_cursor;
    assert_eq!(
        search(&r, "other", &plan).unwrap_err().code,
        ErrorCode::CursorStale
    );
    let mut changed = plan.clone();
    changed.filters.tags.push("new".into());
    assert_eq!(
        search(&r, "needle", &changed).unwrap_err().code,
        ErrorCode::CursorStale
    );
    changed = plan.clone();
    changed.limits.excerpt_bytes += 1;
    assert_eq!(
        search(&r, "needle", &changed).unwrap_err().code,
        ErrorCode::CursorStale
    );
    write(temp.path(), "new.md", b"needle");
    let next = reader(&root, &catalog);
    assert_eq!(
        search(&next, "needle", &plan).unwrap_err().code,
        ErrorCode::CursorStale
    );
    plan.cursor = Some("not-a-cursor".into());
    assert_eq!(
        search(&r, "needle", &plan).unwrap_err().code,
        ErrorCode::CursorStale
    );
}

#[test]
fn invalid_notes_remain_literal_discovery() {
    let (temp, root, catalog) = fixture();
    let raw =
        b"---\nwiki_schema: \"2\"\nwiki_id: future\ninvalid: [unfinished\n---\nRareSymbol<T>\n";
    write(temp.path(), "invalid.md", raw);
    let r = reader(&root, &catalog);
    let hits = search(&r, "RareSymbol<T>", &literal_plan()).unwrap();
    assert_eq!(hits.hits.len(), 1);
    let hit = &hits.hits[0];
    assert!(hit.locator.record.is_none());
    assert_eq!(hit.eligibility, Eligibility::Invalid);
    assert_eq!(hit.locator.observed_hash, Blake3Hash::digest(raw));
    assert!(hit.excerpt.citation.is_none());
    assert!(
        hits.warnings
            .iter()
            .any(|w| w.contains("invalid.md") && w.contains("RECORD_INVALID"))
    );
    let lexical = search(&r, "RareSymbol", &QueryPlan::default()).unwrap();
    assert!(
        lexical
            .hits
            .iter()
            .any(|h| h.locator.path == relative("invalid.md"))
    );
    assert!(lexical.warnings.iter().any(|w| w.contains("invalid.md")));
    assert!(
        search(&r, "Wiki", &QueryPlan::default())
            .unwrap()
            .warnings
            .iter()
            .all(|w| !w.contains("invalid.md"))
    );
}

#[test]
fn malformed_hit_warnings_are_bounded_and_do_not_repeat_diagnostic_text() {
    let (temp, root, catalog) = fixture();
    for i in 0..5 {
        write(
            temp.path(),
            &format!("broken-{i}.md"),
            b"---\nwiki_schema: \"2\"\ninvalid: [unfinished\n---\nNoisyNeedle\n",
        );
    }
    let r = reader(&root, &catalog);
    let result = search(&r, "NoisyNeedle", &QueryPlan::default()).unwrap();
    assert_eq!(result.hits.len(), 5);
    assert!(
        result
            .hits
            .iter()
            .all(|h| h.eligibility == Eligibility::Invalid && h.excerpt.citation.is_none())
    );
    assert_eq!(result.warnings.len(), 4);
    assert!(
        result
            .warnings
            .iter()
            .take(3)
            .all(|w| w.contains("invalid_note_hit:")
                && w.contains("RECORD_INVALID")
                && !w.contains("unfinished"))
    );
    assert!(result.warnings[3].contains("2 additional invalid note hits"));
}

#[test]
fn malformed_packet_path_requires_explicit_historical_audit() {
    let (temp, root, catalog) = fixture();
    let malformed = b"---\nwiki_kind: extraction_packet\nwiki_id: packet_broken\ninvalid: [unfinished\n---\nCEDAR-731 retained window\n";
    write(
        temp.path(),
        "knowledge/extractions/packets/broken.md",
        malformed,
    );
    write(
        temp.path(),
        "ordinary-invalid.md",
        b"---\nwiki_kind: page\nwiki_id: broken_page\ninvalid: [unfinished\n---\nCEDAR-731 ordinary invalid note\n",
    );
    write(temp.path(), "copied-malformed-packet.md", malformed);
    let r = reader(&root, &catalog);
    let hits = search(&r, "CEDAR-731", &literal_plan()).unwrap();
    assert_eq!(hits.hits.len(), 1);
    assert_eq!(hits.hits[0].locator.path, relative("ordinary-invalid.md"));
    assert_eq!(hits.hits[0].eligibility, Eligibility::Invalid);
    let mut audit = literal_plan();
    audit.filters.include_historical = true;
    assert!(
        search(&r, "CEDAR-731", &audit)
            .unwrap()
            .hits
            .iter()
            .any(|hit| hit.locator.path == relative("knowledge/extractions/packets/broken.md"))
    );
}

#[test]
fn copied_packet_with_duplicate_id_never_becomes_invalid_note_source() {
    let (temp, root, catalog) = fixture();
    let captured = capture(&root, &catalog, b"CEDAR-731 source window");
    let view = SourceView::from_fs(catalog.fs()).unwrap();
    let packet = build_packet(
        &view,
        &ExportRequest {
            source_id: captured.source_id.clone(),
            revision_id: Some(captured.revision_id.clone()),
            windows: vec![],
            limits: ExtractionLimits::default(),
            candidate_context: vec![],
        },
    )
    .unwrap();
    let packet_path = packet.locator.path.clone();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let writer = WriterPermit::acquire(&root, Duration::from_millis(200)).unwrap();
    let prepared = engine
        .prepare(&writer, packet.draft.unwrap())
        .unwrap()
        .prepared;
    engine
        .apply(&writer, &prepared, &CatalogGraphValidator, &catalog)
        .unwrap();
    drop(writer);
    fs::copy(
        root.path().join(packet_path.as_str()),
        temp.path().join("copied-packet.md"),
    )
    .unwrap();

    let before = reader(&root, &catalog);
    assert!(
        !before
            .projection()
            .records
            .contains_key(&packet.packet.packet_id)
    );
    let hits = search(&before, "CEDAR-731", &literal_plan()).unwrap();
    assert_eq!(
        hits.hits.len(),
        1,
        "only the captured source is default-searchable"
    );
    assert_eq!(
        hits.hits[0].owner_revision,
        Some(captured.revision_id.clone())
    );

    let withdraw = SourceStore::new(VaultFs::new(root.clone()))
        .plan_withdraw(&captured.source_id, "fixture withdrawal")
        .unwrap();
    let writer = WriterPermit::acquire(&root, Duration::from_millis(200)).unwrap();
    let prepared = engine
        .prepare(&writer, withdraw.draft.unwrap())
        .unwrap()
        .prepared;
    engine
        .apply(&writer, &prepared, &CatalogGraphValidator, &catalog)
        .unwrap();
    drop(writer);
    let after = reader(&root, &catalog);
    assert!(
        search(&after, "CEDAR-731", &literal_plan())
            .unwrap()
            .hits
            .is_empty()
    );
}

#[test]
fn older_packet_projection_requires_sync_before_index_snapshot() {
    let (temp, root, catalog) = fixture();
    page(temp.path(), "page.md", "page", "Page", "CEDAR-731", &[]);
    let first = reader(&root, &catalog);
    let old = Blake3Hash::digest(b"previous packet projection semantics");
    assert_ne!(first.snapshot().parser_fingerprint, old);
    drop(first);
    let cache = root.path().join(".wiki/cache/index.sqlite");
    let connection = rusqlite::Connection::open(cache).unwrap();
    let (generation, serialized): (i64, String) = connection
        .query_row(
            "SELECT g.gen,g.projection_json FROM generations g JOIN index_meta m ON g.gen=m.published_gen",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut projection: CatalogProjection = serde_json::from_str(&serialized).unwrap();
    projection.parser_fingerprint = old.clone();
    connection
        .execute(
            "UPDATE generations SET parser_hash=?1,projection_json=?2 WHERE gen=?3",
            rusqlite::params![
                old.as_str(),
                serde_json::to_string(&projection).unwrap(),
                generation
            ],
        )
        .unwrap();
    drop(connection);
    assert_eq!(
        catalog.index_snapshot().err().unwrap().code,
        ErrorCode::OfflineUnavailable
    );
    let writer = WriterPermit::acquire(&root, Duration::from_millis(200)).unwrap();
    let current = catalog.verified_snapshot(Some(&writer)).unwrap();
    assert_eq!(
        current.snapshot().parser_fingerprint,
        lwiki::catalog::scan::parser_fingerprint()
    );
    assert_eq!(
        search(&current, "CEDAR-731", &literal_plan())
            .unwrap()
            .hits
            .len(),
        1
    );
}

#[test]
fn exact_ids_and_homonym_labels_rank_without_merging() {
    let (temp, root, catalog) = fixture();
    page(temp.path(), "id.md", "needle", "Unrelated", "Nothing", &[]);
    page(temp.path(), "label.md", "label", "needle", "Nothing", &[]);
    page(
        temp.path(),
        "alias.md",
        "alias",
        "Alias owner",
        "Nothing",
        &[],
    );
    let alias = note(
        "page",
        "alias",
        "Alias owner",
        json!({"wiki_status":"reviewed","aliases":["needle"]}),
        "Nothing",
    );
    write(temp.path(), "alias.md", &alias);
    page(temp.path(), "text.md", "text", "Text", "needle", &[]);
    let r = reader(&root, &catalog);
    let hits = search(&r, "needle", &QueryPlan::default()).unwrap();
    assert_eq!(
        hits.hits[0].locator.record.as_ref().unwrap().record_id,
        id("needle")
    );
    assert!(hits.hits[0].reasons.contains(&RetrievalReason::ExactId));
    assert_eq!(hits.hits.len(), 4);
    let middle: Vec<_> = hits.hits[1..3]
        .iter()
        .map(|h| h.locator.record.as_ref().unwrap().record_id.clone())
        .collect();
    assert!(middle.contains(&id("label")) && middle.contains(&id("alias")));
    assert!(hits.hits[3].reasons.contains(&RetrievalReason::Lexical));
    let serialized = serde_json::to_value(&hits.hits[0]).unwrap();
    assert_eq!(serialized["record_ref"]["record_id"], "needle");
    assert_eq!(serialized["path"], "id.md");
}

#[test]
fn current_identity_seeds_exclude_unsupported_description() {
    let (temp, root, catalog) = fixture();
    write(
        temp.path(),
        "entity.md",
        &note(
            "entity",
            "entity_id",
            "Unique Atlas",
            json!({"wiki_status":"active","wiki_entity_type":"concept","aliases":["Atlas alias"]}),
            "SecretUnsupportedDescription\n",
        ),
    );
    let r = reader(&root, &catalog);
    let hits = search(&r, "Atlas", &QueryPlan::default()).unwrap();
    assert_eq!(hits.hits.len(), 1);
    assert_eq!(
        hits.hits[0].identity_eligibility,
        Some(Eligibility::Current)
    );
    assert_eq!(hits.hits[0].eligibility, Eligibility::Unsupported);
    assert!(hits.hits[0].reasons.contains(&RetrievalReason::Identity));
    assert!(hits.hits[0].excerpt.text.is_empty());
    assert!(hits.hits[0].excerpt.span.is_empty());
    assert!(hits.hits[0].excerpt.citation.is_none());
    for label in ["Unique Atlas", "Atlas alias", "entity_id"] {
        let identity = search(&r, label, &QueryPlan::default()).unwrap();
        assert_eq!(identity.hits.len(), 1);
        assert!(identity.hits[0].excerpt.text.is_empty());
        assert!(
            identity.hits[0]
                .reasons
                .contains(&RetrievalReason::Identity)
        );
    }
    assert!(
        search(&r, "SecretUnsupportedDescription", &QueryPlan::default())
            .unwrap()
            .hits
            .is_empty()
    );
    let mut history = QueryPlan::default();
    history.filters.include_historical = true;
    assert_eq!(
        search(&r, "SecretUnsupportedDescription", &history)
            .unwrap()
            .hits
            .len(),
        1
    );
}

fn capture(root: &VaultRoot, catalog: &Catalog, text: &[u8]) -> SourcePlan {
    let writer = WriterPermit::acquire(root, Duration::from_millis(200)).unwrap();
    let store = SourceStore::new(VaultFs::new(root.clone()));
    let plan = store
        .plan_capture(CaptureRequest {
            title: "Capture".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture".into(),
            original: text.to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let prepared = engine
        .prepare(&writer, plan.draft.clone().unwrap())
        .unwrap()
        .prepared;
    engine
        .apply(&writer, &prepared, &CatalogGraphValidator, catalog)
        .unwrap();
    plan
}

#[test]
fn original_source_spans_and_unicode_tokenizer_are_exact() {
    let (_temp, root, catalog) = fixture();
    let raw = "Intro\r\n# Café\r\nAlpha **naïve** &amp; omega.\r\n";
    let plan = capture(&root, &catalog, raw.as_bytes());
    let r = catalog.verified_snapshot(None).unwrap();
    let mut query = QueryPlan::default();
    query.filters.source_ids.push(plan.source_id.clone());
    for term in ["cafe", "naive", "omega"] {
        let hits = search(&r, term, &query).unwrap();
        let hit = hits
            .hits
            .iter()
            .find(|h| h.owner_revision == Some(plan.revision_id.clone()))
            .unwrap();
        assert_eq!(hit.kind, None);
        assert_eq!(
            hit.locator.record.as_ref().unwrap().expected_kind,
            RecordKind::Revision
        );
        assert_eq!(hit.excerpt.span.slice(raw).unwrap(), hit.excerpt.text);
        assert!(!hit.excerpt.matched_spans.is_empty());
        let citation = hit.excerpt.citation.as_ref().unwrap();
        let view = SourceView::from_fs(catalog.fs()).unwrap();
        assert_eq!(
            view.verify(citation, CitationScope::Current).unwrap().quote,
            hit.excerpt.text.as_bytes()
        );
        assert_eq!(hit.excerpt.label, ExcerptLabel::CapturedSource);
    }
    let index = catalog.index_snapshot().unwrap();
    let hits = search(&index, "cafe", &query).unwrap();
    assert!(hits.hits.iter().all(|h| h.excerpt.citation.is_none()));
    assert_eq!(hits.verification, SnapshotVerification::IndexSnapshot);
}

#[test]
fn query_and_candidate_bounds_are_explicit() {
    let (temp, root, catalog) = fixture();
    let many = std::iter::repeat_n("word", 257)
        .collect::<Vec<_>>()
        .join(" ");
    write(temp.path(), "many.md", many.as_bytes());
    for n in 0..90 {
        write(temp.path(), &format!("hits/{n:03}.md"), b"needle");
    }
    let r = reader(&root, &catalog);
    assert_eq!(
        search(&r, &many, &QueryPlan::default()).unwrap_err().code,
        ErrorCode::Usage
    );
    assert_eq!(search(&r, &many, &literal_plan()).unwrap().hits.len(), 1);
    assert_eq!(
        search(&r, &"a".repeat(16 * 1024 + 1), &literal_plan())
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );
    assert_eq!(
        search(&r, " \n ", &literal_plan()).unwrap_err().code,
        ErrorCode::Usage
    );
    let mut plan = QueryPlan::default();
    plan.limits.hits = 50;
    let first = search(&r, "needle", &plan).unwrap();
    assert_eq!(first.hits.len(), 50);
    assert!(first.truncated && first.omitted_candidates >= 1);
    plan.cursor = first.next_cursor;
    let second = search(&r, "needle", &plan).unwrap();
    assert_eq!(second.hits.len(), 30);
    assert!(second.next_cursor.is_none() && second.truncated);
}

#[test]
fn long_scenario_query_keeps_its_last_requirement() {
    let (temp, root, catalog) = fixture();
    write(
        temp.path(),
        "answer.md",
        b"The latequalifier applies here.\n",
    );
    write(temp.path(), "distractor.md", b"Other unrelated material.\n");
    let r = reader(&root, &catalog);
    // Full scenarios may exceed 64 words. The last requirement must still
    // participate in discovery instead of being silently dropped.
    let mut terms = (0..255).map(|n| format!("absent{n}")).collect::<Vec<_>>();
    terms.push("latequalifier".into());
    let hits = search(&r, &terms.join(" "), &QueryPlan::default()).unwrap();
    assert_eq!(hits.hits.len(), 1);
    assert_eq!(hits.hits[0].locator.path, relative("answer.md"));
    assert!(hits.hits[0].excerpt.text.contains("latequalifier"));
    // The full byte allowance is accepted independently of the term allowance.
    let mut padded = terms.join(" ");
    padded.push_str(&" ".repeat(16 * 1024 - padded.len()));
    assert_eq!(
        search(&r, &padded, &QueryPlan::default()).unwrap().hits[0]
            .locator
            .path,
        relative("answer.md")
    );
    terms.push("excess".into());
    assert_eq!(
        search(&r, &terms.join(" "), &QueryPlan::default())
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );
}

#[test]
fn unicode_query_limit_counts_utf8_bytes_and_preserves_literal_text() {
    let (temp, root, catalog) = fixture();
    let query = "界".repeat(5461) + "x";
    assert_eq!(query.len(), 16 * 1024);
    write(temp.path(), "unicode.md", query.as_bytes());
    let r = reader(&root, &catalog);
    assert_eq!(search(&r, &query, &literal_plan()).unwrap().hits.len(), 1);
    assert!(lexical_expression(&query).is_ok());
    assert_eq!(
        lexical_expression(&(query.clone() + "x")).unwrap_err().code,
        ErrorCode::Usage
    );
    assert_eq!(
        search(&r, &(query + "x"), &literal_plan())
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );
}

#[test]
fn lexical_metadata_and_transcripts_are_excluded_but_literal_is_audit() {
    let (temp, root, catalog) = fixture();
    let quote = b"DuplicateOnlyToken";
    let source = capture(&root, &catalog, quote);
    for entity in ["subject", "object"] {
        write(
            temp.path(),
            &format!("entities/{entity}.md"),
            &note(
                "entity",
                entity,
                entity,
                json!({"wiki_status":"active","wiki_entity_type":"concept"}),
                "",
            ),
        );
    }
    write(
        temp.path(),
        "assertion.md",
        &note(
            "assertion",
            "assertion",
            "Proposition",
            json!({"wiki_status":"accepted","wiki_subject_id":"subject","wiki_object_id":"object","wiki_predicate":"uses"}),
            "Proposition",
        ),
    );
    write(
        temp.path(),
        "evidence.md",
        &note(
            "evidence",
            "evidence",
            "Evidence",
            json!({"wiki_status":"active","wiki_assertion_id":"assertion","wiki_source_id":source.source_id,"wiki_source_revision":source.revision_id,"wiki_stance":"supports","wiki_locator_kind":"utf8-bytes","wiki_span_start":0,"wiki_span_end":quote.len(),"wiki_quote_hash":Blake3Hash::digest(quote)}),
            &String::from_utf8(exact_quote_body(quote, "\n", "Explanation").unwrap()).unwrap(),
        ),
    );
    let fingerprint = Blake3Hash::digest(b"packet fixture");
    write(
        temp.path(),
        "packet.md",
        &note(
            "extraction_packet",
            RecordId::packet(&fingerprint).as_str(),
            "Transcript",
            json!({"wiki_source_id":source.source_id,"wiki_source_revision":source.revision_id,"wiki_packet_fingerprint":fingerprint,"wiki_output_schema":"fixture-v1","wiki_created_at":"2026-09-28T00:00:00Z"}),
            "PrivateTranscriptOnlyWord",
        ),
    );
    page(
        temp.path(),
        "page.md",
        "plain_page",
        "Audit",
        "bodyword",
        &[],
    );
    let r = reader(&root, &catalog);
    assert!(
        search(&r, "wiki_status", &QueryPlan::default())
            .unwrap()
            .hits
            .is_empty()
    );
    let mut metadata = literal_plan();
    metadata.filters.kinds.push(RecordKind::Page);
    let literal = search(&r, "wiki_status", &metadata).unwrap();
    assert_eq!(literal.hits.len(), 1);
    assert_eq!(literal.hits[0].excerpt.label, ExcerptLabel::NoteText);
    assert!(literal.hits[0].excerpt.citation.is_none());
    for (kind, query) in [
        (RecordKind::Evidence, "DuplicateOnlyToken"),
        (RecordKind::ExtractionPacket, "PrivateTranscriptOnlyWord"),
    ] {
        let mut plan = QueryPlan::default();
        plan.filters.kinds.push(kind);
        assert!(search(&r, query, &plan).unwrap().hits.is_empty());
        plan.mode = SearchMode::Literal;
        if kind == RecordKind::ExtractionPacket {
            assert!(search(&r, query, &plan).unwrap().hits.is_empty());
            plan.filters.include_historical = true;
        }
        let hits = search(&r, query, &plan).unwrap();
        assert_eq!(hits.hits.len(), 1);
        assert_eq!(hits.hits[0].excerpt.label, ExcerptLabel::NoteText);
        assert!(hits.hits[0].excerpt.citation.is_none());
    }
}

#[test]
fn withdrawn_source_decision_rationale_is_historical_search_only() {
    let (temp, root, catalog) = fixture();
    let phrase = "North Lab does not use South Lab.";
    let source = capture(&root, &catalog, phrase.as_bytes());
    for entity in ["north", "south"] {
        write(
            temp.path(),
            &format!("entities/{entity}.md"),
            &note(
                "entity",
                entity,
                entity,
                json!({"wiki_status":"active","wiki_entity_type":"organization"}),
                "",
            ),
        );
    }
    write(
        temp.path(),
        "assertion.md",
        &note(
            "assertion",
            "assertion",
            "Reviewed claim",
            json!({"wiki_status":"accepted","wiki_subject_id":"north","wiki_object_id":"south","wiki_predicate":"uses","wiki_negated":true}),
            "Reviewed claim\n",
        ),
    );
    write(
        temp.path(),
        "evidence.md",
        &note(
            "evidence",
            "evidence",
            "Exact support",
            json!({"wiki_status":"active","wiki_assertion_id":"assertion","wiki_source_id":source.source_id,"wiki_source_revision":source.revision_id,"wiki_stance":"supports","wiki_locator_kind":"utf8-bytes","wiki_span_start":0,"wiki_span_end":phrase.len(),"wiki_quote_hash":Blake3Hash::digest(phrase.as_bytes())}),
            &String::from_utf8(exact_quote_body(phrase.as_bytes(), "\n", "Explanation").unwrap())
                .unwrap(),
        ),
    );
    let decision_path = "knowledge/decisions/decision.md";
    write(
        temp.path(),
        decision_path,
        &note(
            "decision",
            "decision",
            "Review rationale",
            json!({"wiki_status":"active","wiki_action":"accept","wiki_input_ids":["assertion"],"wiki_output_ids":["assertion"],"wiki_created_at":"2026-09-28T00:00:00Z"}),
            phrase,
        ),
    );
    let before = reader(&root, &catalog);
    assert_eq!(
        before.projection().records[&id("decision")].eligibility,
        Eligibility::Current
    );
    drop(before);
    let withdraw = SourceStore::new(VaultFs::new(root.clone()))
        .plan_withdraw(&source.source_id, "fixture withdrawal")
        .unwrap();
    let writer = WriterPermit::acquire(&root, Duration::from_millis(200)).unwrap();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let prepared = engine
        .prepare(&writer, withdraw.draft.unwrap())
        .unwrap()
        .prepared;
    engine
        .apply(&writer, &prepared, &CatalogGraphValidator, &catalog)
        .unwrap();
    drop(writer);
    let after = catalog.verified_snapshot(None).unwrap();
    assert_eq!(
        after.projection().records[&id("decision")].eligibility,
        Eligibility::Current
    );
    for mode in [SearchMode::Literal, SearchMode::Lexical] {
        let mut plan = QueryPlan {
            mode,
            ..Default::default()
        };
        let current = search(&after, phrase, &plan).unwrap();
        assert!(
            current.hits.iter().all(|hit| {
                hit.locator.path != relative(decision_path)
                    && hit.reasons.contains(&RetrievalReason::Identity)
                    && hit.excerpt.text.is_empty()
            }),
            "mode {mode:?}: {:?}",
            current.hits
        );
        plan.filters.include_historical = true;
        let history = search(&after, phrase, &plan).unwrap();
        assert!(history.hits.iter().any(|hit| {
            hit.locator.path == relative(decision_path)
                && hit.eligibility == Eligibility::Current
                && hit.excerpt.label == ExcerptLabel::NoteText
        }));
    }
}

#[test]
fn pure_plan_validation_has_no_cache_dependency() {
    let too_many = std::iter::repeat_n("word", 257)
        .collect::<Vec<_>>()
        .join(" ");
    for mode in [SearchMode::Lexical, SearchMode::Hybrid] {
        let plan = QueryPlan {
            mode,
            ..Default::default()
        };
        assert_eq!(
            lwiki::retrieval::lexical::validate_plan(&too_many, &plan)
                .unwrap_err()
                .code,
            ErrorCode::Usage
        );
    }
    let mut plan = QueryPlan::default();
    plan.filters.tags = vec!["z".into(), "a".into(), "z".into()];
    plan.cursor = Some("copied without decoding".into());
    let normalized = lwiki::retrieval::lexical::validate_plan("word", &plan).unwrap();
    assert_eq!(normalized.filters.tags, ["a", "z"]);
    assert_eq!(normalized.cursor, plan.cursor);
    plan.filters.tags = vec!["same".into(); 1000];
    assert_eq!(
        lwiki::retrieval::lexical::validate_plan("word", &plan)
            .unwrap()
            .filters
            .tags,
        ["same"]
    );
    plan.filters.tags = (0..81).map(|n| format!("tag{n}")).collect();
    assert_eq!(
        lwiki::retrieval::lexical::validate_plan("word", &plan)
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );
    plan.filters.tags.clear();
    assert_eq!(
        lwiki::retrieval::lexical::validate_plan("NUL\0word", &plan)
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );
    plan.limits.hits = 51;
    assert_eq!(
        lwiki::retrieval::lexical::validate_plan("word", &plan)
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );
}

#[test]
fn markdown_transforms_never_invent_exact_match_offsets() {
    let (temp, root, catalog) = fixture();
    page(
        temp.path(),
        "encoded.md",
        "encoded",
        "Encoded",
        "**caf&eacute;** after\r\n",
        &[],
    );
    let r = reader(&root, &catalog);
    let hits = search(&r, "cafe", &QueryPlan::default()).unwrap();
    assert_eq!(hits.hits.len(), 1);
    let hit = &hits.hits[0];
    let raw = fs::read_to_string(temp.path().join("encoded.md")).unwrap();
    assert_eq!(hit.excerpt.span.slice(&raw).unwrap(), hit.excerpt.text);
    assert!(hit.excerpt.matched_spans.is_empty());
    assert!(!hit.excerpt.text.contains("wiki_schema"));
    assert!(hit.excerpt.citation.is_none());
    let mut plan = QueryPlan::default();
    plan.limits.excerpt_bytes = 1;
    assert!(
        search(&r, "cafe", &plan).unwrap().hits[0]
            .excerpt
            .text
            .len()
            <= 1
    );
}

#[test]
fn withdrawn_payload_requires_explicit_history_and_keeps_exact_ownership() {
    let (_temp, root, catalog) = fixture();
    let captured = capture(&root, &catalog, b"HistoricalNeedle");
    let writer = WriterPermit::acquire(&root, Duration::from_millis(200)).unwrap();
    let store = SourceStore::new(VaultFs::new(root.clone()));
    let plan = store
        .plan_withdraw(&captured.source_id, "fixture withdrawal")
        .unwrap();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let prepared = engine
        .prepare(&writer, plan.draft.unwrap())
        .unwrap()
        .prepared;
    engine
        .apply(&writer, &prepared, &CatalogGraphValidator, &catalog)
        .unwrap();
    drop(writer);
    let reader = catalog.verified_snapshot(None).unwrap();
    let mut plan = QueryPlan::default();
    plan.filters.source_ids.push(captured.source_id.clone());
    assert!(
        search(&reader, "HistoricalNeedle", &plan)
            .unwrap()
            .hits
            .is_empty()
    );
    plan.filters.include_historical = true;
    let hits = search(&reader, "HistoricalNeedle", &plan).unwrap();
    assert_eq!(hits.hits.len(), 1);
    let hit = &hits.hits[0];
    assert_eq!(hit.eligibility, Eligibility::Withdrawn);
    assert_eq!(hit.owner_revision, Some(captured.revision_id));
    assert_eq!(hit.source_id, Some(captured.source_id));
    let view = SourceView::from_fs(catalog.fs()).unwrap();
    assert!(
        view.verify(
            hit.excerpt.citation.as_ref().unwrap(),
            CitationScope::Current
        )
        .is_err()
    );
    assert_eq!(
        view.verify(
            hit.excerpt.citation.as_ref().unwrap(),
            CitationScope::Historical
        )
        .unwrap()
        .quote,
        hit.excerpt.text.as_bytes()
    );
}

#[test]
fn exported_packet_windows_never_supply_default_source_search() {
    let (_temp, root, catalog) = fixture();
    let captured = capture(&root, &catalog, b"CEDAR-731 original source window");
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let view = SourceView::from_fs(catalog.fs()).unwrap();
    let packet = build_packet(
        &view,
        &ExportRequest {
            source_id: captured.source_id.clone(),
            revision_id: Some(captured.revision_id.clone()),
            windows: vec![],
            limits: ExtractionLimits::default(),
            candidate_context: vec![],
        },
    )
    .unwrap();
    let packet_id = packet.packet.packet_id.clone();
    assert!(packet.packet.windows[0].text.contains("CEDAR-731"));
    let writer = WriterPermit::acquire(&root, Duration::from_millis(200)).unwrap();
    let prepared = engine
        .prepare(&writer, packet.draft.unwrap())
        .unwrap()
        .prepared;
    engine
        .apply(&writer, &prepared, &CatalogGraphValidator, &catalog)
        .unwrap();
    drop(writer);

    let before = catalog.verified_snapshot(None).unwrap();
    assert!(before.projection().records.contains_key(&packet_id));
    assert_eq!(
        before.projection().records[&packet_id].eligibility,
        Eligibility::Unsupported
    );
    let hits = search(&before, "CEDAR-731", &literal_plan()).unwrap();
    assert_eq!(hits.hits.len(), 1, "packet must not duplicate source text");
    assert_eq!(
        hits.hits[0].owner_revision,
        Some(captured.revision_id.clone())
    );
    let mut packet_audit = literal_plan();
    packet_audit
        .filters
        .kinds
        .push(RecordKind::ExtractionPacket);
    packet_audit.filters.include_historical = true;
    let packet_hits = search(&before, "CEDAR-731", &packet_audit).unwrap();
    assert_eq!(packet_hits.hits.len(), 1);
    assert_eq!(packet_hits.hits[0].eligibility, Eligibility::Unsupported);
    let units = render::corpus(&before, &EmbeddingSettings::default()).unwrap();
    assert!(units.iter().any(|unit| unit.utf8.contains("CEDAR-731")));
    assert!(
        units
            .iter()
            .all(|unit| unit.owner != packet_hits.hits[0].locator.path)
    );

    let withdraw = SourceStore::new(VaultFs::new(root.clone()))
        .plan_withdraw(&captured.source_id, "fixture withdrawal")
        .unwrap();
    let writer = WriterPermit::acquire(&root, Duration::from_millis(200)).unwrap();
    let prepared = engine
        .prepare(&writer, withdraw.draft.unwrap())
        .unwrap()
        .prepared;
    engine
        .apply(&writer, &prepared, &CatalogGraphValidator, &catalog)
        .unwrap();
    drop(writer);
    for rebuild in [false, true] {
        if rebuild {
            let writer = WriterPermit::acquire(&root, Duration::from_millis(200)).unwrap();
            catalog.rebuild(&writer).unwrap();
        }
        let after = catalog.verified_snapshot(None).unwrap();
        assert_eq!(
            after.projection().records[&packet_id].eligibility,
            Eligibility::Withdrawn
        );
        assert!(
            search(&after, "CEDAR-731", &literal_plan())
                .unwrap()
                .hits
                .is_empty()
        );
        assert!(
            search(
                &catalog.index_snapshot().unwrap(),
                "CEDAR-731",
                &literal_plan()
            )
            .unwrap()
            .hits
            .is_empty()
        );
        let mut history = literal_plan();
        history.filters.include_historical = true;
        let hits = search(&after, "CEDAR-731", &history).unwrap();
        assert!(
            hits.hits
                .iter()
                .any(|h| h.owner_revision == Some(captured.revision_id.clone()))
        );
        let packet_hits = search(&after, "CEDAR-731", &packet_audit).unwrap();
        assert_eq!(packet_hits.hits.len(), 1);
        assert_eq!(packet_hits.hits[0].eligibility, Eligibility::Withdrawn);
        let units = render::corpus(&after, &EmbeddingSettings::default()).unwrap();
        assert!(units.iter().all(|unit| !unit.utf8.contains("CEDAR-731")));
        assert!(load_packet(&SourceView::from_fs(catalog.fs()).unwrap(), &packet_id).is_ok());
    }
}

#[test]
fn api_extraction_and_generation_output_never_supply_current_search() {
    let f = api_fixture::Fixture::new();
    let source_id = f.request.export.source_id.clone();
    let mock = api_fixture::Mock::response(f.response());
    let outcome = f
        .app
        .graph_extract_api(
            &f.request,
            &f.service,
            &f.dispatcher(mock),
            f.options.clone(),
        )
        .unwrap();
    let output_path = outcome.output.as_ref().unwrap().path.clone();
    let imported = outcome.import.unwrap();
    let extraction_path = imported.extraction.path.clone();
    f.apply(imported.prepared.as_ref().unwrap());

    let before = f.catalog.verified_snapshot(None).unwrap();
    for path in [&output_path, &extraction_path] {
        let document = before
            .projection()
            .documents
            .iter()
            .find(|document| &document.path == path)
            .unwrap();
        assert!(document.raw_text.contains("Ada"), "{path}");
    }
    let hits = search(&before, "Ada", &literal_plan()).unwrap();
    assert_eq!(hits.hits.len(), 1, "only the source supplies current text");
    let units = render::corpus(&before, &EmbeddingSettings::default()).unwrap();
    assert!(
        units
            .iter()
            .all(|unit| unit.owner != output_path && unit.owner != extraction_path)
    );

    let withdraw = SourceStore::new(f.fs.clone())
        .plan_withdraw(&source_id, "fixture withdrawal")
        .unwrap();
    let writer = WriterPermit::acquire(f.fs.root(), Duration::from_secs(2)).unwrap();
    let prepared = f
        .engine
        .prepare(&writer, withdraw.draft.unwrap())
        .unwrap()
        .prepared;
    f.engine
        .apply(&writer, &prepared, &CatalogGraphValidator, &f.catalog)
        .unwrap();
    drop(writer);
    let after = f.catalog.verified_snapshot(None).unwrap();
    assert!(
        search(&after, "Ada", &literal_plan())
            .unwrap()
            .hits
            .is_empty()
    );
    let mut audit = literal_plan();
    audit.filters.include_historical = true;
    let history = search(&after, "Ada", &audit).unwrap();
    for path in [&output_path, &extraction_path] {
        assert!(
            history.hits.iter().any(|hit| {
                &hit.locator.path == path && hit.eligibility == Eligibility::Withdrawn
            }),
            "{path}"
        );
    }

    fs::copy(
        f.temp.path().join(extraction_path.as_str()),
        f.temp.path().join("copied-extraction.md"),
    )
    .unwrap();
    fs::copy(
        f.temp.path().join(output_path.as_str()),
        f.temp.path().join("copied-generation-output.md"),
    )
    .unwrap();
    let copied = reader(f.fs.root(), &f.catalog);
    assert!(
        search(&copied, "Ada", &literal_plan())
            .unwrap()
            .hits
            .is_empty()
    );
    let copied_units = render::corpus(&copied, &EmbeddingSettings::default()).unwrap();
    assert!(copied_units.iter().all(|unit| {
        unit.owner != relative("copied-extraction.md")
            && unit.owner != relative("copied-generation-output.md")
    }));
}

#[test]
fn malformed_operational_declarations_preserve_ordinary_invalid_discovery() {
    let (temp, root, catalog) = fixture();
    let extraction = b"---\nwiki_kind: extraction\nwiki_id: extraction_broken\ninvalid: [unfinished\n---\nCEDAR-731 retained output\n";
    let output = b"---\nwiki_kind: run_event\nwiki_id: run_event_generation_broken\ninvalid: [unfinished\n---\nCEDAR-731 retained API response\n";
    write(
        temp.path(),
        "knowledge/extractions/extraction_broken.md",
        extraction,
    );
    write(temp.path(), "copied-malformed-extraction.md", extraction);
    write(
        temp.path(),
        "runs/run_api_broken/outputs/run_event_generation_broken.md",
        output,
    );
    write(temp.path(), "copied-malformed-generation-output.md", output);
    write(
        temp.path(),
        "ordinary-invalid.md",
        b"---\nwiki_kind: page\nwiki_id: broken_page\ninvalid: [unfinished\n---\nCEDAR-731 ordinary author note\n",
    );
    let r = reader(&root, &catalog);
    let hits = search(&r, "CEDAR-731", &literal_plan()).unwrap();
    assert_eq!(hits.hits.len(), 1);
    assert_eq!(hits.hits[0].locator.path, relative("ordinary-invalid.md"));
    let units = render::corpus(&r, &EmbeddingSettings::default()).unwrap();
    assert!(units.iter().all(|unit| {
        !unit.owner.as_str().contains("extraction_broken")
            && !unit.owner.as_str().contains("run_event_generation_broken")
            && !unit.owner.as_str().starts_with("copied-malformed-")
    }));
    let mut audit = literal_plan();
    audit.filters.include_historical = true;
    let historical = search(&r, "CEDAR-731", &audit).unwrap();
    assert_eq!(historical.hits.len(), 5);
}
