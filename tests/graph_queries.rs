use lwiki::{
    catalog::*,
    changes::*,
    domain::*,
    graph::{query::*, *},
    retrieval::SearchFilters,
    sources::{evidence::exact_quote_body, *},
    vault::*,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};
fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
fn note(kind: &str, id: &str, title: &str, extra: Value, body: &str) -> Vec<u8> {
    let mut fields = BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_id".into(), json!(id)),
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
    for (k, v) in fields {
        bytes.extend_from_slice(format!("{k}: {v}\n").as_bytes());
    }
    bytes.extend_from_slice(b"---\n");
    bytes.extend_from_slice(body.as_bytes());
    bytes
}
fn write(root: &Path, name: &str, bytes: &[u8]) {
    let full = root.join(name);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, bytes).unwrap();
}
struct Fixture {
    temp: tempfile::TempDir,
    root: VaultRoot,
    catalog: Catalog,
    source: SourcePlan,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        write(
            temp.path(),
            "WIKI.md",
            &note("vault", "graph_vault", "Graph", json!({}), ""),
        );
        let root = VaultRoot::explicit(temp.path()).unwrap();
        let catalog = Catalog::new(VaultFs::new(root.clone()), id("graph_vault"));
        let writer = WriterPermit::acquire(&root, Duration::from_millis(1000)).unwrap();
        let source = SourceStore::new(VaultFs::new(root.clone()))
            .plan_capture(CaptureRequest {
                title: "Graph support".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "fixture".into(),
                original: b"Exact support. Contradiction.".to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: None,
            })
            .unwrap();
        let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
        let staged = engine
            .prepare(&writer, source.draft.clone().unwrap())
            .unwrap()
            .prepared;
        engine
            .apply(&writer, &staged, &CatalogGraphValidator, &catalog)
            .unwrap();
        drop(writer);
        Self {
            temp,
            root,
            catalog,
            source,
        }
    }
    fn entity(&self, name: &str, title: &str) {
        write(
            self.temp.path(),
            &format!("entities/{name}.md"),
            &note(
                "entity",
                name,
                title,
                json!({"wiki_status":"active","wiki_entity_type":"component"}),
                "Unsupported secret body",
            ),
        );
    }
    fn assertion(&self, name: &str, subject: &str, object: &str, extra: Value) {
        let mut fields = json!({"wiki_status":"accepted","wiki_subject_id":subject,"wiki_predicate":"depends_on","wiki_object_id":object});
        fields
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        write(
            self.temp.path(),
            &format!("assertions/{name}.md"),
            &note("assertion", name, name, fields, "Dependency description"),
        );
        self.evidence(&format!("ev_{name}"), name, true);
    }
    fn evidence(&self, name: &str, assertion: &str, support: bool) {
        let quote = if support {
            b"Exact support.".as_slice()
        } else {
            b"Contradiction.".as_slice()
        };
        let start = if support { 0 } else { 15 };
        write(
            self.temp.path(),
            &format!("evidence/{name}.md"),
            &note(
                "evidence",
                name,
                name,
                json!({"wiki_status":"active","wiki_assertion_id":assertion,"wiki_source_id":self.source.source_id,"wiki_source_revision":self.source.revision_id,"wiki_stance":if support{"supports"}else{"contradicts"},"wiki_locator_kind":"utf8-bytes","wiki_span_start":start,"wiki_span_end":start+quote.len(),"wiki_quote_hash":Blake3Hash::digest(quote)}),
                &String::from_utf8(exact_quote_body(quote, "\n", "Fixture").unwrap()).unwrap(),
            ),
        );
    }
    fn reader(&self) -> ReaderSnapshot {
        let writer = WriterPermit::acquire(&self.root, Duration::from_millis(1000)).unwrap();
        self.catalog.sync(&writer).unwrap();
        self.catalog.verified_snapshot(None).unwrap()
    }
}
#[test]
fn a_b_c_path_does_not_create_a_c_fact() {
    let f = Fixture::new();
    for (name, bytes) in [
        ("a", include_bytes!("fixtures/p08/a.md").as_slice()),
        ("b", include_bytes!("fixtures/p08/b.md").as_slice()),
        ("c", include_bytes!("fixtures/p08/c.md").as_slice()),
    ] {
        write(f.temp.path(), &format!("entities/{name}.md"), bytes);
    }
    f.assertion("ab", "a", "b", json!({}));
    f.assertion("bc", "b", "c", json!({"wiki_predicate":"maintains"}));
    let r = f.reader();
    let mut plan = GraphPlan {
        strategy: GraphStrategy::Entity,
        ..Default::default()
    };
    plan.limits.depth = 2;
    let result = query(&r, "a", &plan).unwrap();
    assert_eq!(result.assertions.len(), 2);
    let bc = result
        .assertions
        .iter()
        .find(|e| e.record_ref.record_id == id("bc"))
        .unwrap();
    assert_eq!(bc.path.len(), 2);
    assert_eq!(bc.subject.record_id, id("b"));
    assert_eq!(bc.predicate, "maintains");
    assert!(result.assertions.iter().all(|e|!(e.subject.record_id==id("a") && matches!(&e.object,GraphObject::Entity{record_ref} if record_ref.record_id==id("c")))));
    assert_eq!(bc.path[0].assertion.record_id, id("ab"));
    assert_eq!(bc.path[1].assertion.record_id, id("bc"));
    assert_eq!(bc.path[1].traversal, TraversalDirection::Outgoing);
    let incoming = neighbors(&r, &id("c"), &plan).unwrap();
    let edge = &incoming.assertions[0];
    assert_eq!(edge.subject.record_id, id("b"));
    assert_eq!(edge.path[0].traversal, TraversalDirection::Incoming);
}
#[test]
fn opposite_negated_dated_literal_disputed_edges() {
    let f = Fixture::new();
    f.entity("a", "A");
    f.entity("b", "B");
    f.assertion(
        "ab",
        "a",
        "b",
        json!({"wiki_valid_from":"2020-01-01","wiki_valid_until":"2021-01-01"}),
    );
    f.assertion(
        "ba",
        "b",
        "a",
        json!({"wiki_negated":true,"wiki_modality":"possible"}),
    );
    write(
        f.temp.path(),
        "assertions/literal.md",
        &note(
            "assertion",
            "literal",
            "Decimal",
            json!({"wiki_status":"accepted","wiki_subject_id":"a","wiki_predicate":"has_property","wiki_property":"cost","wiki_literal_type":"decimal","wiki_literal_value":"2.50","wiki_unit":"USD"}),
            "Cost",
        ),
    );
    f.evidence("ev_literal", "literal", true);
    f.evidence("contradict_ab", "ab", false);
    let r = f.reader();
    let out = neighbors(&r, &id("a"), &GraphPlan::default()).unwrap();
    assert_eq!(out.assertions.len(), 3);
    let ab = out
        .assertions
        .iter()
        .find(|e| e.record_ref.record_id == id("ab"))
        .unwrap();
    assert!(ab.disputed);
    assert_eq!(ab.qualifiers.valid_until.as_deref(), Some("2021-01-01"));
    assert_eq!(ab.support.len(), 1);
    assert_eq!(ab.contradictions.len(), 1);
    assert_eq!(ab.contradictions[0].stance, EvidenceStance::Contradicts);
    let ba = out
        .assertions
        .iter()
        .find(|e| e.record_ref.record_id == id("ba"))
        .unwrap();
    assert_eq!(ba.subject.record_id, id("b"));
    assert!(ba.qualifiers.negated);
    assert_eq!(ba.qualifiers.modality, "possible");
    let literal = out
        .assertions
        .iter()
        .find(|e| e.record_ref.record_id == id("literal"))
        .unwrap();
    assert_eq!(
        literal.object,
        GraphObject::Literal {
            literal_type: "decimal".into(),
            value: "2.50".into()
        }
    );
    assert_eq!(literal.qualifiers.unit.as_deref(), Some("USD"));
    let view = SourceView::from_fs(f.catalog.fs()).unwrap();
    for edge in &out.assertions {
        for evidence in edge.support.iter().chain(&edge.contradictions) {
            assert!(
                !view
                    .verify(evidence.citation.as_ref().unwrap(), CitationScope::Current)
                    .unwrap()
                    .quote
                    .is_empty()
            );
        }
    }
}
#[test]
fn homonym_labels_never_merge() {
    let f = Fixture::new();
    for name in ["ada1", "ada2"] {
        write(
            f.temp.path(),
            &format!("entities/{name}.md"),
            &note(
                "entity",
                name,
                "Ada",
                json!({"wiki_status":"active","wiki_entity_type":"person","aliases":["Same alias"]}),
                "UnsupportedDescription",
            ),
        );
    }
    let r = f.reader();
    let result = query(&r, "Same alias", &GraphPlan::default()).unwrap();
    assert_eq!(result.seeds.len(), 2);
    assert_eq!(result.entities.len(), 2);
    assert_ne!(result.entities[0].record_ref, result.entities[1].record_ref);
    assert!(result.entities.iter().all(|e| e.description.is_none()));
    assert!(
        query(&r, "UnsupportedDescription", &GraphPlan::default())
            .unwrap()
            .seeds
            .is_empty()
    );
    let mut history = GraphPlan::default();
    history.filters.include_historical = true;
    let result = query(&r, "Ada", &history).unwrap();
    assert_eq!(result.entities.len(), 2);
    assert!(
        result
            .entities
            .iter()
            .all(|e| e.description.as_deref() == Some("UnsupportedDescription"))
    );
    assert_eq!(
        query(&r, "UnsupportedDescription", &history)
            .unwrap()
            .seeds
            .len(),
        2
    );
}
#[test]
fn hub_cycle_depth_and_incident_caps() {
    let f = Fixture::new();
    f.entity("hub", "Hub");
    f.entity("small", "Small");
    for n in 0..20 {
        let leaf = format!("leaf{n:02}");
        f.entity(&leaf, &leaf);
        f.assertion(&format!("hub{n:02}"), "hub", &leaf, json!({}));
    }
    f.assertion("small_edge", "small", "leaf00", json!({}));
    f.assertion("cycle", "leaf00", "hub", json!({}));
    let r = f.reader();
    let mut plan = GraphPlan::default();
    plan.limits.hits = 50;
    plan.limits.depth = 2;
    plan.limits.incident_per_seed = 2;
    let result = query(&r, "Hub Small", &plan).unwrap();
    assert!(
        result
            .assertions
            .iter()
            .any(|e| e.record_ref.record_id == id("small_edge"))
    );
    assert!(result.coverage.omitted_incident_assertions > 0);
    assert!(result.truncated);
    assert!(result.coverage.visited_assertions <= 128);
    assert!(result.assertions.iter().all(|e| e.path.len() <= 2));
    let ids: std::collections::BTreeSet<_> = result
        .assertions
        .iter()
        .map(|e| e.record_ref.record_id.clone())
        .collect();
    assert_eq!(ids.len(), result.assertions.len());
    plan.limits.assertions = 2;
    let bounded = query(&r, "Hub Small", &plan).unwrap();
    assert_eq!(bounded.coverage.visited_assertions, 2);
    assert!(
        bounded
            .assertions
            .iter()
            .any(|e| e.record_ref.record_id == id("small_edge"))
    );
}
#[test]
fn graph_lexical_zero_model_calls() {
    let f = Fixture::new();
    f.entity("a", "A");
    f.entity("b", "B");
    f.assertion("ab", "a", "b", json!({}));
    let r = f.reader();
    let before = query(&r, "depends_on OR a*", &GraphPlan::default()).unwrap();
    assert!(!before.assertions.is_empty());
    drop(r);
    fs::remove_file(f.temp.path().join(".wiki/cache/index.sqlite")).unwrap();
    let writer = WriterPermit::acquire(&f.root, Duration::from_millis(1000)).unwrap();
    f.catalog.rebuild(&writer).unwrap();
    drop(writer);
    let r = f.catalog.verified_snapshot(None).unwrap();
    let after = query(&r, "depends_on OR a*", &GraphPlan::default()).unwrap();
    assert_eq!(before.assertions, after.assertions);
    assert_eq!(before.entities, after.entities);
    assert_eq!(before.dependency_fingerprint, after.dependency_fingerprint);
    let snapshot = f.catalog.index_snapshot().unwrap();
    assert!(
        neighbors(&snapshot, &id("a"), &GraphPlan::default())
            .unwrap()
            .assertions
            .iter()
            .flat_map(|e| &e.support)
            .all(|e| e.citation.is_none())
    );
}
#[test]
fn filtered_graph_candidates_rank_and_cursor_are_stable() {
    let f = Fixture::new();
    for n in 0..8 {
        let name = format!("bad{n}");
        write(
            f.temp.path(),
            &format!("entities/{name}.md"),
            &note(
                "entity",
                &name,
                "Needle",
                json!({"wiki_status":"active","wiki_entity_type":"concept","tags":["wrong"]}),
                "",
            ),
        );
    }
    for name in ["a", "b", "c"] {
        write(
            f.temp.path(),
            &format!("entities/{name}.md"),
            &note(
                "entity",
                name,
                "Needle",
                json!({"wiki_status":"active","wiki_entity_type":"concept","tags":["keep"]}),
                "",
            ),
        );
    }
    f.assertion("ab", "a", "b", json!({"tags":["keep"]}));
    f.assertion("bc", "b", "c", json!({"tags":["keep"]}));
    let r = f.reader();
    let mut plan = GraphPlan {
        filters: SearchFilters {
            tags: vec!["keep".into()],
            ..Default::default()
        },
        strategy: GraphStrategy::Entity,
        ..Default::default()
    };
    plan.limits.candidates = 2;
    plan.limits.hits = 1;
    let first = query(&r, "Needle", &plan).unwrap();
    assert_eq!(first.seeds.len(), 2);
    assert!(
        first
            .seeds
            .iter()
            .all(|s| !s.record_ref.record_id.as_str().starts_with("bad"))
    );
    assert_eq!(first.assertions.len(), 1);
    plan.cursor = first.next_cursor;
    let second = query(&r, "Needle", &plan).unwrap();
    assert_eq!(second.assertions.len(), 1);
    assert_ne!(
        first.assertions[0].record_ref,
        second.assertions[0].record_ref
    );
    assert_eq!(
        query(&r, "Changed", &plan).unwrap_err().code,
        ErrorCode::CursorStale
    );
    let direct = query(&r, "ab", &GraphPlan::default()).unwrap();
    assert!(direct.assertions[0].direct_seed);
    assert!(
        direct.assertions[0]
            .rank_contributions
            .iter()
            .all(|c| c.channel != "parent_seed")
    );
    assert!(
        direct.seeds[0]
            .rank_contributions
            .iter()
            .any(|c| c.channel.ends_with("exact_id"))
    );
}
#[test]
fn navigation_only_neighbors_page_without_asserted_facts() {
    let f = Fixture::new();
    for name in ["a", "b", "c"] {
        write(
            f.temp.path(),
            &format!("pages/{name}.md"),
            &note(
                "page",
                name,
                name,
                json!({"wiki_status":"reviewed"}),
                if name == "a" {
                    "[[b.md]] and [[c.md]]"
                } else {
                    "body"
                },
            ),
        );
    }
    let r = f.reader();
    let mut plan = GraphPlan {
        include_navigation: true,
        ..Default::default()
    };
    plan.limits.hits = 1;
    let first = neighbors(&r, &id("a"), &plan).unwrap();
    assert!(first.assertions.is_empty());
    assert_eq!(first.navigation.len(), 1);
    assert_eq!(first.navigation[0].reason, NavigationReason::PageLink);
    plan.cursor = first.next_cursor;
    assert!(plan.cursor.is_some());
    let second = neighbors(&r, &id("a"), &plan).unwrap();
    assert_eq!(second.navigation.len(), 1);
    assert_ne!(first.navigation, second.navigation);
    assert!(second.next_cursor.is_none());
    let source = neighbors(
        &r,
        &f.source.source_id,
        &GraphPlan {
            include_navigation: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        source
            .navigation
            .iter()
            .any(|e| e.reason == NavigationReason::Provenance)
    );
}
#[test]
fn proposals_history_and_evidence_omissions_remain_discovery() {
    let f = Fixture::new();
    f.entity("a", "A");
    f.entity("b", "B");
    f.assertion("ab", "a", "b", json!({}));
    for name in ["ev2", "ev3"] {
        f.evidence(name, "ab", true);
    }
    f.assertion("proposal", "a", "b", json!({"wiki_status":"proposed"}));
    let r = f.reader();
    let current = neighbors(&r, &id("a"), &GraphPlan::default()).unwrap();
    assert_eq!(current.assertions.len(), 1);
    assert_eq!(current.assertions[0].support.len(), 2);
    assert_eq!(current.assertions[0].omitted_support, 1);
    let mut proposed = GraphPlan::default();
    proposed.filters.include_proposed = true;
    let visible = neighbors(&r, &id("a"), &proposed).unwrap();
    assert_eq!(visible.assertions.len(), 2);
    assert!(
        visible
            .assertions
            .iter()
            .find(|e| e.record_ref.record_id == id("proposal"))
            .unwrap()
            .eligibility
            != Eligibility::Current
    );
    assert!(
        visible
            .assertions
            .iter()
            .find(|e| e.record_ref.record_id == id("proposal"))
            .unwrap()
            .support
            .iter()
            .all(|e| e.citation.is_none())
    );
    drop(r);
    let writer = WriterPermit::acquire(&f.root, Duration::from_millis(1000)).unwrap();
    let engine = ChangeEngine::new(VaultFs::new(f.root.clone())).unwrap();
    let draft = SourceStore::new(VaultFs::new(f.root.clone()))
        .plan_withdraw(&f.source.source_id, "test")
        .unwrap()
        .draft
        .unwrap();
    let change = engine.prepare(&writer, draft).unwrap().prepared;
    engine
        .apply(&writer, &change, &CatalogGraphValidator, &f.catalog)
        .unwrap();
    drop(writer);
    let r = f.catalog.verified_snapshot(None).unwrap();
    assert!(
        neighbors(&r, &id("a"), &GraphPlan::default())
            .unwrap()
            .assertions
            .is_empty()
    );
    let mut history = GraphPlan::default();
    history.filters.include_historical = true;
    let historical = neighbors(&r, &id("a"), &history).unwrap();
    assert_eq!(historical.assertions.len(), 1);
    assert!(historical.assertions[0].eligibility != Eligibility::Current);
    assert_eq!(
        historical.assertions[0].support[0].eligibility,
        Eligibility::Withdrawn
    );
}
#[test]
fn graph_limits_and_duplicate_identity_refuse_unsafe_requests() {
    let f = Fixture::new();
    f.entity("a", "A");
    write(
        f.temp.path(),
        "copy.md",
        &fs::read(f.temp.path().join("entities/a.md")).unwrap(),
    );
    let r = f.reader();
    assert_eq!(
        neighbors(&r, &id("a"), &GraphPlan::default())
            .unwrap_err()
            .code,
        ErrorCode::ReferenceAmbiguous
    );
    let mut plan = GraphPlan::default();
    plan.limits.depth = 3;
    assert_eq!(validate_plan(&plan).unwrap_err().code, ErrorCode::Usage);
    assert_eq!(
        query(&r, "** ::", &GraphPlan::default()).unwrap_err().code,
        ErrorCode::Usage
    );
    let before = fs::read(f.temp.path().join(".wiki/cache/index.sqlite")).unwrap();
    validate_plan(&GraphPlan::default()).unwrap();
    assert_eq!(
        before,
        fs::read(f.temp.path().join(".wiki/cache/index.sqlite")).unwrap()
    );
}

#[test]
fn later_better_parent_replaces_expanded_rank_and_recorded_path() {
    let f = Fixture::new();
    f.entity("a", "Anchor");
    f.entity("b", "Middle");
    f.entity("c", "Anchor");
    f.assertion("ab", "a", "b", json!({}));
    f.assertion("bc", "b", "c", json!({}));
    let r = f.reader();
    let mut plan = GraphPlan {
        strategy: GraphStrategy::Entity,
        ..Default::default()
    };
    plan.limits.depth = 2;
    let result = query(&r, "Anchor", &plan).unwrap();
    assert_eq!(result.seeds[0].record_ref.record_id, id("a"));
    assert_eq!(result.seeds[1].record_ref.record_id, id("c"));
    let bc = result
        .assertions
        .iter()
        .find(|e| e.record_ref.record_id == id("bc"))
        .unwrap();
    assert!(!bc.direct_seed);
    assert_eq!(bc.rank_contributions.len(), 1);
    assert_eq!(bc.rank_contributions[0].channel, "parent_seed");
    assert_eq!(bc.rank_contributions[0].rank, 1);
    assert_eq!(bc.hop, 2);
    assert_eq!(
        bc.path
            .iter()
            .map(|p| p.assertion.record_id.clone())
            .collect::<Vec<_>>(),
        [id("ab"), id("bc")]
    );
    assert_eq!(bc.seed_ids, [id("a"), id("c")]);
    assert_eq!(bc.rrf_score, 1.0 / 61.0);
}

#[test]
fn navigation_shares_incident_and_total_budgets() {
    let f = Fixture::new();
    let mut links = String::new();
    for n in 0..5 {
        let name = format!("p{n}");
        write(
            f.temp.path(),
            &format!("pages/{name}.md"),
            &note("page", &name, &name, json!({"wiki_status":"reviewed"}), ""),
        );
        links.push_str(&format!("[[{name}.md]] "));
    }
    write(
        f.temp.path(),
        "pages/root.md",
        &note(
            "page",
            "page_root",
            "Root",
            json!({"wiki_status":"reviewed"}),
            &links,
        ),
    );
    let r = f.reader();
    let mut plan = GraphPlan {
        include_navigation: true,
        ..Default::default()
    };
    plan.limits.incident_per_seed = 2;
    let result = neighbors(&r, &id("page_root"), &plan).unwrap();
    assert_eq!(result.navigation.len(), 2);
    assert_eq!(result.coverage.omitted_navigation, 3);
    plan.limits.assertions = 1;
    let result = neighbors(&r, &id("page_root"), &plan).unwrap();
    assert_eq!(result.navigation.len(), 1);
    assert_eq!(result.coverage.omitted_navigation, 4);
    assert!(result.truncated);
}

#[test]
fn original_seed_budget_spans_branched_depth_two() {
    let f = Fixture::new();
    for name in ["a", "b", "c", "d", "e"] {
        f.entity(name, name);
    }
    f.assertion("zz_ab", "a", "b", json!({}));
    f.assertion("zz_ac", "a", "c", json!({}));
    f.assertion("aa_bd", "b", "d", json!({}));
    f.assertion("aa_ce", "c", "e", json!({}));
    let r = f.reader();
    let mut plan = GraphPlan::default();
    plan.limits.depth = 2;
    plan.limits.hits = 50;
    plan.limits.incident_per_seed = 3;
    let result = neighbors(&r, &id("a"), &plan).unwrap();
    assert_eq!(result.assertions.len(), 3);
    assert_eq!(result.coverage.visited_assertions, 3);
    assert_eq!(result.coverage.omitted_incident_assertions, 1);
    assert!(result.assertions.iter().any(|e| e.hop == 2));
    assert!(result.truncated);
    plan.limits.incident_per_seed = 1;
    let one = neighbors(&r, &id("a"), &plan).unwrap();
    assert_eq!(one.assertions.len(), 1);
    assert_eq!(one.assertions[0].record_ref.record_id, id("zz_ab"));
    assert!(one.coverage.omitted_incident_assertions >= 2);
    let direct = neighbors(&r, &id("zz_ab"), &plan).unwrap();
    assert_eq!(direct.assertions.len(), 1);
    assert!(direct.assertions[0].direct_seed);
    assert_eq!(direct.assertions[0].direct_seed_rank, Some(1));
    assert!(direct.coverage.omitted_assertions > 0);
}

#[test]
fn navigation_budget_spans_hops_and_rejection_does_not_hide_other_seed() {
    let f = Fixture::new();
    for (name, body) in [("a", "[[b.md]]"), ("b", "[[c.md]]"), ("c", "")] {
        write(
            f.temp.path(),
            &format!("pages/{name}.md"),
            &note("page", name, name, json!({"wiki_status":"reviewed"}), body),
        );
    }
    let r = f.reader();
    let mut plan = GraphPlan {
        include_navigation: true,
        ..Default::default()
    };
    plan.limits.depth = 2;
    plan.limits.incident_per_seed = 1;
    let result = neighbors(&r, &id("a"), &plan).unwrap();
    assert_eq!(result.navigation.len(), 1);
    assert_eq!(
        result.navigation[0].to.record.as_ref().unwrap().record_id,
        id("b")
    );
    assert_eq!(result.coverage.omitted_navigation, 1);
    drop(r);
    for name in ["a", "b", "c"] {
        fs::remove_file(f.temp.path().join(format!("pages/{name}.md"))).unwrap();
    }
    f.entity("a", "Anchor");
    f.entity("b", "Middle");
    f.entity("c", "Anchor");
    write(
        f.temp.path(),
        "entities/a.md",
        &note(
            "entity",
            "a",
            "Anchor",
            json!({"wiki_status":"active","wiki_entity_type":"component"}),
            "[[c.md]]",
        ),
    );
    f.assertion("ab", "a", "b", json!({}));
    let r = f.reader();
    plan.strategy = GraphStrategy::Entity;
    let result = query(&r, "Anchor", &plan).unwrap();
    assert_eq!(result.assertions.len(), 1);
    assert_eq!(result.navigation.len(), 1);
    assert_eq!(
        result.navigation[0].from.record.as_ref().unwrap().record_id,
        id("a")
    );
    assert_eq!(
        result.navigation[0].to.record.as_ref().unwrap().record_id,
        id("c")
    );
    assert_eq!(result.coverage.omitted_navigation, 0);
}

#[test]
fn direct_assertion_pages_preserve_fused_exact_id_seed_order() {
    let f = Fixture::new();
    f.entity("a", "A");
    f.entity("b", "B");
    f.assertion(
        "zzz_target",
        "a",
        "b",
        json!({"wiki_status":"proposed","title":"Other title"}),
    );
    f.assertion(
        "aaa_title",
        "a",
        "b",
        json!({"wiki_status":"proposed","title":"zzz_target"}),
    );
    let r = f.reader();
    let mut plan = GraphPlan {
        strategy: GraphStrategy::Relationship,
        ..Default::default()
    };
    plan.filters.include_proposed = true;
    plan.limits.hits = 1;
    let first = query(&r, "zzz_target", &plan).unwrap();
    assert_eq!(first.seeds[0].record_ref.record_id, id("zzz_target"));
    assert_eq!(first.seeds[1].record_ref.record_id, id("aaa_title"));
    assert_eq!(first.assertions[0].record_ref.record_id, id("zzz_target"));
    assert_eq!(first.assertions[0].direct_seed_rank, Some(1));
    assert!(
        !first.assertions[0]
            .rank_contributions
            .iter()
            .any(|c| c.channel == "parent_seed")
    );
    plan.cursor = first.next_cursor;
    let second = query(&r, "zzz_target", &plan).unwrap();
    assert_eq!(second.assertions[0].record_ref.record_id, id("aaa_title"));
    assert_eq!(second.assertions[0].direct_seed_rank, Some(2));
}

#[test]
fn invalid_historical_assertion_never_emits_citation_authority() {
    let f = Fixture::new();
    f.entity("a", "A");
    f.entity("b", "B");
    f.assertion("ab", "a", "b", json!({"wiki_depends_on_ids":["missing"]}));
    let r = f.reader();
    let mut plan = GraphPlan::default();
    plan.filters.include_historical = true;
    let result = neighbors(&r, &id("ab"), &plan).unwrap();
    assert_eq!(result.assertions.len(), 1);
    assert_eq!(result.assertions[0].eligibility, Eligibility::Invalid);
    assert!(
        result.assertions[0]
            .support
            .iter()
            .all(|e| e.citation.is_none())
    );
    assert_eq!(
        result.assertions[0].support[0].eligibility,
        Eligibility::Invalid
    );
}

#[test]
fn navigation_round_robin_and_depth_omissions_are_visible() {
    let f = Fixture::new();
    for (name, body) in [("a", "[[b.md]]"), ("b", "[[c.md]]"), ("c", "")] {
        write(
            f.temp.path(),
            &format!("pages/{name}.md"),
            &note("page", name, name, json!({"wiki_status":"reviewed"}), body),
        );
    }
    let r = f.reader();
    let plan = GraphPlan {
        include_navigation: true,
        ..Default::default()
    };
    let chain = neighbors(&r, &id("a"), &plan).unwrap();
    assert_eq!(chain.navigation.len(), 1);
    assert!(chain.coverage.depth_limited && chain.truncated);
    drop(r);
    write(
        f.temp.path(),
        "pages/b.md",
        &note("page", "b", "b", json!({"wiki_status":"reviewed"}), ""),
    );
    let r = f.reader();
    let backlink_only = neighbors(&r, &id("a"), &plan).unwrap();
    assert!(!backlink_only.coverage.depth_limited);
    assert!(!backlink_only.truncated);
    drop(r);
    for name in ["a", "b", "c"] {
        fs::remove_file(f.temp.path().join(format!("pages/{name}.md"))).unwrap();
    }
    let mut links = String::new();
    for n in 0..4 {
        let target = format!("leaf{n}");
        f.entity(&target, &target);
        links.push_str(&format!("[[{target}.md]] "));
    }
    f.entity("last", "Last");
    write(
        f.temp.path(),
        "entities/a.md",
        &note(
            "entity",
            "a",
            "Anchor",
            json!({"wiki_status":"active","wiki_entity_type":"component"}),
            &links,
        ),
    );
    write(
        f.temp.path(),
        "entities/c.md",
        &note(
            "entity",
            "c",
            "Anchor",
            json!({"wiki_status":"active","wiki_entity_type":"component"}),
            "[[last.md]]",
        ),
    );
    let r = f.reader();
    let mut plan = GraphPlan {
        strategy: GraphStrategy::Entity,
        include_navigation: true,
        ..Default::default()
    };
    plan.limits.assertions = 2;
    let fair = query(&r, "Anchor", &plan).unwrap();
    assert_eq!(fair.navigation.len(), 2);
    assert!(
        fair.navigation
            .iter()
            .any(|n| n.from.record.as_ref().unwrap().record_id == id("a"))
    );
    assert!(
        fair.navigation
            .iter()
            .any(|n| n.from.record.as_ref().unwrap().record_id == id("c"))
    );
}

#[test]
fn typed_depth_two_rotates_origins_and_repeats_do_not_consume_turns() {
    let f = Fixture::new();
    f.entity("a", "Anchor");
    f.entity("z", "Anchor");
    for n in 0..3 {
        let left = format!("b{n}");
        let right = format!("y{n}");
        let end = format!("d{n}");
        f.entity(&left, &left);
        f.entity(&right, &right);
        f.entity(&end, &end);
        f.assertion(&format!("zz_a{n}"), "a", &left, json!({}));
        f.assertion(&format!("zz_z{n}"), "z", &right, json!({}));
        f.assertion(&format!("aa_b{n}"), &left, &end, json!({}));
    }
    f.entity("last", "Last");
    f.assertion("aa_y2", "y2", "last", json!({}));
    let r = f.reader();
    let mut plan = GraphPlan {
        strategy: GraphStrategy::Entity,
        ..Default::default()
    };
    plan.limits.depth = 2;
    plan.limits.hits = 50;
    plan.limits.assertions = 8;
    let result = query(&r, "Anchor", &plan).unwrap();
    assert_eq!(result.coverage.visited_assertions, 8);
    assert!(
        result
            .assertions
            .iter()
            .any(|e| e.record_ref.record_id == id("aa_y2"))
    );
    assert_eq!(
        result
            .assertions
            .iter()
            .filter(|e| e.record_ref.record_id.as_str().starts_with("aa_b"))
            .count(),
        1
    );
}
