//! Public app workflows for compact, incremental preparation on disposable vaults.
use super::*;
use crate::{
    app::indexed_embedding_inputs::attribution,
    catalog::query_types::{QueryCatalog, QueryReadLimits},
    retrieval::unit_inventory_types::{RenderPolicyId, UnitDescriptor, UnitOwnerBinding},
};
use std::{collections::BTreeMap, fs, sync::atomic::AtomicUsize};

// Prospective work ceilings from the required safety pipeline, fixed before
// execution: page selection, task guard freeze and final acknowledgment each
// authenticate all changed owners; pre-send, receipt and post-receipt rebind
// each authenticate the owners of the missing paid input. Received recovery
// skips guard freezing and send, then reuses the cached page proof for its ack.
const ONLINE_CHANGED_OWNER_PASSES: usize = 3;
const ONLINE_PAID_OWNER_PASSES: usize = 3;
const RECEIVED_SINGLE_OWNER_PASSES: usize = 3;

fn ordinary_settings() -> EmbeddingSettings {
    let settings = EmbeddingSettings::default();
    assert_eq!(settings.max_input_bytes, 12_000);
    assert_eq!(settings.quality_target_bytes, None);
    settings
}
fn rel(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn page_bytes(id: &str, dependency: Option<&str>, body: &str) -> Vec<u8> {
    let dependency =
        dependency.map_or(String::new(), |id| format!("wiki_depends_on_ids: [{id}]\n"));
    format!("---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: {id}\ntitle: Inventory fixture\nwiki_status: reviewed\n{dependency}---\n{body}").into_bytes()
}
fn owner_state(
    fixture: &Fixture,
    settings: &EmbeddingSettings,
    path: &VaultRelativePath,
) -> (UnitOwnerBinding, Vec<UnitDescriptor>) {
    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let policy =
        RenderPolicyId::for_settings(&reader.snapshot().parser_fingerprint, settings).unwrap();
    let inventory = reader.unit_inventory_state(&policy).unwrap().unwrap();
    assert!(inventory.complete);
    let binding = reader.unit_owner_binding(&policy, path).unwrap().unwrap();
    let descriptors = reader
        .unit_descriptors_for_owner(&policy, path, 4096)
        .unwrap();
    (binding, descriptors)
}
fn cached_noop(fixture: &Fixture, settings: &EmbeddingSettings, eligible: usize) {
    let (result, work) =
        attribution::with_observation(|| fixture.offline().embeddings_sync_cached(settings));
    let report = result.unwrap();
    complete(&report, eligible);
    assert!(!report.network_used);
    assert!(report.run_id.is_none());
    assert_eq!(report.generated_inputs, 0);
    assert_eq!(work.owner_attempts, 0);
    assert_eq!(work.authenticated_owners, 0);
    assert_eq!(work.rendered_bytes, 0);
    assert_eq!(work.rendered_units, 0);
}

struct ThirdReceivedCut {
    received: AtomicUsize,
}
impl LedgerFault for ThirdReceivedCut {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        if point == LedgerCheckpoint::AfterReceived
            && self.received.fetch_add(1, Ordering::SeqCst) == 2
        {
            Err(WikiError::new(
                ErrorCode::Internal,
                "stop after the third retained response",
            ))
        } else {
            Ok(())
        }
    }
}

