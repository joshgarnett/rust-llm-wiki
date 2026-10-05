use super::*;
use crate::{
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub(crate) fn id(text: &str) -> RecordId {
    RecordId::new(text).unwrap()
}
pub(crate) struct Fixture {
    _temp: tempfile::TempDir,
    pub(crate) root: PathBuf,
    pub(crate) catalog: Catalog,
    pub(crate) source: RecordId,
    revision: RecordId,
    pub(crate) second_source: RecordId,
    second_revision: RecordId,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("graph fixture with spaces");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("WIKI.md"),"---\nwiki_schema: '1'\nwiki_id: vault_named_neighbors\nwiki_kind: vault\ntitle: Neighbors\n---\n").unwrap();
        let handle = VaultFs::new(VaultRoot::explicit(&root).unwrap());
        let capture = |name: &str| {
            let plan=SourceStore::new(handle.clone()).plan_capture(CaptureRequest {
                title:name.into(),origin_kind:SourceOrigin::LocalFile,origin:format!("{name}.txt"),
                original:b"Relay uses Cedar. Cedar feeds Birch. Incoming Birch relay. Delay is 17 ms. Caf\xc3\xa9.".to_vec(),
                extraction:ExtractionInput::Utf8Preserve,media_type:None}).unwrap();
            for operation in plan.draft.unwrap().operations {
                let target = root.join(operation.target.as_str());
                fs::create_dir_all(target.parent().unwrap()).unwrap();
                fs::write(target, operation.proposed.unwrap()).unwrap();
            }
            (plan.source_id, plan.revision_id)
        };
        let (source, revision) = capture("first");
        let (second_source, second_revision) = capture("second");
        let catalog = Catalog::new(handle, id("vault_named_neighbors"));
        let f = Self {
            _temp: temp,
            root,
            catalog,
            source,
            revision,
            second_source,
            second_revision,
        };
        for name in ["relay", "cedar", "birch"] {
            f.note(&format!("{name}.md"),&format!("wiki_id: entity_{name}\nwiki_kind: entity\ntitle: {name}\nwiki_status: active\nwiki_entity_type: component\naliases: [{name}-alias]"),"Unsupported descriptive body.");
        }
        f.edge("a", "relay", Some("cedar"), "uses", false);
        f.edge("b", "cedar", Some("birch"), "depends_on", false);
        f.edge("c", "birch", Some("relay"), "uses", false);
        f.edge("d", "relay", None, "has_property", false);
        f.edge("e", "relay", Some("cedar"), "uses", true);
        f.evidence("a", "second", "supports", true);
        f.evidence("a", "contrary", "contradicts", false);
        f.rebuild();
        f
    }
    pub(crate) fn note(&self, path: &str, fields: &str, body: &str) {
        let text = format!("---\nwiki_schema: '1'\n{fields}\n---\n{body}\n");
        let parsed = crate::records::parse_note(text.as_bytes());
        assert!(
            parsed.canonical.is_some(),
            "invalid canonical fixture note {path}: {:?}",
            parsed.diagnostics
        );
        fs::write(self.root.join(path), text).unwrap();
    }
    fn edge(&self, key: &str, subject: &str, object: Option<&str>, predicate: &str, negated: bool) {
        let object=object.map(|s|format!("wiki_object_id: entity_{s}")).unwrap_or("wiki_literal_type: decimal\nwiki_literal_value: '17'\nwiki_unit: ms\nwiki_property: timeout".into());
        self.note(&format!("assertion-{key}.md"),&format!("wiki_id: assertion_{key}\nwiki_kind: assertion\ntitle: Relation {key}\nwiki_status: accepted\nwiki_subject_id: entity_{subject}\n{object}\nwiki_predicate: {predicate}\nwiki_negated: {negated}\nwiki_modality: possible\nwiki_valid_from: '2026-10-01'"),"Recorded relation.");
        self.evidence(key, "support", "supports", false);
    }
    fn evidence(&self, key: &str, suffix: &str, stance: &str, second: bool) {
        let (source, revision) = if second {
            (&self.second_source, &self.second_revision)
        } else {
            (&self.source, &self.revision)
        };
        let quote = b"Relay uses Cedar.";
        let record = CanonicalRecord::from_value(serde_json::json!({
            "wiki_schema": "1",
            "wiki_id": format!("evidence_{key}_{suffix}"),
            "wiki_kind": "evidence",
            "title": format!("Evidence {key} {suffix}"),
            "wiki_status": "active",
            "wiki_assertion_id": format!("assertion_{key}"),
            "wiki_source_id": source,
            "wiki_source_revision": revision,
            "wiki_stance": stance,
            "wiki_locator_kind": "utf8-bytes",
            "wiki_span_start": 0,
            "wiki_span_end": quote.len(),
            "wiki_quote_hash": Blake3Hash::digest(quote),
        }))
        .unwrap();
        let body = crate::sources::evidence::exact_quote_body(
            quote,
            "\n",
            "Fixture mechanics: quotation integrity, not semantic entailment.",
        )
        .unwrap();
        let bytes = crate::sources::revision::record_bytes(record, &body).unwrap();
        let parsed = crate::records::parse_note(&bytes);
        assert!(
            parsed.canonical.is_some(),
            "invalid canonical evidence fixture {key}/{suffix}: {:?}",
            parsed.diagnostics
        );
        fs::write(self.root.join(format!("evidence-{key}-{suffix}.md")), bytes).unwrap();
    }
    pub(crate) fn rebuild(&self) {
        let writer =
            WriterPermit::acquire(self.catalog.fs().root(), Duration::from_secs(1)).unwrap();
        self.catalog.rebuild_normalized(&writer).unwrap();
    }
    pub(crate) fn query(&self, plan: &GraphPlan, verified: bool) -> GraphResult {
        neighbors(
            &self.catalog,
            &id("entity_relay"),
            plan,
            &VerificationBudget::default(),
            verified,
        )
        .unwrap()
    }
}
fn plan() -> GraphPlan {
    GraphPlan::default()
}
#[test]
fn qualified_directional_literal_neighbors_have_selected_citations_and_identity_only() {
    let f = Fixture::new();
    let out = f.query(&plan(), true);
    assert!(matches!(
        out.verification,
        SnapshotVerification::IndexedEvidence {
            global_membership_verified: false,
            ..
        }
    ));
    assert!(!out.network_used);
    assert!(out.navigation.is_empty());
    assert_eq!(
        out.assertions
            .iter()
            .map(|a| a.record_ref.record_id.as_str())
            .collect::<Vec<_>>(),
        vec!["assertion_a", "assertion_c", "assertion_d", "assertion_e"]
    );
    let incoming = out
        .assertions
        .iter()
        .find(|a| a.record_ref.record_id == id("assertion_c"))
        .unwrap();
    assert_eq!(incoming.path[0].traversal, TraversalDirection::Incoming);
    let literal = out
        .assertions
        .iter()
        .find(|a| a.record_ref.record_id == id("assertion_d"))
        .unwrap();
    assert_eq!(
        literal.object,
        GraphObject::Literal {
            literal_type: "decimal".into(),
            value: "17".into()
        }
    );
    assert_eq!(literal.qualifiers.unit.as_deref(), Some("ms"));
    assert_eq!(literal.qualifiers.modality, "possible");
    let main = out
        .assertions
        .iter()
        .find(|a| a.record_ref.record_id == id("assertion_a"))
        .unwrap();
    assert!(main.disputed);
    assert_eq!(main.opposing_assertions.len(), 1);
    assert_eq!(main.contradictions.len(), 1);
    let view = SourceStore::new(f.catalog.fs().clone());
    let view = view.view().unwrap();
    for assertion in &out.assertions {
        for evidence in assertion.support.iter().chain(&assertion.contradictions) {
            let verified = view
                .verify(
                    evidence.citation.as_ref().unwrap(),
                    crate::sources::CitationScope::Current,
                )
                .unwrap();
            assert_eq!(verified.quote, b"Relay uses Cedar.");
            assert_eq!(
                Blake3Hash::digest(&verified.quote),
                evidence.source.quote_hash
            );
        }
    }
    let root = out
        .entities
        .iter()
        .find(|e| e.record_ref.record_id == id("entity_relay"))
        .unwrap();
    assert_eq!(root.identity_eligibility, Some(Eligibility::Current));
    assert_eq!(root.description_eligibility, Some(Eligibility::Unsupported));
    assert!(root.description.is_none());
}
#[test]
fn depth_zero_two_and_cycles_obey_hop_then_id_order() {
    let f = Fixture::new();
    let mut request = plan();
    request.limits.depth = 0;
    let zero = f.query(&request, true);
    assert!(zero.assertions.is_empty());
    assert_eq!(zero.entities.len(), 1);
    request.limits.depth = 2;
    let two = f.query(&request, true);
    assert_eq!(two.assertions.len(), 5);
    let last = two.assertions.last().unwrap();
    assert_eq!(last.record_ref.record_id, id("assertion_b"));
    assert_eq!(last.hop, 2);
    assert_eq!(last.path.len(), 2);
    let ids = two
        .assertions
        .iter()
        .map(|a| a.record_ref.record_id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), 5);
}
#[test]
fn hub_33_returns_partial_and_lowered_global_caps_bound_selection() {
    let f = Fixture::new();
    for n in 0..33 {
        f.edge(&format!("hub{n:02}"), "relay", None, "has_property", false);
    }
    f.rebuild();
    let mut request = plan();
    request.limits.hits = 50;
    let out = f.query(&request, true);
    assert_eq!(out.assertions.len(), 16);
    assert_eq!(out.coverage.assertion_candidates, 16);
    assert!(out.truncated);
    assert!(out.coverage.omissions_are_lower_bounds);
    assert!(out.coverage.omitted_incident_assertions >= 1);
    request.limits.candidates = 1;
    request.limits.assertions = 1;
    request.limits.depth = 2;
    let one = f.query(&request, true);
    assert_eq!(one.assertions.len(), 1);
    assert_eq!(one.coverage.assertion_candidates, 1);
    assert!(one.coverage.omissions_are_lower_bounds);
}
#[test]
fn source_filter_uses_support_not_contrary_and_counts_hidden_membership() {
    let f = Fixture::new();
    let mut request = plan();
    request.filters.source_ids = vec![f.second_source.clone()];
    let out = f.query(&request, true);
    assert_eq!(out.assertions.len(), 1);
    let assertion = &out.assertions[0];
    assert_eq!(assertion.record_ref.record_id, id("assertion_a"));
    assert_eq!(assertion.support.len(), 1);
    assert_eq!(assertion.support[0].source.source_id, f.second_source);
    assert_eq!(assertion.omitted_support, 1);
    assert_eq!(assertion.omitted_contradictions, 1);
    let raw = fs::read_to_string(f.root.join("evidence-a-second.md")).unwrap();
    assert!(raw.contains("wiki_stance: \"supports\""));
    fs::write(
        f.root.join("evidence-a-second.md"),
        raw.replace("wiki_stance: \"supports\"", "wiki_stance: \"contradicts\""),
    )
    .unwrap();
    f.rebuild();
    assert!(f.query(&request, true).assertions.is_empty());
}
#[test]
fn display_limits_do_not_reduce_proof_membership() {
    let f = Fixture::new();
    for n in 0..3 {
        f.evidence("a", &format!("more{n}"), "supports", false);
    }
    for n in 0..2 {
        f.evidence("a", &format!("counter{n}"), "contradicts", false);
    }
    f.rebuild();
    let out = f.query(&plan(), true);
    let a = out
        .assertions
        .iter()
        .find(|a| a.record_ref.record_id == id("assertion_a"))
        .unwrap();
    assert_eq!(a.support.len(), 2);
    assert_eq!(a.omitted_support, 3);
    assert_eq!(a.contradictions.len(), 1);
    assert_eq!(a.omitted_contradictions, 2);
    let raw = fs::read_to_string(f.root.join("evidence-a-more2.md")).unwrap();
    fs::write(
        f.root.join("evidence-a-more2.md"),
        raw.replace("Fixture mechanics", "Changed mechanics"),
    )
    .unwrap();
    assert_eq!(
        neighbors(
            &f.catalog,
            &id("entity_relay"),
            &plan(),
            &VerificationBudget::default(),
            true
        )
        .unwrap_err()
        .code,
        ErrorCode::FreshnessConflict
    );
}
#[test]
fn cached_neighbors_remain_uncited_after_selected_edit() {
    let f = Fixture::new();
    let raw = fs::read_to_string(f.root.join("assertion-a.md")).unwrap();
    fs::write(
        f.root.join("assertion-a.md"),
        raw.replace("Recorded relation.", "External relation."),
    )
    .unwrap();
    let cached = f.query(&plan(), false);
    assert_eq!(cached.verification, SnapshotVerification::IndexSnapshot);
    assert!(
        cached
            .assertions
            .iter()
            .flat_map(|a| a.support.iter().chain(&a.contradictions))
            .all(|e| e.citation.is_none())
    );
    assert_eq!(
        neighbors(
            &f.catalog,
            &id("entity_relay"),
            &plan(),
            &VerificationBudget::default(),
            true
        )
        .unwrap_err()
        .code,
        ErrorCode::FreshnessConflict
    );
}
#[test]
fn final_cut_is_reached_and_selected_same_size_change_refuses() {
    let f = Fixture::new();
    let reached = Arc::new(AtomicBool::new(false));
    let mark = reached.clone();
    let result = neighbors_with_final_check(
        &f.catalog,
        &id("entity_relay"),
        &plan(),
        &VerificationBudget::default(),
        true,
        || {
            mark.store(true, Ordering::SeqCst);
            let raw = fs::read_to_string(f.root.join("assertion-a.md")).unwrap();
            let edited = raw.replace("Recorded relation.", "External relation.");
            assert_eq!(raw.len(), edited.len());
            fs::write(f.root.join("assertion-a.md"), edited).unwrap();
            Ok(())
        },
    );
    assert!(reached.load(Ordering::SeqCst));
    assert_eq!(result.unwrap_err().code, ErrorCode::FreshnessConflict);
}
#[test]
fn final_cut_allows_unrelated_edit_and_held_publication_rebuild() {
    let f = Fixture::new();
    let reached = AtomicBool::new(false);
    let out = neighbors_with_final_check(
        &f.catalog,
        &id("entity_relay"),
        &plan(),
        &VerificationBudget::default(),
        true,
        || {
            reached.store(true, Ordering::SeqCst);
            fs::write(f.root.join("unrelated.md"), "Unrelated new text").unwrap();
            f.rebuild();
            Ok(())
        },
    )
    .unwrap();
    assert!(reached.load(Ordering::SeqCst));
    assert!(!out.assertions.is_empty());
    assert_eq!(out.assertions, f.query(&plan(), true).assertions);
}
#[test]
fn root_authenticates_even_when_no_assertions_and_operation_guard_runs_at_final_cut() {
    let f = Fixture::new();
    let mut request = plan();
    request.limits.depth = 0;
    let error = neighbors_with_final_check(
        &f.catalog,
        &id("entity_relay"),
        &request,
        &VerificationBudget::default(),
        true,
        || {
            let raw = fs::read_to_string(f.root.join("relay.md")).unwrap();
            fs::write(
                f.root.join("relay.md"),
                raw.replace("title: relay", "title: Relay"),
            )
            .unwrap();
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::FreshnessConflict);
    let f = Fixture::new();
    let error = neighbors_with_final_check(
        &f.catalog,
        &id("entity_relay"),
        &request,
        &VerificationBudget::default(),
        false,
        || {
            fs::remove_file(f.root.join(".wiki/state/operations.json")).unwrap();
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryRequired);
}
#[test]
fn unsupported_options_and_ambiguous_or_absent_root_refuse_without_scan() {
    let f = Fixture::new();
    let mut request = plan();
    request.filters.include_historical = true;
    assert_eq!(
        neighbors(
            &f.catalog,
            &id("entity_relay"),
            &request,
            &VerificationBudget::default(),
            true
        )
        .unwrap_err()
        .code,
        ErrorCode::CapabilityUnavailable
    );
    assert_eq!(
        neighbors(
            &f.catalog,
            &id("entity_absent"),
            &plan(),
            &VerificationBudget::default(),
            true
        )
        .unwrap_err()
        .code,
        ErrorCode::RecordNotFound
    );
    fs::copy(f.root.join("relay.md"), f.root.join("duplicate.md")).unwrap();
    f.rebuild();
    assert_eq!(
        neighbors(
            &f.catalog,
            &id("entity_relay"),
            &plan(),
            &VerificationBudget::default(),
            true
        )
        .unwrap_err()
        .code,
        ErrorCode::ReferenceAmbiguous
    );
}
#[test]
fn proof_limits_refuse_instead_of_silently_dropping_support() {
    let f = Fixture::new();
    let budget = VerificationBudget {
        max_files: 2,
        ..Default::default()
    };
    assert_eq!(
        neighbors(&f.catalog, &id("entity_relay"), &plan(), &budget, true)
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
}

#[test]
fn self_loop_endpoint_streams_deduplicate_and_lowered_assertion_cap_stays_global() {
    let f = Fixture::new();
    f.edge("0self", "relay", Some("relay"), "uses", false);
    f.rebuild();
    let mut request = plan();
    request.limits.depth = 2;
    request.limits.candidates = 1;
    request.limits.assertions = 1;
    let out = f.query(&request, true);
    assert_eq!(out.assertions.len(), 1);
    assert_eq!(
        out.assertions[0].record_ref.record_id,
        id("assertion_0self")
    );
    assert_eq!(out.coverage.assertion_candidates, 1);
    assert!(out.coverage.omissions_are_lower_bounds);
}

#[test]
fn filters_scope_assertions_before_caps_without_excluding_exact_identity() {
    let f = Fixture::new();
    let mut request = plan();
    request.limits.incident_per_seed = 1;
    request.filters.path_prefix = Some("assertion-d".into());
    let out = f.query(&request, true);
    assert_eq!(out.assertions.len(), 1);
    assert_eq!(out.assertions[0].record_ref.record_id, id("assertion_d"));
    assert_eq!(out.entities.len(), 1);
    assert_eq!(out.entities[0].record_ref.record_id, id("entity_relay"));
    request.filters.path_prefix = None;
    request.filters.authored_statuses = vec!["proposed".into()];
    assert!(f.query(&request, true).assertions.is_empty());
}

#[test]
fn repeated_depth_two_sentinels_do_not_invent_multiple_unique_omissions() {
    let f = Fixture::new();
    for entry in fs::read_dir(&f.root).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_str().unwrap();
        if name.starts_with("assertion-") || name.starts_with("evidence-") {
            fs::remove_file(entry.path()).unwrap();
        }
    }
    // Each endpoint has the same17 unique assertions; each indexed probe returns
    // the same16 IDs plus the same sentinel. There is only one unique omission.
    for n in 0..17 {
        f.edge(
            &format!("parallel{n:02}"),
            "relay",
            Some("cedar"),
            "uses",
            false,
        );
    }
    f.rebuild();
    let mut request = plan();
    request.limits.depth = 2;
    request.limits.hits = 50;
    let out = f.query(&request, true);
    assert_eq!(out.assertions.len(), 16);
    assert_eq!(out.coverage.assertion_candidates, 16);
    assert_eq!(out.coverage.visited_entities, 2);
    assert_eq!(out.coverage.omitted_incident_assertions, 0);
    assert!(out.coverage.omissions_are_lower_bounds);
    assert!(out.truncated);
}
