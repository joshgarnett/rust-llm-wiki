fn auto_prepare(fixture: &Fixture, query: &str, request: &ContextRequest) -> ContextResult {
    fixture
        .run(
            query,
            request,
            SelectionAction::PrepareOriginalsAuto {
                max_input_bytes: packet::MAX_ORIGINAL_INPUT_BYTES,
            },
        )
        .unwrap()
}

fn auto_reply(prepared: &ContextResult, spans: &[(usize, u64, u64)]) -> OriginalSelectionReply {
    let mut reply = Fixture::reply(prepared, spans);
    reply.version = OriginalSelectionVersion::V2;
    reply
}

fn auto_apply(
    fixture: &Fixture,
    query: &str,
    request: &ContextRequest,
    reply: OriginalSelectionReply,
) -> Result<ContextResult> {
    fixture.run(
        query,
        request,
        SelectionAction::ApplyOriginalsAuto {
            max_input_bytes: packet::MAX_ORIGINAL_INPUT_BYTES,
            reply,
        },
    )
}

#[test]
fn automatic_originals_preserve_lexical_order_and_distinct_equal_content_provenance() {
    let text = "Needle café requires a written permit.\n";
    let fixture = Fixture::new(&[text, text]);
    let authored = CanonicalRecord::new(BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_id".into(), json!("page_auto_distractor")),
        ("wiki_kind".into(), json!("page")),
        ("wiki_status".into(), json!("reviewed")),
        ("title".into(), json!("Needle")),
    ]))
    .unwrap();
    fs::write(
        fixture.catalog.fs().root().path().join("authored.md"),
        record_bytes(
            authored,
            b"Needle authored distractor must not become an original.\n",
        )
        .unwrap(),
    )
    .unwrap();
    fixture.sync();
    let request = fixture.request();
    let reader = fixture
        .catalog
        .cached_query_snapshot(Default::default())
        .unwrap();
    let discovered =
        super::super::lexical::search_context_catalog(&reader, "Needle", &request.documents, false)
            .unwrap();
    drop(reader);
    assert!(discovered.hits.iter().any(|hit| hit.source_id.is_none()));
    let prepared = auto_prepare(&fixture, "Needle", &request);
    let packet = prepared.selection_packet().unwrap();
    assert_eq!(packet.originals.len(), 2);
    assert_eq!(
        packet
            .originals
            .iter()
            .map(|original| &original.locator.path)
            .collect::<Vec<_>>(),
        discovered
            .hits
            .iter()
            .filter(|hit| hit.source_id.is_some())
            .map(|hit| &hit.locator.path)
            .collect::<Vec<_>>()
    );
    assert_ne!(packet.originals[0].source_id, packet.originals[1].source_id);
    assert_eq!(packet.originals[0].text, packet.originals[1].text);
    assert_eq!(packet.input_bytes, packet.selector_input.len());
    assert!(packet.input_bytes <= packet::MAX_ORIGINAL_INPUT_BYTES);
    assert!(
        prepared
            .omissions()
            .iter()
            .any(
                |omission| omission.path.as_ref() == Some(&path("authored.md"))
                    && omission.reason == "original_discovery_not_captured"
            )
    );
    let task: serde_json::Value = serde_json::from_str(&packet.selector_input).unwrap();
    assert_eq!(
        task["payload"]["version"],
        "lwiki.context-original-selection.v2"
    );
    assert_eq!(
        task["payload"]["admission"]["authenticated_paths"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let reply = auto_reply(&prepared, &[(1, 0, text.len() as u64)]);
    let selected = auto_apply(&fixture, "Needle", &request, reply).unwrap();
    assert_eq!(selected.passages().len(), 1);
    assert_eq!(selected.passages()[0].text, text);
    assert_eq!(selected.passages()[0].locator, packet.originals[1].locator);
    assert!(
        matches!(&selected.passages()[0].citations[0], CitationRef::Source(reference)
        if reference.source_id == packet.originals[1].source_id
        && reference.source_revision == packet.originals[1].source_revision
        && reference.quote_hash == Blake3Hash::digest(text))
    );
}

#[test]
fn automatic_oversized_owner_omits_without_reading_its_canonical_body_and_empty_is_bounded() {
    let text = format!("Needle {}", "x".repeat(packet::MAX_ORIGINAL_BYTES));
    let fixture = Fixture::new(&[&text]);
    // A skipped owner's cached metadata is not proof of its canonical bytes.
    // Removing the skipped body must not force a hidden whole-body proof.
    fs::remove_file(
        fixture
            .catalog
            .fs()
            .root()
            .path()
            .join(fixture.paths[0].as_str()),
    )
    .unwrap();
    let request = fixture.request();
    let prepared = auto_prepare(&fixture, "Needle", &request);
    let packet = prepared.selection_packet().unwrap();
    assert!(packet.originals.is_empty());
    assert!(!prepared.truncated());
    assert!(
        prepared
            .omissions()
            .iter()
            .any(|omission| omission.path.as_ref() == Some(&fixture.paths[0])
                && omission.reason == "original_admission_raw_byte_limit")
    );
    let task: serde_json::Value = serde_json::from_str(&packet.selector_input).unwrap();
    assert!(
        task["payload"]["admission"]["authenticated_paths"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let selected = auto_apply(&fixture, "Needle", &request, auto_reply(&prepared, &[])).unwrap();
    assert!(selected.passages().is_empty());
    assert!(
        selected
            .warnings()
            .iter()
            .any(|warning| warning.contains("bounded task"))
    );
    assert!(!selected.omissions().is_empty());
}

#[test]
fn automatic_task_serialization_omits_whole_newline_heavy_owner_and_discloses_proof_work() {
    let large = "Needle\n".repeat(12_000);
    let small = "Needle short qualifying fact.\n";
    let fixture = Fixture::new(&[&large, small]);
    let request = fixture.request();
    let prepared = auto_prepare(&fixture, "Needle", &request);
    let packet = prepared.selection_packet().unwrap();
    assert_eq!(packet.originals.len(), 1);
    assert_eq!(packet.originals[0].text, small);
    assert!(packet.input_bytes <= packet::MAX_ORIGINAL_INPUT_BYTES);
    assert!(
        prepared
            .omissions()
            .iter()
            .any(|omission| omission.path.as_ref() == Some(&fixture.paths[0])
                && omission.reason == "original_admission_serialized_task_limit")
    );
    let task: serde_json::Value = serde_json::from_str(&packet.selector_input).unwrap();
    assert_eq!(
        task["payload"]["admission"]["authenticated_paths"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(!packet.selector_input.contains(&large));
    assert!(prepared.usage().verification_bytes >= large.len());
    let selected = auto_apply(
        &fixture,
        "Needle",
        &request,
        auto_reply(&prepared, &[(0, 0, small.len() as u64)]),
    )
    .unwrap();
    assert_eq!(selected.passages()[0].text, small);
    assert!(
        selected
            .omissions()
            .iter()
            .any(|omission| omission.reason == "original_admission_serialized_task_limit")
    );
}

#[test]
fn automatic_discovery_limits_omissions_and_reply_modes_are_bound_without_transport_truncation() {
    let fixture = Fixture::new(&["Needle first fact.\n", "Needle second fact.\n"]);
    let mut request = fixture.request();
    request.documents.limits.hits = 1;
    let prepared = auto_prepare(&fixture, "Needle", &request);
    assert_eq!(prepared.selection_packet().unwrap().originals.len(), 1);
    assert!(!prepared.truncated());
    assert!(prepared.omissions().iter().any(|omission|
        omission.reason == "original_discovery_bounded_candidates_not_complete"));
    let reply = auto_reply(&prepared, &[]);
    let mut changed = request.clone();
    changed.documents.limits.hits = 2;
    assert_eq!(
        auto_apply(&fixture, "Needle", &changed, reply.clone())
            .unwrap_err()
            .code,
        ErrorCode::FreshnessConflict
    );
    let mut wrong_version = reply;
    wrong_version.version = OriginalSelectionVersion::V1;
    assert_eq!(
        auto_apply(&fixture, "Needle", &request, wrong_version)
            .unwrap_err()
            .code,
        ErrorCode::FreshnessConflict
    );
    let explicit = fixture.prepare(&request);
    let mut wrong_version = Fixture::reply(&explicit, &[]);
    wrong_version.version = OriginalSelectionVersion::V2;
    assert_eq!(
        fixture.apply(&request, wrong_version).unwrap_err().code,
        ErrorCode::FreshnessConflict
    );
    for (hits, candidates) in [(11, 80), (10, 81)] {
        let mut over = fixture.request();
        over.documents.limits.hits = hits;
        over.documents.limits.candidates = candidates;
        assert_eq!(
            fixture
                .run(
                    "Needle",
                    &over,
                    SelectionAction::PrepareOriginalsAuto {
                        max_input_bytes: packet::MAX_ORIGINAL_INPUT_BYTES,
                    }
                )
                .unwrap_err()
                .code,
            ErrorCode::Usage
        );
    }
    assert_eq!(
        fixture
            .run(
                "Needle",
                &request,
                SelectionAction::PrepareOriginalsAuto {
                    max_input_bytes: 10,
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
}

#[test]
fn automatic_refresh_withdrawal_and_historical_inclusion_invalidate_prior_reply() {
    let fixture = Fixture::new(&["Needle earlier permit.\n"]);
    let request = fixture.request();
    let prepared = auto_prepare(&fixture, "Needle", &request);
    let old_reply = auto_reply(&prepared, &[(0, 0, 6)]);
    let refresh = SourceStore::new(fixture.catalog.fs().clone())
        .plan_refresh(&fixture.source, capture("Needle current amber permit.\n"))
        .unwrap();
    seed(fixture.catalog.fs(), refresh.draft.unwrap());
    fixture.sync();
    assert_eq!(
        auto_apply(&fixture, "Needle", &request, old_reply)
            .unwrap_err()
            .code,
        ErrorCode::FreshnessConflict
    );
    let current = auto_prepare(&fixture, "Needle", &request);
    assert_eq!(current.selection_packet().unwrap().originals.len(), 1);
    assert!(
        current.selection_packet().unwrap().originals[0]
            .text
            .contains("current amber")
    );
    let current_reply = auto_reply(&current, &[]);
    let withdrawal = SourceStore::new(fixture.catalog.fs().clone())
        .plan_withdraw(&fixture.source, "Controlled withdrawal")
        .unwrap();
    seed(fixture.catalog.fs(), withdrawal.draft.unwrap());
    fixture.sync();
    assert_eq!(
        auto_apply(&fixture, "Needle", &request, current_reply)
            .unwrap_err()
            .code,
        ErrorCode::FreshnessConflict
    );
    let excluded = auto_prepare(&fixture, "Needle", &request);
    assert!(excluded.selection_packet().unwrap().originals.is_empty());
    let mut history = request;
    history.documents.filters.include_historical = true;
    let retained = auto_prepare(&fixture, "Needle", &history);
    assert!(!retained.selection_packet().unwrap().originals.is_empty());
    assert!(
        retained
            .selection_packet()
            .unwrap()
            .originals
            .iter()
            .all(|original| matches!(
                original.eligibility,
                Eligibility::Historical | Eligibility::Withdrawn
            ))
    );
}

#[test]
fn automatic_discovery_and_authentication_share_cumulative_catalog_and_canonical_budgets() {
    let fixture = Fixture::new(&["Needle authentic fact.\n"]);
    let request = fixture.request();
    let probe = fixture
        .catalog
        .cached_query_snapshot(Default::default())
        .unwrap();
    super::super::lexical::search_context_catalog(&probe, "Needle", &request.documents, false)
        .unwrap();
    let discovery_rows = probe.usage().rows;
    drop(probe);
    let reader = fixture
        .catalog
        .cached_query_snapshot(crate::catalog::query_types::QueryReadLimits {
            max_rows: discovery_rows,
            ..Default::default()
        })
        .unwrap();
    let mut meter = Meter::new(&request.verification_budget);
    let error = context(
        &fixture.catalog,
        &reader,
        &mut meter,
        "Needle",
        &request,
        &ContextOptions {
            selection: SelectionAction::PrepareOriginalsAuto {
                max_input_bytes: packet::MAX_ORIGINAL_INPUT_BYTES,
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    let mut limited = request;
    limited.verification_budget.max_bytes = 1;
    assert_eq!(
        fixture
            .run(
                "Needle",
                &limited,
                SelectionAction::PrepareOriginalsAuto {
                    max_input_bytes: packet::MAX_ORIGINAL_INPUT_BYTES,
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::BudgetExceeded
    );
}