#[test]
fn inventory_multiple_pages_received_resume_skips_ready_owners_and_noop_renders_nothing() {
    let fixture = normalized();
    for index in 0..129 {
        let path = fixture
            .fs
            .root()
            .path()
            .join(format!("inventory-{index:03}.md"));
        let body = if index == 128 {
            "Resumeprobe final owner has 29 violet tokens. café 東京 🦀.\n"
        } else {
            "Resumeprobe shared first-page fact has 17 amber tokens. café 東京 🦀.\n"
        };
        fs::write(
            path,
            page_bytes(&format!("page_inventory_{index:03}"), None, body),
        )
        .unwrap();
    }
    fixture.app.index_sync(false).unwrap();
    let settings = ordinary_settings();
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let preflight = runtime(&fixture.service, &dispatch);
    let too_many_guards = fixture
        .app
        .embeddings_sync(&settings, &preflight)
        .unwrap_err();
    assert_eq!(
        too_many_guards.code,
        ErrorCode::BudgetExceeded,
        "{too_many_guards:?}"
    );
    assert_eq!(
        responses.calls.load(Ordering::SeqCst),
        0,
        "the 128-owner shared-input guard union must refuse before dispatch"
    );
    for index in 64..128 {
        fs::write(
            fixture
                .fs
                .root()
                .path()
                .join(format!("inventory-{index:03}.md")),
            page_bytes(
                &format!("page_inventory_{index:03}"),
                None,
                "Resumeprobe second shared first-page fact has 23 green tokens. café 東京 🦀.\n",
            ),
        )
        .unwrap();
    }
    fixture.app.index_sync(false).unwrap();
    let fault = Arc::new(ThirdReceivedCut {
        received: AtomicUsize::new(0),
    });
    let mut interrupted = runtime(&fixture.service, &dispatch);
    interrupted.job_options.fault = Some(fault.clone());
    // The first 128-owner preparation page contains two shared inputs, each
    // with 64 owner guards plus WIKI.md, inside the 128-guard receipt limit.
    // A third distinct input belongs to the final owner on the next page.
    // Each attempt retains its real 8 MiB
    // reservation if accounting is unknown. No finite response ceiling is set.
    assert_eq!(interrupted.limits.response_bytes, None);
    let stopped = fixture
        .app
        .embeddings_sync(&settings, &interrupted)
        .unwrap_err();
    assert_eq!(stopped.code, ErrorCode::Internal, "{stopped:?}");
    assert_eq!(fault.received.load(Ordering::SeqCst), 3);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
    let run: RecordId = serde_json::from_value(stopped.details["run_id"].clone()).unwrap();
    let job = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        run,
        options(),
    )
    .unwrap();
    let before = job.inspect().unwrap();
    assert_eq!(before.attempts.len(), 1);
    assert_eq!(before.attempts[0].phase, AttemptPhase::Received);
    assert!(before.attempts[0].spool.is_some());
    assert_eq!(before.budget.dispatched_requests, 1);
    assert_eq!(before.budget.outstanding.requests, 1);

    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let policy =
        RenderPolicyId::for_settings(&reader.snapshot().parser_fingerprint, &settings).unwrap();
    let store = VectorStore::open(&fixture.fs, None).unwrap();
    let space = fixture.spec().id().unwrap();
    let mut retained = Vec::new();
    for index in 0..128 {
        let path = rel(&format!("inventory-{index:03}.md"));
        let binding = reader.unit_owner_binding(&policy, &path).unwrap().unwrap();
        assert!(store.owner_binding_ready(&space, &binding).unwrap());
        retained.push(binding);
    }
    let final_binding = reader
        .unit_owner_binding(&policy, &rel("inventory-128.md"))
        .unwrap()
        .unwrap();
    assert!(!store.owner_binding_ready(&space, &final_binding).unwrap());
    drop(store);
    drop(reader);

    let resumed = runtime(&fixture.service, &dispatch);
    let (result, work) =
        attribution::with_observation(|| fixture.app.embeddings_sync(&settings, &resumed));
    complete(&result.unwrap(), 129);
    assert_eq!(
        responses.calls.load(Ordering::SeqCst),
        3,
        "Received response must not be resent"
    );
    assert!(
        work.owner_attempts > 0 && work.owner_attempts <= RECEIVED_SINGLE_OWNER_PASSES,
        "the first 128 ready owners must not be reauthenticated: {}",
        work.owner_attempts
    );
    let after = job.inspect().unwrap();
    assert_eq!(after.attempts.len(), 1);
    assert_eq!(after.attempts[0].phase, AttemptPhase::Settled);
    assert_eq!(after.budget.dispatched_requests, 1);
    assert_eq!(paid_inputs(&fixture, &after).len(), 1);
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    for binding in retained {
        assert_eq!(
            reader.unit_owner_binding(&policy, &binding.owner).unwrap(),
            Some(binding),
            "resuming the final owner must retain each ready owner's exact binding"
        );
    }
    drop(reader);
    cached_noop(&fixture, &settings, 129);

    fixture.query_seed(&fixture.spec(), "Resumeprobe", vec![1.0, 0.0]);
    let last = fixture
        .offline()
        .semantic_search_selected(
            "Resumeprobe",
            &QueryPlan {
                mode: SearchMode::Semantic,
                filters: SearchFilters {
                    path_prefix: Some("inventory-128.md".into()),
                    ..Default::default()
                },
                limits: SearchLimits {
                    hits: 1,
                    candidates: 1,
                    excerpt_bytes: 256,
                },
                cursor: None,
            },
            None,
            false,
        )
        .unwrap();
    assert_eq!(last.hits.len(), 1);
    assert_eq!(last.hits[0].locator.path, rel("inventory-128.md"));
    assert!(last.hits[0].excerpt.text.contains("29 violet tokens"));
    assert!(
        last.warnings
            .iter()
            .any(|warning| warning.contains("rendered 0 units / 0 UTF-8 bytes"))
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
}

