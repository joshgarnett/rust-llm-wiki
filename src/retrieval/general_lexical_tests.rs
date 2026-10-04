//! Expectations were frozen in general-lexical-implementation.json before edits.
//! Every compatibility fixture uses the actual legacy and normalized builders.
use super::*;
use crate::{
    catalog::{
        Catalog,
        file_types::{BuildIdentity, CatalogSelection},
        normalized_build::{BuildLimits, CompletedCatalog, NormalizedBuilder},
        query_types::QueryReadLimits,
        scan, selector,
    },
    changes::ChangeDraft,
    sources::{
        CaptureRequest, ExtractionInput, SourceOrigin, SourceStore, evidence::exact_quote_body,
    },
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use rusqlite::StatementStatus;
use serde_json::{Value as Json, json};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn note(kind: &str, name: &str, title: &str, extra: Json, body: &str) -> Vec<u8> {
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
    let canonical = CanonicalRecord::new(fields.clone()).unwrap_or_else(|error| {
        panic!("fixture {kind}/{name} schema validation failed: {error:?}")
    });
    validate_status(&canonical).unwrap_or_else(|error| {
        panic!("fixture {kind}/{name} lifecycle validation failed: {error:?}")
    });
    let mut text = "---\n".to_owned();
    for (key, value) in fields {
        text.push_str(&format!("{key}: {value}\n"));
    }
    text.push_str("---\n");
    text.push_str(body);
    let parsed = parse_note(text.as_bytes());
    assert!(
        parsed.diagnostics.is_empty(),
        "fixture {kind}/{name} YAML diagnostics: {:?}",
        parsed.diagnostics
    );
    assert_eq!(
        parsed.canonical.as_ref(),
        Some(&canonical),
        "fixture {kind}/{name} exact YAML roundtrip"
    );
    text.into_bytes()
}
fn write(root: &Path, name: &str, bytes: &[u8]) {
    let target = root.join(name);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, bytes).unwrap();
}
fn seed(root: &Path, draft: ChangeDraft) {
    for op in draft.operations {
        let bytes = op.proposed.unwrap();
        if op.target.as_str().ends_with("/source.md")
            || op.target.as_str().ends_with("/revision.md")
        {
            let parsed = parse_note(&bytes);
            assert!(
                parsed.diagnostics.is_empty(),
                "generated metadata {}: {:?}",
                op.target,
                parsed.diagnostics
            );
            let canonical = parsed
                .canonical
                .as_ref()
                .expect("SourceStore metadata must remain canonical");
            validate_status(canonical).unwrap_or_else(|error| {
                panic!("generated metadata {} lifecycle: {error:?}", op.target)
            });
        }
        write(root, op.target.as_str(), &bytes);
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    catalog: Catalog,
    legacy: ReaderSnapshot,
    completed: CompletedCatalog,
    source: RecordId,
    old: RecordId,
    head: RecordId,
    withdrawn_source: RecordId,
    withdrawn_head: RecordId,
}
impl Fixture {
    fn new(extra_rows: usize) -> Self {
        Self::configured(extra_rows, 0)
    }
    fn configured(extra_rows: usize, matching_rows: usize) -> Self {
        Self::build(extra_rows, matching_rows, true, 0)
    }
    fn build(
        extra_rows: usize,
        matching_rows: usize,
        layout2: bool,
        metadata_bytes: usize,
    ) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(
            root,
            "WIKI.md",
            &note(
                "vault",
                "vault_general_lexical",
                "Lexical fixture",
                json!({}),
                "",
            ),
        );
        let fs_handle = VaultFs::new(VaultRoot::explicit(root).unwrap());
        let catalog = Catalog::new(fs_handle.clone(), id("vault_general_lexical"));
        let store = SourceStore::new(fs_handle.clone());
        let request = |bytes: &[u8]| CaptureRequest {
            title: "Captured commonword".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture.txt".into(),
            original: bytes.to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        };
        let first = store
            .plan_capture(request(b"commonword oldpayload historical fact"))
            .unwrap();
        seed(root, first.draft.unwrap());
        let current = b"commonword currentpayload shared proof";
        let second = store
            .plan_refresh(&first.source_id, request(current))
            .unwrap();
        seed(root, second.draft.unwrap());
        let withdrawn = store
            .plan_capture(CaptureRequest {
                title: "Withdrawn lifecycle source".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "withdrawn-fixture.txt".into(),
                original: b"lifecyclewithdrawnneedle immutable retained quotation".to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: None,
            })
            .unwrap();
        seed(root, withdrawn.draft.unwrap());
        let withdrawal = store
            .plan_withdraw(&withdrawn.source_id, "fixture withdrawal")
            .unwrap();
        seed(root, withdrawal.draft.unwrap());
        write(
            root,
            "alpha.md",
            &note(
                "page",
                "page_alpha",
                "Exact commonword",
                json!({"wiki_status":"reviewed","aliases":["SharedAlias"],"tags":["keep","blue"]}),
                "commonword alpha fact\n",
            ),
        );
        write(
            root,
            "beta.md",
            &note(
                "page",
                "page_beta",
                "Exact commonword",
                json!({"wiki_status":"draft","aliases":["SharedAlias"],"tags":["blue"]}),
                "commonword draft fact\n",
            ),
        );
        write(
            root,
            "entity.md",
            &note(
                "entity",
                "entity_identity",
                "Unique Atlas",
                json!({"wiki_status":"active","wiki_entity_type":"concept","aliases":["AtlasAlias"]}),
                "SecretUnsupportedDescription commonword\n",
            ),
        );
        write(
            root,
            "unicode.md",
            &note(
                "page",
                "page_unicode_general",
                "Unicode parity",
                json!({"wiki_status":"reviewed","aliases":["Éclair 東京"]}),
                "# Unicode\r\nCafé **naïve** e\u{301} λ 東京 retain exact bytes.\r\n",
            ),
        );
        for (name, body) in [
            ("duplicate-a.md", "duplicateidentityneedle first\n"),
            ("duplicate-b.md", "duplicateidentityneedle second\n"),
        ] {
            write(
                root,
                name,
                &note(
                    "page",
                    "page_duplicate_general",
                    "Duplicate declaration",
                    json!({"wiki_status":"reviewed","aliases":["DuplicatedAlias"]}),
                    body,
                ),
            );
        }
        write(
            root,
            "deprecated.md",
            &note(
                "page",
                "page_deprecated_general",
                "Deprecated lifecycle",
                json!({"wiki_status":"deprecated"}),
                "lifecycledeprecatedneedle retained author prose\n",
            ),
        );
        write(
            root,
            "plain.md",
            b"# Plain commonword\ncommonword plaintext\n",
        );
        write(root,"invalid.md",b"---\nwiki_kind: page\nwiki_id: broken_page\ninvalid: [unfinished\n---\ncommonword invalidtext\n");
        write(root, "nonutf8.md", b"\xff\xfe");
        write(root,"copied-packet.md",b"---\nwiki_kind: extraction_packet\nwiki_id: packet_broken\ninvalid: [unfinished\n---\ncommonword packettext\n");
        write(
            root,
            "assertion.md",
            &note(
                "assertion",
                "assertion_supported",
                "Supported commonword",
                // Same accepted uses fields as the proven assertion/evidence
                // pair in tests/retrieval_lexical.rs; self endpoints are also
                // represented by src/catalog/eligibility_facts.rs fixtures.
                json!({"wiki_status":"accepted","wiki_subject_id":"entity_identity","wiki_object_id":"entity_identity","wiki_predicate":"uses"}),
                "commonword assertiontext\n",
            ),
        );
        write(
            root,
            "proposed.md",
            &note(
                "assertion",
                "assertion_proposed",
                "Proposed commonword",
                // Proven registry predicate from tests/entity_decisions_cli.rs;
                // keep a distinct proposition from the supported uses assertion.
                json!({"wiki_status":"proposed","wiki_subject_id":"entity_identity","wiki_object_id":"entity_identity","wiki_predicate":"depends_on"}),
                "commonword proposedtext\n",
            ),
        );
        write(
            root,
            "evidence.md",
            &note(
                "evidence",
                "evidence_supported",
                "Evidence fixture",
                json!({"wiki_status":"active","wiki_assertion_id":"assertion_supported","wiki_source_id":first.source_id,"wiki_source_revision":second.revision_id,"wiki_stance":"supports","wiki_locator_kind":"utf8-bytes","wiki_span_start":0,"wiki_span_end":current.len(),"wiki_quote_hash":Blake3Hash::digest(current)}),
                &String::from_utf8(exact_quote_body(current, "\n", "").unwrap()).unwrap(),
            ),
        );
        for n in 0..extra_rows {
            write(
                root,
                &format!("growth/{n:05}.md"),
                format!("# Unrelated\nunrelated unrelated{n}\n").as_bytes(),
            );
        }
        for n in 0..matching_rows {
            let mut fields =
                json!({"wiki_status":"reviewed","aliases":["SharedAlias"],"tags":["bulk"]});
            if metadata_bytes > 0 {
                // A long canonical provenance field remains RecordRow metadata,
                // distinct from body/raw text; source filters must parse JSON
                // before admitting a hit even when no payload row is returned.
                fields["provenance_notes"] = json!(
                    "Imported from retained reference; metadata review details. "
                        .repeat(metadata_bytes / 58)
                );
            }
            write(
                root,
                &format!("matching/{n:05}.md"),
                &note(
                    "page",
                    &format!("page_matching_{n:05}"),
                    "Exact commonword",
                    fields,
                    "commonword repeated fact\n",
                ),
            );
        }
        let writer = WriterPermit::acquire(fs_handle.root(), Duration::ZERO).unwrap();
        catalog.sync(&writer).unwrap();
        let legacy = catalog.index_snapshot().unwrap();
        let identity = BuildIdentity {
            selection: CatalogSelection::new(id("vault_general_lexical"), 2).unwrap(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        selector::prepare(&fs_handle, &writer, &identity.selection).unwrap();
        let mut builder =
            NormalizedBuilder::begin(&fs_handle, &writer, identity, BuildLimits::default())
                .unwrap();
        let input = scan::scan_input(&fs_handle, &id("vault_general_lexical")).unwrap();
        let completed = if layout2 {
            let projection =
                scan::project_normalized_with_sink(&fs_handle, &input, false, &mut builder)
                    .unwrap();
            builder.finish_normalized(&projection).unwrap()
        } else {
            let projection =
                scan::project_with_sink(&fs_handle, &input, false, &mut builder).unwrap();
            builder.finish(&projection).unwrap()
        };
        selector::publish(
            &fs_handle,
            &writer,
            &completed.identity.selection,
            Duration::ZERO,
        )
        .unwrap();
        Self {
            _temp: temp,
            catalog,
            legacy,
            completed,
            source: first.source_id,
            old: first.revision_id,
            head: second.revision_id,
            withdrawn_source: withdrawn.source_id,
            withdrawn_head: withdrawn.revision_id,
        }
    }
    fn query(&self) -> crate::catalog::query::QuerySnapshot {
        self.catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap()
    }
    fn paired(&self, query: &str, plan: &QueryPlan, context: Option<bool>) -> HitSet {
        let normalized = self.query();
        let old = search_inner(&self.legacy, query, plan, context, false).unwrap();
        let new = search_inner(&normalized, query, plan, context, false).unwrap();
        assert_eq!(new.hits, old.hits, "{query} {plan:?} context={context:?}");
        assert_eq!(
            (new.candidate_count, new.omitted_candidates, new.truncated),
            (old.candidate_count, old.omitted_candidates, old.truncated)
        );
        assert_eq!(new.warnings, old.warnings);
        assert!(new.hits.iter().all(|h| h.excerpt.citation.is_none()));
        assert_eq!(new.verification, SnapshotVerification::IndexSnapshot);
        new
    }
}
#[test]
fn paired_actual_builders_preserve_tiers_identity_invalid_and_history() {
    let f = Fixture::new(0);
    for query in [
        "page_alpha",
        "Exact commonword",
        "SharedAlias",
        "commonword",
        "Unique Atlas",
        "AtlasAlias",
        "entity_identity",
        "SecretUnsupportedDescription",
        "invalidtext",
        "plaintext",
        "nonutf8.md",
        "currentpayload",
        "oldpayload",
        "packettext",
        "proposedtext",
        "absent",
    ] {
        for historical in [false, true] {
            for proposed in [false, true] {
                let mut plan = QueryPlan::default();
                plan.filters.include_historical = historical;
                plan.filters.include_proposed = proposed;
                f.paired(query, &plan, None);
            }
        }
    }
    let identity = f.paired("AtlasAlias", &QueryPlan::default(), None);
    assert_eq!(identity.hits.len(), 1);
    assert!(
        identity.hits[0]
            .reasons
            .contains(&RetrievalReason::Identity)
    );
    assert!(identity.hits[0].excerpt.text.is_empty());
    assert!(
        f.paired("SecretUnsupportedDescription", &QueryPlan::default(), None)
            .hits
            .is_empty()
    );
    assert!(
        f.paired("oldpayload", &QueryPlan::default(), None)
            .hits
            .is_empty()
    );
    let mut historical = QueryPlan::default();
    historical.filters.include_historical = true;
    assert!(
        f.paired("oldpayload", &historical, None)
            .hits
            .iter()
            .any(|h| h.owner_revision.as_ref() == Some(&f.old))
    );
    assert!(
        f.paired("currentpayload", &QueryPlan::default(), None)
            .hits
            .iter()
            .any(|h| h.owner_revision.as_ref() == Some(&f.head))
    );
    assert!(
        f.paired("packettext", &QueryPlan::default(), None)
            .hits
            .is_empty()
    );
    assert!(
        f.paired("invalidtext", &QueryPlan::default(), None)
            .warnings
            .iter()
            .any(|w| w.contains("invalid_note_hit"))
    );
}
#[test]
fn paired_filters_precede_caps_and_selected_context_revalidation() {
    let f = Fixture::new(0);
    for filters in [
        SearchFilters {
            tags: vec!["blue".into(), "keep".into()],
            ..Default::default()
        },
        SearchFilters {
            kinds: vec![RecordKind::Page],
            authored_statuses: vec!["draft".into()],
            ..Default::default()
        },
        SearchFilters {
            source_ids: vec![f.source.clone()],
            ..Default::default()
        },
        SearchFilters {
            path_prefix: Some("alpha".into()),
            ..Default::default()
        },
        SearchFilters {
            tags: vec!["absent".into()],
            ..Default::default()
        },
        SearchFilters {
            kinds: vec![RecordKind::Assertion],
            source_ids: vec![f.source.clone()],
            ..Default::default()
        },
    ] {
        let plan = QueryPlan {
            filters,
            limits: SearchLimits {
                hits: 1,
                candidates: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let hits = f.paired("commonword", &plan, None);
        let normalized = f.query();
        for hit in hits.hits {
            let document = normalized.document(&hit.locator.path).unwrap().unwrap();
            assert!(
                filters::matches_catalog_document(&normalized, &document, &plan.filters).unwrap()
            );
            assert!(
                filters::matches_catalog_document(&f.legacy, &document, &plan.filters).unwrap()
            );
        }
    }
    for context in [false, true] {
        for query in [
            "commonword",
            "AtlasAlias",
            "invalidtext",
            "oldpayload",
            "currentpayload",
        ] {
            f.paired(query, &QueryPlan::default(), Some(context));
        }
    }
    let mut plan = QueryPlan::default();
    plan.filters.source_ids = vec![f.source.clone()];
    plan.filters.kinds = vec![RecordKind::Assertion];
    assert!(
        f.paired("assertiontext", &plan, None)
            .hits
            .iter()
            .any(|h| h.locator.path == path("assertion.md"))
    );
    // Source IDs are supported own metadata on Source/Revision, and evidence
    // associates an Assertion with its Source. Pages have no semantic source
    // field in schema1 and must not gain one just to construct a test fixture.
    plan.filters.kinds = vec![RecordKind::Source];
    assert!(
        f.paired(f.source.as_str(), &plan, None)
            .hits
            .iter()
            .any(|hit| hit
                .locator
                .record
                .as_ref()
                .is_some_and(|record| record.record_id == f.source))
    );
    plan.filters.kinds = vec![RecordKind::Revision];
    assert!(
        f.paired(f.head.as_str(), &plan, None)
            .hits
            .iter()
            .any(|hit| hit
                .locator
                .record
                .as_ref()
                .is_some_and(|record| record.record_id == f.head))
    );
    plan.filters.kinds = vec![RecordKind::Page];
    assert!(f.paired("commonword", &plan, None).hits.is_empty());
    let mut filtered = QueryPlan::default();
    filtered.filters.tags = vec!["keep".into()];
    filtered.limits.candidates = 1;
    assert_eq!(
        f.paired("commonword", &filtered, None).hits[0].locator.path,
        path("alpha.md")
    );
}
#[test]
fn selected_cache_validation_and_read_only_capability_fences() {
    for mutation in [
        "UPDATE documents SET raw_text='tampered' WHERE path='alpha.md'",
        "UPDATE documents SET aliases_text='wrong' WHERE path='alpha.md'",
        "UPDATE documents SET tags_text='wrong' WHERE path='alpha.md'",
        "UPDATE records SET description_eligibility='current' WHERE id='entity_identity'",
    ] {
        let f = Fixture::new(0);
        let db = Connection::open(&f.completed.path).unwrap();
        selector::configure_wal(&db).unwrap();
        db.execute_batch(mutation).unwrap();
        drop(db);
        let query = if mutation.contains("description") {
            "AtlasAlias"
        } else {
            "page_alpha"
        };
        assert_eq!(
            search_catalog(&f.query(), query, &QueryPlan::default())
                .unwrap_err()
                .code,
            ErrorCode::IndexCorrupt,
            "{mutation}"
        );
    }
    for definition in [
        "",
        "CREATE INDEX document_titles ON documents(title COLLATE NOCASE,record_id,path)",
        "CREATE INDEX document_titles ON documents(title,record_id,path) WHERE eligibility='current'",
    ] {
        let f = Fixture::new(0);
        let db = Connection::open(&f.completed.path).unwrap();
        selector::configure_wal(&db).unwrap();
        db.execute_batch("DROP INDEX document_titles").unwrap();
        db.execute_batch(definition).unwrap();
        drop(db);
        assert_eq!(
            search_catalog(&f.query(), "page_alpha", &QueryPlan::default())
                .unwrap_err()
                .code,
            ErrorCode::CapabilityUnavailable
        );
    }
    let f = Fixture::new(0);
    let db = Connection::open(&f.completed.path).unwrap();
    selector::configure_wal(&db).unwrap();
    db.execute_batch("UPDATE documents SET aliases_json='{' WHERE path='beta.md'")
        .unwrap();
    drop(db);
    let mut plan = QueryPlan::default();
    plan.filters.path_prefix = Some("alpha".into());
    assert_eq!(
        search_catalog(&f.query(), "commonword", &plan)
            .unwrap()
            .hits[0]
            .locator
            .path,
        path("alpha.md")
    );
    let literal = QueryPlan {
        mode: SearchMode::Literal,
        ..Default::default()
    };
    assert_eq!(
        search_catalog(&f.query(), "commonword", &literal)
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable
    );
    assert_eq!(
        search_catalog(&f.query(), "***", &QueryPlan::default())
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );
}
#[derive(Debug)]
struct Work {
    vm: i32,
    scan: i32,
    sort: i32,
    rows: usize,
    elapsed: Duration,
    plans: Vec<String>,
}
fn work(reader: &dyn QueryCatalog, query: &str, plan: &QueryPlan, leg: usize) -> Work {
    let expression = lexical_expression(query).unwrap();
    let (sql, values) = normalized_candidate_query(query, &expression, plan, "1", leg);
    let (keys, _) = sql.split_once("SELECT d.path,d.file_hash").unwrap();
    assert!(keys.contains("AS MATERIALIZED"));
    assert!(!keys.contains("raw_text"));
    assert!(!keys.contains("d.body"));
    assert!(!keys.contains("d.row_json"));
    let plans = reader
        .connection()
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(params_from_iter(values.clone()), |r| r.get(3))
        .unwrap()
        .collect::<std::result::Result<Vec<String>, _>>()
        .unwrap();
    let mut statement = reader.connection().prepare(&sql).unwrap();
    let start = Instant::now();
    let mut rows = statement.query(params_from_iter(values)).unwrap();
    let mut count = 0;
    while rows.next().unwrap().is_some() {
        count += 1;
    }
    drop(rows);
    Work {
        vm: statement.get_status(StatementStatus::VmStep),
        scan: statement.get_status(StatementStatus::FullscanStep),
        sort: statement.get_status(StatementStatus::Sort),
        rows: count,
        elapsed: start.elapsed(),
        plans,
    }
}
#[test]
fn actual_candidate_plans_and_work_distinguish_postings_from_unrelated_growth() {
    let small = Fixture::new(0);
    let grown = Fixture::new(1000);
    let plan = QueryPlan::default();
    for (query, leg, index) in [
        ("page_alpha", 0, "document_record_ids"),
        ("Exact commonword", 1, "document_titles"),
        ("SharedAlias", 2, "sqlite_autoindex_registry_match_keys_1"),
        ("currentpayload", 3, "VIRTUAL TABLE INDEX"),
        ("Unique Atlas", 4, "VIRTUAL TABLE INDEX"),
    ] {
        let a = work(&small.query(), query, &plan, leg);
        let b = work(&grown.query(), query, &plan, leg);
        assert!(a.plans.iter().any(|p| p.contains(index)), "{a:?}");
        assert_eq!(a.rows, b.rows);
        assert!(b.vm <= a.vm * 2 + 200, "{query}: {a:?} -> {b:?}");
        assert!(b.scan <= a.scan + 10, "{a:?} -> {b:?}");
        eprintln!(
            "general lexical unrelated-growth query={query:?} leg={leg} small={a:?} grown={b:?}"
        );
    }
    for filters in [
        SearchFilters::default(),
        SearchFilters {
            tags: vec!["keep".into()],
            ..Default::default()
        },
        SearchFilters {
            tags: vec!["absent".into()],
            ..Default::default()
        },
        SearchFilters {
            kinds: vec![RecordKind::Page],
            authored_statuses: vec!["reviewed".into()],
            ..Default::default()
        },
        SearchFilters {
            source_ids: vec![grown.source.clone()],
            ..Default::default()
        },
        SearchFilters {
            path_prefix: Some("alpha".into()),
            ..Default::default()
        },
    ] {
        let plan = QueryPlan {
            filters,
            limits: SearchLimits {
                candidates: 1,
                hits: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let measured = work(&grown.query(), "commonword", &plan, 3);
        assert!(measured.rows <= 2);
        assert!(measured.vm < 20000, "{measured:?}");
        eprintln!("general lexical restrictive {plan:?}: {measured:?}");
        let reader = grown.query();
        let hits = search_catalog(&reader, "commonword", &plan).unwrap();
        eprintln!(
            "general lexical restrictive selected usage hits={} usage={:?}",
            hits.hits.len(),
            reader.usage()
        );
    }
}
#[test]
fn huge_title_alias_buckets_and_popular_filtered_queries_bound_payload_not_postings() {
    let small = Fixture::configured(0, 10);
    let large = Fixture::configured(0, 1000);
    let plan = QueryPlan {
        limits: SearchLimits {
            hits: 1,
            candidates: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    for (query, leg) in [
        ("Exact commonword", 1),
        ("SharedAlias", 2),
        ("commonword", 3),
    ] {
        let a = work(&small.query(), query, &plan, leg);
        let b = work(&large.query(), query, &plan, leg);
        assert_eq!(a.rows, 2);
        assert_eq!(b.rows, 2);
        assert!(b.vm < 10_000_000, "{b:?}");
        if leg == 2 {
            assert!(
                b.vm <= a.vm * 2 + 200,
                "indexed alias order should stop after admitted keys: {a:?} -> {b:?}"
            );
        } else {
            assert!(
                b.vm > a.vm,
                "matching work must be measured, not inferred from LIMIT: {a:?} -> {b:?}"
            );
        }
        eprintln!(
            "general lexical matching-growth query={query:?} leg={leg} small={a:?} large={b:?}"
        );
        let reader = large.query();
        let hits = search_catalog(&reader, query, &plan).unwrap();
        eprintln!(
            "general lexical matching-growth selected query={query:?} usage={:?}",
            reader.usage()
        );
        assert_eq!(hits.hits.len(), 1);
        assert!(reader.usage().rows <= 20, "{:?}", reader.usage());
    }
    for filters in [
        SearchFilters {
            tags: vec!["keep".into()],
            ..Default::default()
        },
        SearchFilters {
            tags: vec!["absent".into()],
            ..Default::default()
        },
        SearchFilters {
            source_ids: vec![large.source.clone()],
            kinds: vec![RecordKind::Page],
            ..Default::default()
        },
        SearchFilters {
            path_prefix: Some("alpha".into()),
            ..Default::default()
        },
        SearchFilters {
            authored_statuses: vec!["draft".into()],
            ..Default::default()
        },
    ] {
        let filtered = QueryPlan {
            filters,
            ..plan.clone()
        };
        let metrics = work(&large.query(), "commonword", &filtered, 3);
        assert!(metrics.vm < 10_000_000, "{metrics:?}");
        eprintln!("general lexical popular restrictive {filtered:?}: {metrics:?}");
        let reader = large.query();
        let hits = search_catalog(&reader, "commonword", &filtered).unwrap();
        eprintln!(
            "general lexical popular restrictive selected usage hits={} usage={:?}",
            hits.hits.len(),
            reader.usage()
        );
        if filtered.filters.tags == ["absent"]
            || (!filtered.filters.source_ids.is_empty()
                && filtered.filters.kinds == [RecordKind::Page])
        {
            assert!(hits.hits.is_empty());
            assert_eq!(reader.usage().rows, 0);
        } else if filtered.filters.authored_statuses == ["draft"] {
            assert_eq!(hits.hits[0].locator.path, path("beta.md"));
        } else {
            assert_eq!(hits.hits[0].locator.path, path("alpha.md"));
        }
        assert!(reader.usage().rows <= 5, "{:?}", reader.usage());
    }
}
#[test]
fn generic_search_cursor_is_scope_bound_and_metadata_is_snapshot_only() {
    let f = Fixture::new(0);
    let reader = f.query();
    let plan = QueryPlan {
        limits: SearchLimits {
            hits: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let first = search_catalog(&reader, "commonword", &plan).unwrap();
    let cursor = first.next_cursor.unwrap();
    let second_plan = QueryPlan {
        cursor: Some(cursor),
        ..plan
    };
    let second = search_catalog(&reader, "commonword", &second_plan).unwrap();
    assert_ne!(first.hits[0].locator.path, second.hits[0].locator.path);
    assert_eq!(
        search_catalog(&reader, "changed query", &second_plan)
            .unwrap_err()
            .code,
        ErrorCode::CursorStale
    );
    let mut changed = second_plan.clone();
    changed.filters.tags = vec!["changed tag".into()];
    assert_eq!(
        search_catalog(&reader, "commonword", &changed)
            .unwrap_err()
            .code,
        ErrorCode::CursorStale
    );
    let indexed = f
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(
        search_catalog(&indexed, "commonword", &second_plan)
            .unwrap_err()
            .code,
        ErrorCode::CursorStale
    );
    assert_eq!(
        search_context_catalog(&reader, "commonword", &second_plan, false)
            .unwrap_err()
            .code,
        ErrorCode::CursorStale
    );
    assert_eq!(first.verification, SnapshotVerification::IndexSnapshot);
}
#[test]
fn source_metadata_errors_are_selected_and_unrelated_rows_stay_outside_filters() {
    let f = Fixture::new(0);
    let db = Connection::open(&f.completed.path).unwrap();
    selector::configure_wal(&db).unwrap();
    db.execute_batch("UPDATE records SET row_json='{' WHERE id='page_beta'")
        .unwrap();
    drop(db);
    let plan = QueryPlan {
        filters: SearchFilters {
            source_ids: vec![f.source.clone()],
            path_prefix: Some("alpha".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let hits = search_catalog(&f.query(), "commonword", &plan).unwrap();
    assert!(
        hits.hits.is_empty(),
        "pages have no supported source association field"
    );
    let selected = QueryPlan {
        filters: SearchFilters {
            path_prefix: Some("beta".into()),
            ..plan.filters.clone()
        },
        ..plan
    };
    assert_eq!(
        search_catalog(&f.query(), "commonword", &selected)
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
}
#[test]
fn incomplete_fact_layout_refuses_instead_of_silently_losing_exact_alias() {
    let f = Fixture::build(0, 0, false, 0);
    let old = search(&f.legacy, "AtlasAlias", &QueryPlan::default()).unwrap();
    assert!(old.hits[0].reasons.contains(&RetrievalReason::ExactAlias));
    let reader = f.query();
    let aliases: i64 = reader
        .connection()
        .query_row(
            "SELECT count(*) FROM registry_match_keys WHERE kind='alias'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        aliases, 0,
        "the actual layout0 builder never published alias membership"
    );
    assert_eq!(
        search_catalog(&reader, "AtlasAlias", &QueryPlan::default())
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable
    );
    assert_eq!(
        search_context_catalog(&reader, "AtlasAlias", &QueryPlan::default(), false)
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable
    );
    drop(reader);
    let db = Connection::open(&f.completed.path).unwrap();
    selector::configure_wal(&db).unwrap();
    db.execute_batch("UPDATE catalog_meta SET proof_layout_version=1")
        .unwrap();
    drop(db);
    assert_eq!(
        search_catalog(&f.query(), "AtlasAlias", &QueryPlan::default())
            .unwrap_err()
            .code,
        ErrorCode::CapabilityUnavailable
    );
    let supported = Fixture::new(0);
    let hits = supported.paired("AtlasAlias", &QueryPlan::default(), None);
    assert!(hits.hits[0].reasons.contains(&RetrievalReason::ExactAlias));
    assert!(
        hits.hits[0]
            .rank_contributions
            .iter()
            .any(|rank| rank.channel == "exact_alias" && rank.score.is_none())
    );
    // Captured-source fast-path does not depend on general registry membership.
    assert!(
        !search_indexed_sources(&f.query(), "currentpayload", &QueryPlan::default())
            .unwrap()
            .hits
            .is_empty()
    );
}
#[test]
fn long_record_metadata_source_filter_reports_native_time_separately_from_vm() {
    let short = Fixture::build(0, 100, true, 0);
    let long = Fixture::build(0, 100, true, 96 * 1024);
    let filtered = QueryPlan {
        filters: SearchFilters {
            source_ids: vec![id("source_absent_metadata_diagnostic")],
            ..Default::default()
        },
        limits: SearchLimits {
            candidates: 1,
            hits: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let a = work(&short.query(), "commonword", &filtered, 3);
    let reader = long.query();
    let metadata_bytes: i64 = reader
        .connection()
        .query_row(
            "SELECT sum(length(row_json)) FROM records WHERE path GLOB 'matching/*'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let adopted_rows: i64 = reader
        .connection()
        .query_row(
            "SELECT count(*) FROM records WHERE path GLOB 'matching/*'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        adopted_rows, 100,
        "diagnostic must use long adopted RecordRows, not invalid/plain body-only notes"
    );
    assert!(
        metadata_bytes >= 100 * 64 * 1024,
        "long canonical metadata fixture: {metadata_bytes}"
    );
    let b = work(&reader, "commonword", &filtered, 3);
    assert_eq!(a.rows, 0);
    assert_eq!(b.rows, 0);
    assert!(b.vm < 10_000_000, "{b:?}");
    // Native JSON parsing cost can rise without equivalent VM counter growth.
    // Report both rather than treating SQLITE_STMTSTATUS_VM_STEP as CPU work.
    eprintln!(
        "general lexical long RecordRow metadata/source-filter diagnostic metadata_bytes={metadata_bytes} adopted_rows={adopted_rows} short_vm={} long_vm={} short_elapsed_us={} long_elapsed_us={} short_sort={} long_sort={} short={a:?} long={b:?}",
        a.vm,
        b.vm,
        a.elapsed.as_micros(),
        b.elapsed.as_micros(),
        a.sort,
        b.sort
    );
    let hits = search_catalog(&reader, "commonword", &filtered).unwrap();
    eprintln!(
        "general lexical long RecordRow metadata selected usage hits={} usage={:?}",
        hits.hits.len(),
        reader.usage()
    );
    assert!(hits.hits.is_empty());
    assert_eq!(
        reader.usage().rows,
        0,
        "filtered RecordRow metadata is SQL/native work, not admitted payload decoding"
    );
}
#[test]
fn paired_unicode_normalization_preserves_raw_spans_and_exact_alias_bytes() {
    let f = Fixture::new(0);
    let raw = fs::read_to_string(f._temp.path().join("unicode.md")).unwrap();
    let plan = QueryPlan {
        filters: SearchFilters {
            path_prefix: Some("unicode.md".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    for query in ["cafe", "naive", "e\u{301}", "λ", "東京"] {
        let hits = f.paired(query, &plan, None);
        assert_eq!(hits.hits.len(), 1, "{query}");
        let hit = &hits.hits[0];
        assert_eq!(hit.excerpt.span.slice(&raw).unwrap(), hit.excerpt.text);
        assert!(!hit.excerpt.matched_spans.is_empty(), "{query}: {hit:?}");
        for span in &hit.excerpt.matched_spans {
            assert!(!span.slice(&raw).unwrap().is_empty());
        }
        if query == "cafe" {
            assert!(
                hit.excerpt
                    .matched_spans
                    .iter()
                    .any(|span| span.slice(&raw).unwrap() == "Café")
            );
        }
        if query == "naive" {
            assert!(
                hit.excerpt
                    .matched_spans
                    .iter()
                    .any(|span| span.slice(&raw).unwrap() == "naïve")
            );
        }
    }
    let exact = f.paired("Éclair 東京", &plan, None);
    assert_eq!(exact.hits.len(), 1);
    assert!(exact.hits[0].reasons.contains(&RetrievalReason::ExactAlias));
    for query in ["éclair 東京", "E\u{301}clair 東京"] {
        let folded = f.paired(query, &plan, None);
        assert_eq!(
            folded.hits.len(),
            1,
            "Unicode FTS OR query still matches 東京"
        );
        assert!(
            !folded.hits[0]
                .reasons
                .contains(&RetrievalReason::ExactAlias),
            "exact aliases remain byte/case sensitive: {query}"
        );
        assert!(folded.hits[0].reasons.contains(&RetrievalReason::Lexical));
    }
}
#[test]
fn paired_duplicate_id_search_keeps_invalid_discovery_without_identity_privilege() {
    let f = Fixture::new(0);
    assert!(
        !f.legacy
            .projection()
            .records
            .contains_key(&id("page_duplicate_general"))
    );
    let hits = f.paired("duplicateidentityneedle", &QueryPlan::default(), None);
    assert_eq!(hits.hits.len(), 2);
    assert_eq!(
        hits.hits
            .iter()
            .map(|hit| hit.locator.path.clone())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([path("duplicate-a.md"), path("duplicate-b.md")])
    );
    for hit in &hits.hits {
        assert_eq!(hit.eligibility, Eligibility::Invalid);
        assert!(hit.locator.record.is_none());
        assert!(!hit.reasons.contains(&RetrievalReason::ExactId));
        assert!(!hit.reasons.contains(&RetrievalReason::ExactAlias));
    }
    for query in ["page_duplicate_general", "DuplicatedAlias"] {
        assert!(f.paired(query, &QueryPlan::default(), None).hits.is_empty());
    }
    for historical in [false, true] {
        assert!(
            f.paired(
                "duplicateidentityneedle",
                &QueryPlan::default(),
                Some(historical)
            )
            .hits
            .is_empty()
        );
    }
}
#[test]
fn paired_withdrawn_source_and_deprecated_page_preserve_lifecycle_filters() {
    let f = Fixture::new(0);
    for query in ["lifecyclewithdrawnneedle", "lifecycledeprecatedneedle"] {
        assert!(f.paired(query, &QueryPlan::default(), None).hits.is_empty());
        assert!(
            f.paired(query, &QueryPlan::default(), Some(false))
                .hits
                .is_empty()
        );
        let history = QueryPlan {
            filters: SearchFilters {
                include_historical: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(!f.paired(query, &history, None).hits.is_empty());
        assert!(!f.paired(query, &history, Some(true)).hits.is_empty());
    }
    let withdrawn = QueryPlan {
        filters: SearchFilters {
            source_ids: vec![f.withdrawn_source.clone()],
            include_historical: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let hits = f.paired("lifecyclewithdrawnneedle", &withdrawn, None);
    let payload = hits
        .hits
        .iter()
        .find(|hit| hit.owner_revision.as_ref() == Some(&f.withdrawn_head))
        .unwrap();
    assert_eq!(payload.eligibility, Eligibility::Withdrawn);
    assert_eq!(payload.source_id.as_ref(), Some(&f.withdrawn_source));
    assert!(payload.excerpt.citation.is_none());
    let deprecated = QueryPlan {
        filters: SearchFilters {
            authored_statuses: vec!["deprecated".into()],
            include_historical: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let hits = f.paired("lifecycledeprecatedneedle", &deprecated, None);
    assert_eq!(hits.hits.len(), 1);
    assert_eq!(hits.hits[0].authored_status.as_deref(), Some("deprecated"));
}
