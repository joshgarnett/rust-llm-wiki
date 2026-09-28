use lwiki::{
    catalog::*,
    changes::*,
    domain::*,
    retrieval::{lexical::lexical_expression, *},
    sources::{evidence::exact_quote_body, *},
    vault::*,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};

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
        search(&r, "RareSymbol", &QueryPlan::default())
            .unwrap()
            .hits
            .iter()
            .any(|h| h.locator.path == relative("invalid.md"))
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
    let many = std::iter::repeat_n("word", 65)
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
        search(&r, &"a".repeat(4097), &literal_plan())
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
        let hits = search(&r, query, &plan).unwrap();
        assert_eq!(hits.hits.len(), 1);
        assert_eq!(hits.hits[0].excerpt.label, ExcerptLabel::NoteText);
        assert!(hits.hits[0].excerpt.citation.is_none());
    }
}

#[test]
fn pure_plan_validation_has_no_cache_dependency() {
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