#[test]
fn inventory_immutable_refresh_exposes_missing_new_fact_and_preserves_unrelated_ready_owner() {
    let fixture = normalized();
    let settings = ordinary_settings();
    let (changing, old_revision) = add(&fixture, TITLE, "inventory-changing.txt", FIRST);
    let (stable, stable_revision) = add(
        &fixture,
        "Stable inventory title",
        "inventory-stable.txt",
        "Signalneedle unchanged source retains 41 green tokens.\n",
    );
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    let initial = fixture.app.embeddings_sync(&settings, &runtime).unwrap();
    complete(&initial, 2);
    let stable_before = owner_state(
        &fixture,
        &settings,
        &content_path(&stable, &stable_revision),
    );
    let inputs = paid_inputs(&fixture, &ledger(&fixture, &initial).inspect().unwrap());
    let old_input = inputs
        .iter()
        .find(|input| input.utf8.contains("17 amber tokens"))
        .unwrap();
    let space = initial.space.unwrap();
    fixture.query_seed(&fixture.spec(), "Signalneedle", vec![1.0, 0.0]);
    cached_noop(&fixture, &settings, 2);

    let changed = fixture
        .app
        .source_refresh(
            changing.clone(),
            capture(TITLE, "inventory-changing.txt", SECOND),
        )
        .unwrap();
    let revision = changed.allocated_ids["revision"].clone();
    assert_ne!(revision, old_revision);
    assert_eq!(
        owner_state(
            &fixture,
            &settings,
            &content_path(&stable, &stable_revision)
        ),
        stable_before
    );
    let (missing, work) =
        attribution::with_observation(|| fixture.offline().embeddings_sync_cached(&settings));
    let missing = missing.unwrap();
    assert!(!missing.network_used);
    assert_eq!(missing.coverage.eligible_units, 2);
    assert_eq!(missing.coverage.available_units, 1);
    assert_eq!(missing.coverage.missing_units, 1);
    assert_eq!(work.owner_attempts, 1);
    let request = plan(SearchMode::Semantic, vec![changing.clone()], 2);
    let before = fixture
        .offline()
        .semantic_search_selected("Signalneedle", &request, None, false)
        .unwrap();
    assert!(
        before.hits.is_empty(),
        "old vector cannot supply the new immutable fact"
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);

    let (refreshed, work) =
        attribution::with_observation(|| fixture.app.embeddings_sync(&settings, &runtime));
    let refreshed = refreshed.unwrap();
    complete(&refreshed, 2);
    assert_eq!(refreshed.generated_inputs, 1);
    assert!(
        work.owner_attempts > 0
            && work.owner_attempts <= ONLINE_CHANGED_OWNER_PASSES + ONLINE_PAID_OWNER_PASSES,
        "required repeats may authenticate the changed Source only: {} attempts",
        work.owner_attempts
    );
    assert_eq!(
        owner_state(
            &fixture,
            &settings,
            &content_path(&stable, &stable_revision)
        ),
        stable_before,
        "unrelated Source binding and descriptors must survive paid publication unchanged"
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
    for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
        let hits = fixture
            .offline()
            .semantic_search_selected(
                "Signalneedle",
                &plan(mode, vec![changing.clone()], 2),
                None,
                false,
            )
            .unwrap();
        selected_verification(&hits);
        assert_eq!(hits.hits.len(), 1);
        exact_source_hit(&hits.hits[0], &changing, &revision, SECOND);
        assert!(hits.hits[0].excerpt.text.contains("29 violet tokens"));
    }
    assert_eq!(
        fs::read(
            fixture
                .fs
                .root()
                .path()
                .join(content_path(&changing, &old_revision).as_str())
        )
        .unwrap(),
        FIRST.as_bytes()
    );
    assert!(
        VectorStore::open(&fixture.fs, None)
            .unwrap()
            .vector(&space, &old_input.input_hash)
            .unwrap()
            .is_some()
    );
    fixture
        .app
        .source_withdraw(stable.clone(), "Withdraw unrelated inventory source")
        .unwrap();
    complete(
        &fixture.offline().embeddings_sync_cached(&settings).unwrap(),
        1,
    );
    let withdrawn = fixture
        .offline()
        .semantic_search_selected(
            "Signalneedle",
            &plan(SearchMode::Semantic, vec![stable], 2),
            None,
            false,
        )
        .unwrap();
    assert!(withdrawn.hits.is_empty());
    cached_noop(&fixture, &settings, 1);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
}

#[test]
fn inventory_current_support_dependency_invalidates_proof_preserves_unrelated_owner() {
    let fixture = normalized();
    let settings = ordinary_settings();
    let a = rel("pages/inventory-a.md");
    let c = rel("pages/inventory-c.md");
    let (source, revision) = add(&fixture, TITLE, "inventory-support.txt", FIRST);
    // Declared Page support must name an Assertion, whose current evidence
    // reaches an immutable capture. Page IDs are invalid declared support.
    fs::write(
        fixture.fs.root().path().join("inventory-entity.md"),
        "---\nwiki_schema: '1'\nwiki_kind: entity\nwiki_id: entity_inventory_support\ntitle: Inventory support identity\nwiki_status: active\nwiki_entity_type: component\n---\n",
    )
    .unwrap();
    fs::write(
        fixture.fs.root().path().join("inventory-assertion.md"),
        "---\nwiki_schema: '1'\nwiki_kind: assertion\nwiki_id: assertion_inventory_support\ntitle: Inventory supported fact\nwiki_status: accepted\nwiki_subject_id: entity_inventory_support\nwiki_object_id: entity_inventory_support\nwiki_predicate: uses\n---\nSupported fixture proposition.\n",
    )
    .unwrap();
    let mut evidence = format!(
        "---\nwiki_schema: '1'\nwiki_kind: evidence\nwiki_id: evidence_inventory_support\ntitle: Exact inventory support\nwiki_status: active\nwiki_assertion_id: assertion_inventory_support\nwiki_source_id: '{source}'\nwiki_source_revision: '{revision}'\nwiki_stance: supports\nwiki_locator_kind: utf8-bytes\nwiki_span_start: 0\nwiki_span_end: {}\nwiki_quote_hash: {}\n---\n",
        FIRST.len(),
        Blake3Hash::digest(FIRST.as_bytes()),
    )
    .into_bytes();
    evidence.extend(
        crate::sources::evidence::exact_quote_body(FIRST.as_bytes(), "\n", "Support").unwrap(),
    );
    // Generated Source IDs are 39 decimal digits. They must remain quoted
    // strings rather than out-of-range YAML integers in a hand-written fixture.
    let parsed_evidence = crate::records::parse_note(&evidence);
    let evidence_record = parsed_evidence
        .canonical
        .as_ref()
        .unwrap_or_else(|| panic!("invalid support Evidence fixture: {parsed_evidence:?}"));
    assert_eq!(evidence_record.kind(), RecordKind::Evidence);
    assert_eq!(
        evidence_record.string("wiki_source_id"),
        Some(source.as_str())
    );
    assert_eq!(
        evidence_record.string("wiki_source_revision"),
        Some(revision.as_str())
    );
    fs::write(
        fixture.fs.root().path().join("inventory-evidence.md"),
        evidence,
    )
    .unwrap();
    fixture.app.index_sync(false).unwrap();
    fixture
        .app
        .page_put(
            a.clone(),
            page_bytes(
                "page_inventory_a",
                Some("assertion_inventory_support"),
                "Consumer unchanged evidence. café 東京.\n",
            ),
            None,
        )
        .unwrap();
    fixture
        .app
        .page_put(
            c.clone(),
            page_bytes("page_inventory_c", None, "Unrelated retained evidence.\n"),
            None,
        )
        .unwrap();
    let b = content_path(&source, &revision);
    let assert_current_chain = |stage: &str| {
        let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        for path in [&a, &b, &c] {
            let document = reader
                .document(path)
                .unwrap()
                .unwrap_or_else(|| panic!("{stage}: missing eligible owner {path}"));
            assert_eq!(
                document.eligibility,
                Eligibility::Current,
                "{stage}: {document:?}; diagnostics: {:?}",
                reader
                    .diagnostics(&std::collections::BTreeSet::from([document.path.clone()]))
                    .unwrap()
            );
        }
        for id in [
            RecordId::new("page_inventory_a").unwrap(),
            RecordId::new("page_inventory_c").unwrap(),
            RecordId::new("assertion_inventory_support").unwrap(),
            RecordId::new("evidence_inventory_support").unwrap(),
            source.clone(),
            revision.clone(),
        ] {
            let row = reader
                .record(&id)
                .unwrap()
                .unwrap_or_else(|| panic!("{stage}: missing support-chain record {id}"));
            assert_eq!(
                row.eligibility,
                Eligibility::Current,
                "{stage}: {row:?}; diagnostics: {:?}",
                reader
                    .diagnostics(&std::collections::BTreeSet::from([row.path.clone()]))
                    .unwrap()
            );
        }
        for (owner, role, target) in [
            (
                "page_inventory_a",
                crate::catalog::eligibility_facts::EligibilityRole::DeclaredSupport,
                "assertion_inventory_support",
            ),
            (
                "assertion_inventory_support",
                crate::catalog::eligibility_facts::EligibilityRole::AssertionEvidence,
                "evidence_inventory_support",
            ),
        ] {
            let edges = reader
                .outgoing_edges(&RecordId::new(owner).unwrap(), &[role])
                .unwrap();
            assert_eq!(
                edges.len(),
                1,
                "{stage}: {owner} support membership: {edges:?}"
            );
            assert_eq!(edges[0].target_id.as_str(), target, "{stage}: {edges:?}");
        }
        let entity = reader
            .record(&RecordId::new("entity_inventory_support").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            entity.identity_eligibility,
            Some(Eligibility::Current),
            "{stage}: assertion endpoints must have Current identity: {entity:?}"
        );
    };
    assert_current_chain("before initial preparation");
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    complete(
        &fixture.app.embeddings_sync(&settings, &runtime).unwrap(),
        3,
    );
    let (a_before, a_units) = owner_state(&fixture, &settings, &a);
    let c_before = owner_state(&fixture, &settings, &c);
    let (_, b_units) = owner_state(&fixture, &settings, &b);
    let changed = fixture
        .app
        .source_refresh_with_title(
            source.clone(),
            capture(TITLE, "inventory-support.txt", FIRST),
            Some("Updated inventory support display title"),
        )
        .unwrap();
    assert!(changed.change.is_some());
    assert_eq!(changed.allocated_ids["revision"], revision);
    assert_current_chain("after dependency-only refresh, before preparation");
    let (a_after, a_current_units) = owner_state(&fixture, &settings, &a);
    assert_eq!(
        a_current_units, a_units,
        "dependency-only change must preserve A's exact inputs"
    );
    assert_eq!(a_after.render_token, a_before.render_token);
    assert!(a_after.proof_version > a_before.proof_version);
    assert_eq!(owner_state(&fixture, &settings, &b).1, b_units);
    assert_eq!(owner_state(&fixture, &settings, &c), c_before);
    let (prepared, work) =
        attribution::with_observation(|| fixture.app.embeddings_sync(&settings, &runtime));
    let prepared = prepared.unwrap();
    complete(&prepared, 3);
    assert_eq!(prepared.generated_inputs, 0);
    assert!(!prepared.network_used);
    assert!(prepared.run_id.is_none());
    assert_eq!(
        work.owner_attempts, 2,
        "A's changed support proof and the Source's changed envelope require authentication, C does not"
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
    assert_eq!(owner_state(&fixture, &settings, &c), c_before);
    cached_noop(&fixture, &settings, 3);

    fixture.query_seed(&fixture.spec(), "Signalneedle", vec![1.0, 0.0]);
    for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
        let hits = fixture
            .offline()
            .semantic_search_selected(
                "Signalneedle",
                &plan(mode, vec![source.clone()], 1),
                None,
                false,
            )
            .unwrap();
        selected_verification(&hits);
        assert_eq!(hits.hits.len(), 1);
        exact_source_hit(&hits.hits[0], &source, &revision, FIRST);
    }
    assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
}

#[test]
fn inventory_catalog_rebuild_and_retained_cache_restore_reuse_vectors_offline() {
    let fixture = normalized();
    let settings = ordinary_settings();
    let (source, revision) = add(&fixture, TITLE, "inventory-rebuild.txt", FIRST);
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    let prepared = fixture.app.embeddings_sync(&settings, &runtime).unwrap();
    complete(&prepared, 1);
    let inputs = paid_inputs(&fixture, &ledger(&fixture, &prepared).inspect().unwrap());
    fixture.query_seed(&fixture.spec(), "Signalneedle", vec![1.0, 0.0]);
    let original = owner_state(&fixture, &settings, &content_path(&source, &revision));
    let cache = fixture.fs.root().path().join(".wiki/cache");
    let mut retained = BTreeMap::new();
    for name in [
        "embeddings.sqlite3",
        "embeddings.sqlite3-wal",
        "embeddings.sqlite3-shm",
    ] {
        let path = cache.join(name);
        if path.exists() {
            retained.insert(name, fs::read(path).unwrap());
        }
    }
    assert!(retained.contains_key("embeddings.sqlite3"));
    fixture.app.index_rebuild_normalized().unwrap();
    let request = plan(SearchMode::Semantic, vec![source.clone()], 1);
    assert_eq!(
        fixture
            .offline()
            .semantic_search_selected("Signalneedle", &request, None, false)
            .unwrap_err()
            .code,
        ErrorCode::OfflineUnavailable
    );
    let (rebuilt, work) =
        attribution::with_observation(|| fixture.offline().embeddings_sync_cached(&settings));
    complete(&rebuilt.unwrap(), 1);
    assert_eq!(
        work.owner_attempts, 1,
        "rebuild cannot reuse old-incarnation owner authority"
    );
    let current = owner_state(&fixture, &settings, &content_path(&source, &revision));
    assert_ne!(current.0.incarnation, original.0.incarnation);
    assert_eq!(current.1, original.1);
    for input in &inputs {
        assert!(
            VectorStore::open(&fixture.fs, None)
                .unwrap()
                .vector(prepared.space.as_ref().unwrap(), &input.input_hash)
                .unwrap()
                .is_some()
        );
    }
    cached_noop(&fixture, &settings, 1);
    for name in [
        "embeddings.sqlite3",
        "embeddings.sqlite3-wal",
        "embeddings.sqlite3-shm",
    ] {
        let path = cache.join(name);
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }
    assert_eq!(
        fixture
            .offline()
            .embeddings_sync_cached(&settings)
            .unwrap_err()
            .code,
        ErrorCode::OfflineUnavailable
    );
    assert_eq!(
        fixture
            .offline()
            .semantic_search_selected("Signalneedle", &request, None, false)
            .unwrap_err()
            .code,
        ErrorCode::OfflineUnavailable
    );
    for (name, bytes) in retained {
        fs::write(cache.join(name), bytes).unwrap();
    }
    complete(
        &fixture.offline().embeddings_sync_cached(&settings).unwrap(),
        1,
    );
    let hits = fixture
        .offline()
        .semantic_search_selected("Signalneedle", &request, None, false)
        .unwrap();
    selected_verification(&hits);
    assert_eq!(hits.hits.len(), 1);
    exact_source_hit(&hits.hits[0], &source, &revision, FIRST);
    cached_noop(&fixture, &settings, 1);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
}
