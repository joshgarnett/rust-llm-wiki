//! Public app workflows for compact, incremental preparation on disposable vaults.
use super::*;
use crate::{
    app::indexed_embedding_inputs::attribution,
    catalog::query_types::{QueryCatalog, QueryReadLimits},
    retrieval::unit_inventory_types::{RenderPolicyId, UnitDescriptor, UnitOwnerBinding},
};
use std::{collections::BTreeMap, fs, sync::atomic::AtomicUsize};

// Retain the original conservative online work ceilings. Detached planning
// and fresh acknowledgment now authenticate changed owners independently;
// pre-send, receipt and post-receipt rebind authenticate paid suppliers.
// Received recovery
// authenticates receipt validation and post-receipt rebind, then independently
// authenticates planning and fresh acknowledgment; ready owners stay untouched.
const ONLINE_CHANGED_OWNER_PASSES: usize = 3;
const ONLINE_PAID_OWNER_PASSES: usize = 3;
const RECEIVED_SINGLE_OWNER_PASSES: usize = 4;

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
        } else if index < 64 {
            "Resumeprobe shared first-page fact has 17 amber tokens. café 東京 🦀.\n"
        } else {
            "Resumeprobe second shared first-page fact has 23 green tokens. café 東京 🦀.\n"
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
    let fault = Arc::new(ThirdReceivedCut {
        received: AtomicUsize::new(0),
    });
    let mut interrupted = runtime(&fixture.service, &dispatch);
    interrupted.job_options.fault = Some(fault.clone());
    // The first page acquires two shared inputs from one authenticated
    // supplier each; all 128 owners receive separate fresh acknowledgments.
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
    assert_eq!(
        work.owner_attempts, RECEIVED_SINGLE_OWNER_PASSES,
        "only the final owner needs receipt validation, post-receipt rebind, planning and fresh acknowledgment; the first 128 owners stay ready"
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
        work.owner_attempts, 4,
        "A's changed support proof and the Source's changed envelope each need planning and fresh acknowledgment; C does not"
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
        work.owner_attempts, 2,
        "rebuild needs independent planning and fresh acknowledgment; old-incarnation authority is not reused"
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

fn retained_marker_count(fixture: &Fixture) -> usize {
    fs::read_dir(fixture.fs.root().path().join(".wiki/state/embedding-jobs"))
        .unwrap()
        .count()
}

/// Inspect the actual named canonical receipt publication, rather than a
/// freshly calculated partition or a later reconstruction of its preconditions.
fn assert_receipt_guards(fixture: &Fixture, inspection: &LedgerInspection) {
    let engine = crate::changes::ChangeEngine::new(fixture.fs.clone()).unwrap();
    for attempt in &inspection.attempts {
        let reference = attempt.receipt.as_ref().unwrap();
        let receipt = crate::jobs::checkpoint::receipt(&fixture.fs, reference).unwrap();
        assert_eq!(receipt.output_disposition, OutputDisposition::Validated);
        let committed = crate::jobs::checkpoint::named_job_committed(
            &fixture.fs,
            fixture.app.vault_id(),
            &inspection.spec.run_id,
            crate::jobs::checkpoint::JobPublicationKey::Receipt {
                receipt_id: receipt.receipt_id.clone(),
            },
            &reference.path,
            &reference.hash,
        )
        .unwrap()
        .unwrap();
        let (manifest, hash) = engine.load_manifest(&committed.change_id).unwrap();
        assert_eq!(hash, committed.manifest_hash);
        let task = &inspection.tasks[&attempt.attempt.task_key].spec;
        let mut expected = BTreeMap::new();
        for guard in inspection
            .spec
            .scope
            .read_preconditions
            .iter()
            .chain(&task.source_bindings)
        {
            assert!(
                expected
                    .insert(&guard.path, &guard.expected)
                    .is_none_or(|previous| previous == &guard.expected)
            );
            assert!(manifest.read_preconditions.contains(guard));
        }
        assert_eq!(manifest.read_preconditions.len(), expected.len() + 1);
        assert!(manifest.read_preconditions.len() <= 128);
        let run = inspection.run_note.as_ref().unwrap();
        assert!(
            manifest
                .read_preconditions
                .iter()
                .any(|guard| guard.path == run.path)
        );
    }
}

#[test]
fn inventory_automatic_guard_identical_128_129_257_acquires_once_and_noop_has_zero_owner_work() {
    for owners in [128, 129, 257] {
        let fixture = normalized();
        for index in 0..owners {
            fs::write(
                fixture
                    .fs
                    .root()
                    .path()
                    .join(format!("identical-{index:03}.md")),
                page_bytes(
                    &format!("page_identical_{index:03}"),
                    None,
                    "One identical corpus input stores 17 amber tokens. café 東京 🦀.\n",
                ),
            )
            .unwrap();
        }
        fixture.app.index_sync(false).unwrap();
        let responses = Arc::new(Responses::new());
        let dispatch = dispatcher(&fixture.fs, responses.clone());
        let runtime = runtime(&fixture.service, &dispatch);
        let report = fixture
            .app
            .embeddings_sync(&ordinary_settings(), &runtime)
            .unwrap();
        complete(&report, owners);
        assert_eq!(report.generated_inputs, 1);
        assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
        assert_eq!(retained_marker_count(&fixture), 1);
        let job = ledger(&fixture, &report);
        let inspection = job.inspect().unwrap();
        assert_eq!(inspection.tasks.len(), 1);
        assert_eq!(inspection.attempts.len(), 1);
        let task = &inspection.tasks.values().next().unwrap().spec;
        assert_eq!(
            task.source_bindings.len(),
            2,
            "supplier owner and WIKI.md only"
        );
        assert_receipt_guards(&fixture, &inspection);
        let marker: serde_json::Value = serde_json::from_slice(
            &fs::read(
                fs::read_dir(fixture.fs.root().path().join(".wiki/state/embedding-jobs"))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(marker["expected_units"].as_array().unwrap().len(), 1);
        assert_eq!(marker["expected_units"][0]["owner"], "identical-000.md");
        cached_noop(&fixture, &ordinary_settings(), owners);
        assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
    }
}

/// Grow an actual selected support closure to the requested guard count. Each
/// extra valid Evidence adds exactly one canonical path to the same Assertion.
/// The corpus still has one Source and one authored Page, rather than hundreds
/// of artificial provider inputs or hand-injected read guards.
fn owner_with_guard_count(
    fixture: &Fixture,
    prefix: &str,
    guards: usize,
    body: &str,
) -> VaultRelativePath {
    let (source, revision) = add(fixture, TITLE, &format!("{prefix}-support.txt"), FIRST);
    let assertion = format!("assertion_{prefix}");
    let entity = format!("entity_{prefix}");
    fs::write(fixture.fs.root().path().join(format!("{prefix}-entity.md")),
        format!("---\nwiki_schema: '1'\nwiki_kind: entity\nwiki_id: {entity}\ntitle: Guard identity\nwiki_status: active\nwiki_entity_type: component\n---\n")).unwrap();
    fs::write(fixture.fs.root().path().join(format!("{prefix}-assertion.md")),
        format!("---\nwiki_schema: '1'\nwiki_kind: assertion\nwiki_id: {assertion}\ntitle: Guard support\nwiki_status: accepted\nwiki_subject_id: {entity}\nwiki_object_id: {entity}\nwiki_predicate: uses\n---\nSupported proposition.\n")).unwrap();
    let evidence = |index: usize| {
        let mut bytes = format!("---\nwiki_schema: '1'\nwiki_kind: evidence\nwiki_id: evidence_{prefix}_{index:03}\ntitle: Exact guard support\nwiki_status: active\nwiki_assertion_id: {assertion}\nwiki_source_id: '{source}'\nwiki_source_revision: '{revision}'\nwiki_stance: supports\nwiki_locator_kind: utf8-bytes\nwiki_span_start: 0\nwiki_span_end: {}\nwiki_quote_hash: {}\n---\n", FIRST.len(), Blake3Hash::digest(FIRST.as_bytes())).into_bytes();
        bytes.extend(
            crate::sources::evidence::exact_quote_body(FIRST.as_bytes(), "\n", "Support").unwrap(),
        );
        fs::write(
            fixture
                .fs
                .root()
                .path()
                .join(format!("{prefix}-evidence-{index:03}.md")),
            bytes,
        )
        .unwrap();
    };
    evidence(0);
    let owner = rel(&format!("{prefix}-page.md"));
    fs::write(
        fixture.fs.root().path().join(owner.as_str()),
        page_bytes(&format!("page_{prefix}"), Some(&assertion), body),
    )
    .unwrap();
    fixture.app.index_sync(false).unwrap();
    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    let proof = super::super::indexed_embedding_inputs::materialize(
        &catalog,
        &ordinary_settings(),
        Some(std::slice::from_ref(&owner)),
        &crate::retrieval::context_types::VerificationBudget::default(),
    )
    .unwrap();
    let base = proof.owner_dependencies().unwrap()[0].1.len();
    assert!(base <= guards);
    drop(proof);
    for index in 1..=guards - base {
        evidence(index);
    }
    fixture.app.index_sync(false).unwrap();
    let proof = super::super::indexed_embedding_inputs::materialize(
        &catalog,
        &ordinary_settings(),
        Some(std::slice::from_ref(&owner)),
        &crate::retrieval::context_types::VerificationBudget::default(),
    )
    .unwrap();
    assert_eq!(proof.owner_dependencies().unwrap()[0].1.len(), guards);
    owner
}

/// Historical merged identities are part of the selected subject's complete
/// reverse-incidence proof, with one canonical path per identity. This keeps
/// the receipt guard geometry exact without repeating a captured payload's
/// multiple direct-path/edge rows for every extra Evidence record.
fn owner_with_identity_guard_count(
    fixture: &Fixture,
    prefix: &str,
    guards: usize,
    body: &str,
) -> VaultRelativePath {
    let base_guards = 12;
    assert!(guards >= base_guards);
    let owner = owner_with_guard_count(fixture, prefix, base_guards, body);
    for index in 0..guards - base_guards {
        let bytes = format!(
            "---\nwiki_schema: '1'\nwiki_kind: entity\nwiki_id: entity_{prefix}_historical_{index:03}\ntitle: Historical merged guard identity\nwiki_status: superseded\nwiki_entity_type: component\nwiki_superseded_by_id: entity_{prefix}\n---\n"
        );
        let parsed = crate::records::parse_note(bytes.as_bytes());
        assert_eq!(
            parsed.canonical.as_ref().unwrap().kind(),
            RecordKind::Entity
        );
        fs::write(
            fixture
                .fs
                .root()
                .path()
                .join(format!("{prefix}-historical-{index:03}.md")),
            bytes,
        )
        .unwrap();
    }
    fixture.app.index_sync(false).unwrap();
    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    let proof = super::super::indexed_embedding_inputs::materialize(
        &catalog,
        &ordinary_settings(),
        Some(std::slice::from_ref(&owner)),
        &crate::retrieval::context_types::VerificationBudget::default(),
    )
    .unwrap();
    assert_eq!(proof.owner_dependencies().unwrap()[0].1.len(), guards);
    assert!(
        proof.units.len() > 0,
        "current supported Page must remain eligible"
    );
    owner
}

#[test]
fn inventory_automatic_guard_actual_126_127_receipts_and_unique_128_preflight() {
    for guards in [126, 127, 128] {
        let fixture = normalized();
        let owner = owner_with_guard_count(
            &fixture,
            "boundary",
            guards,
            "The uniquely supplied Page stores 59 silver tokens.\n",
        );
        let responses = Arc::new(Responses::new());
        let dispatch = dispatcher(&fixture.fs, responses.clone());
        let runtime = runtime(&fixture.service, &dispatch);
        let result = fixture.app.embeddings_sync(&ordinary_settings(), &runtime);
        if guards == 128 {
            let error = result.unwrap_err();
            assert_eq!(error.code, ErrorCode::BudgetExceeded);
            assert!(error.message.contains("128 source guards"));
            assert!(error.message.contains("127 are available"));
            assert_eq!(error.details["owner"], owner.as_str());
            assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
            assert!(
                !fixture
                    .fs
                    .root()
                    .path()
                    .join(".wiki/state/embedding-jobs")
                    .exists()
            );
        } else {
            let report = result.unwrap();
            complete(&report, 2);
            let inspection = ledger(&fixture, &report).inspect().unwrap();
            assert!(
                inspection
                    .tasks
                    .values()
                    .any(|task| task.spec.source_bindings.len() == guards)
            );
            assert_receipt_guards(&fixture, &inspection);
            assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
            cached_noop(&fixture, &ordinary_settings(), 2);
        }
    }
}

#[test]
fn inventory_automatic_guard_oversized_first_supplier_uses_alternate_and_cached_rebuild() {
    let fixture = normalized();
    let body = "An oversized owner reuses exactly these 59 silver tokens.\n";
    let oversized = owner_with_guard_count(&fixture, "aoversized", 128, body);
    let alternate = rel("z-feasible.md");
    fs::write(
        fixture.fs.root().path().join(alternate.as_str()),
        page_bytes("page_feasible", None, body),
    )
    .unwrap();
    fixture.app.index_sync(false).unwrap();
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    let report = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &runtime)
        .unwrap();
    complete(&report, 3);
    let inspection = ledger(&fixture, &report).inspect().unwrap();
    assert_eq!(paid_inputs(&fixture, &inspection).len(), 2);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    assert!(inspection.tasks.values().any(|task| {
        task.spec
            .source_bindings
            .iter()
            .any(|guard| guard.path == alternate)
    }));
    assert!(inspection.tasks.values().all(|task| {
        task.spec
            .source_bindings
            .iter()
            .all(|guard| guard.path != oversized)
    }));
    assert_receipt_guards(&fixture, &inspection);
    cached_noop(&fixture, &ordinary_settings(), 3);
    // The oversized owner can subsequently reuse the compatible paid vector
    // without any remaining feasible authored supplier or a new receipt.
    fs::remove_file(fixture.fs.root().path().join(alternate.as_str())).unwrap();
    fixture.app.index_sync(false).unwrap();
    fixture.app.index_rebuild_normalized().unwrap();
    let (rebuilt, work) = attribution::with_observation(|| {
        fixture
            .offline()
            .embeddings_sync_cached(&ordinary_settings())
    });
    complete(&rebuilt.unwrap(), 2);
    assert_eq!(
        work.authenticated_owners, 4,
        "both rebuilt owners need independent planning and fresh acknowledgment"
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    cached_noop(&fixture, &ordinary_settings(), 2);
}

#[test]
fn inventory_automatic_guard_two_request_three_task_resume_requires_existing_amendment() {
    let fixture = normalized();
    for index in 0..3 {
        fs::write(
            fixture.fs.root().path().join(format!("budget-{index}.md")),
            page_bytes(
                &format!("page_budget_{index}"),
                None,
                &format!("Unique corpus fact number {index} stores amber tokens.\n"),
            ),
        )
        .unwrap();
    }
    fixture.app.index_sync(false).unwrap();
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let mut limited = runtime(&fixture.service, &dispatch);
    limited.limits.requests = 2;
    let error = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &limited)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    let run = serde_json::from_value(error.details["run_id"].clone()).unwrap();
    let job = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        run,
        options(),
    )
    .unwrap();
    let before = job.inspect().unwrap();
    assert_eq!(before.tasks.len(), 3);
    assert_eq!(before.attempts.len(), 2);
    assert_eq!(before.budget.dispatched_requests, 2);
    assert_eq!(before.budget.unknown_attempts.len(), 2);
    assert_eq!(retained_marker_count(&fixture), 1);
    let again = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &limited)
        .unwrap_err();
    assert_eq!(again.code, ErrorCode::BudgetExceeded);
    assert_eq!(
        again.details["run_id"],
        serde_json::to_value(&before.spec.run_id).unwrap()
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    let stopped = job.inspect().unwrap();
    assert_eq!(stopped.attempts, before.attempts);
    assert_eq!(stopped.budget.outstanding, before.budget.outstanding);
    assert_eq!(
        stopped.budget.unknown_attempts,
        before.budget.unknown_attempts
    );
    assert_eq!(retained_marker_count(&fixture), 1);
    limited.limits.requests = 3;
    job.amend_retained_limits(
        limited.limits.clone(),
        before.effective_deadline_utc_ms,
        "Explicit allowance for the existing third receipt".into(),
    )
    .unwrap();
    let report = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &limited)
        .unwrap();
    complete(&report, 3);
    assert_eq!(report.run_id, Some(before.spec.run_id.clone()));
    assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
    assert_eq!(retained_marker_count(&fixture), 1);
    let after = job.inspect().unwrap();
    assert_eq!(after.attempts.len(), 3);
    assert_eq!(after.budget.dispatched_requests, 3);
    assert_eq!(&after.attempts[..2], before.attempts.as_slice());
    assert_eq!(after.budget.unknown_attempts.len(), 3);
    assert_receipt_guards(&fixture, &after);
    cached_noop(&fixture, &ordinary_settings(), 3);
}

struct BatchResponses {
    fs: crate::vault::VaultFs,
    vault: RecordId,
    calls: AtomicUsize,
    items: AtomicUsize,
}
impl crate::providers::types::Transport for BatchResponses {
    fn execute<'a>(
        &'a self,
        request: crate::providers::types::AuthenticatedRequest<'a>,
        _: crate::providers::types::TransportContext,
    ) -> crate::providers::types::TransportFuture<'a> {
        let summary = request.summary();
        let job = JobLedger::new(
            self.fs.clone(),
            self.vault.clone(),
            summary.attempt.run_id.clone(),
            options(),
        )
        .unwrap();
        let inspection = job.inspect().unwrap();
        let reference = &inspection.tasks[&summary.attempt.task_key].spec.input;
        let bytes = fs::read(self.fs.root().path().join(reference.path.as_str())).unwrap();
        assert_eq!(Blake3Hash::digest(&bytes), reference.hash);
        let remote: RemoteInput = serde_json::from_slice(&bytes).unwrap();
        let RemoteOperation::Embed { inputs, .. } = remote.operation else {
            panic!("embedding task");
        };
        assert!(!inputs.is_empty() && inputs.len() <= 32);
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.items.fetch_add(inputs.len(), Ordering::SeqCst);
        let data: Vec<_> = inputs
            .iter()
            .enumerate()
            .map(|(index, input)| {
                assert_eq!(Blake3Hash::digest(input.utf8.as_bytes()), input.input_hash);
                serde_json::json!({"index":index,"embedding":[1.0,0.0]})
            })
            .collect();
        Box::pin(async move {
            Ok(crate::providers::types::TransportReply::new(
                200,
                vec![],
                serde_json::to_vec(&serde_json::json!({"model":"test-model","data":data})).unwrap(),
            )
            .unwrap())
        })
    }
}
struct NoBatchJitter;
impl crate::providers::types::JitterSource for NoBatchJitter {
    fn sample_inclusive(&self, _: u64) -> Result<u64> {
        Ok(0)
    }
}
fn batch_dispatch(
    fixture: &mut Fixture,
) -> (
    crate::providers::dispatcher::Dispatcher,
    Arc<BatchResponses>,
) {
    let config = fs::read_to_string(&fixture.config)
        .unwrap()
        .replace("max_batch_items=1", "max_batch_items=32");
    common::private_write(&fixture.config, config);
    fixture.service = crate::config::providers::ProviderConfig::load(&fixture.config)
        .unwrap()
        .authorize(
            &fixture.fs,
            fixture.app.vault_id(),
            "primary",
            Capability::Embed,
        )
        .unwrap();
    let responses = Arc::new(BatchResponses {
        fs: fixture.fs.clone(),
        vault: fixture.app.vault_id().clone(),
        calls: AtomicUsize::new(0),
        items: AtomicUsize::new(0),
    });
    let dispatch = crate::providers::dispatcher::Dispatcher::new(
        fixture.fs.clone(),
        crate::providers::types::DispatchOptions {
            broker: Arc::new(crate::providers::credentials::CredentialBroker::new(
                crate::providers::credentials::CredentialOptions {
                    clock: Arc::new(common::TestClock),
                    inputs: Arc::new(crate::providers::credentials::NativeSecretInputs),
                    runner: Arc::new(crate::providers::credentials::NativeHelperRunner),
                },
            )),
            transport: responses.clone(),
            jitter: Arc::new(NoBatchJitter),
        },
    );
    (dispatch, responses)
}

#[test]
fn inventory_automatic_guard_overlapping_multi_unit_scopes_fit_actual_shared_union() {
    let mut fixture = normalized();
    let body = format!(
        "{}{}",
        "Alpha guard body stores 59 silver tokens.\n".repeat(240),
        "Beta guard body stores 61 violet tokens.\n".repeat(240)
    );
    let owner = owner_with_identity_guard_count(&fixture, "overlap", 126, &body);
    let original = fs::read(fixture.fs.root().path().join(owner.as_str())).unwrap();
    // A duplicate owner with the same support closure and two distinct units
    // is acknowledged independently, while each input is acquired once.
    let duplicate = String::from_utf8(original)
        .unwrap()
        .replace("wiki_id: page_overlap", "wiki_id: page_overlap_duplicate");
    fs::write(
        fixture.fs.root().path().join("overlap-duplicate.md"),
        duplicate,
    )
    .unwrap();
    fixture.app.index_sync(false).unwrap();
    let (dispatch, responses) = batch_dispatch(&mut fixture);
    let runtime = runtime(&fixture.service, &dispatch);
    let report = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &runtime)
        .unwrap();
    let inspection = ledger(&fixture, &report).inspect().unwrap();
    let submitted = paid_inputs(&fixture, &inspection);
    assert_eq!(
        submitted
            .iter()
            .map(|input| &input.input_hash)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        submitted.len()
    );
    assert!(submitted.len() >= 3, "two Page units and one Source input");
    assert_eq!(
        inspection.tasks.len(),
        1,
        "overlapping complete guards count once"
    );
    assert_eq!(
        inspection
            .tasks
            .values()
            .next()
            .unwrap()
            .spec
            .source_bindings
            .len(),
        126
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
    assert_eq!(responses.items.load(Ordering::SeqCst), submitted.len());
    assert_eq!(retained_marker_count(&fixture), 1);
    assert_receipt_guards(&fixture, &inspection);
    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let policy =
        RenderPolicyId::for_settings(&reader.snapshot().parser_fingerprint, &ordinary_settings())
            .unwrap();
    let count = reader
        .unit_inventory_state(&policy)
        .unwrap()
        .unwrap()
        .unit_count;
    complete(&report, count);
    drop(reader);
    cached_noop(&fixture, &ordinary_settings(), count);
}

#[test]
fn inventory_automatic_guard_three_disjoint_scopes_share_one_limited_run() {
    let mut fixture = normalized();
    for index in 0..3 {
        owner_with_identity_guard_count(
            &fixture,
            &format!("disjoint{index}"),
            70,
            &format!("The unique disjoint supplier {index} stores 59 silver tokens.\n"),
        );
    }
    let (dispatch, responses) = batch_dispatch(&mut fixture);
    let mut limited = runtime(&fixture.service, &dispatch);
    limited.limits.requests = 2;
    let (error, initial_work) = attribution::with_observation(|| {
        fixture.app.embeddings_sync(&ordinary_settings(), &limited)
    });
    let error = error.unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded, "{error:?}");
    assert_eq!(
        initial_work.prior_accounting_discovery_calls, 1,
        "fixture must reach paid Run allocation; actual error: {error:?}; authenticated owners: {}; cache rows: {}",
        initial_work.authenticated_owners, initial_work.cache_rows
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    let run = serde_json::from_value(error.details["run_id"].clone()).unwrap();
    let job = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        run,
        options(),
    )
    .unwrap();
    let before = job.inspect().unwrap();
    assert_eq!(
        before.tasks.len(),
        3,
        "actual guard unions force three receipts despite 32-item allowance"
    );
    assert_eq!(before.attempts.len(), 2);
    assert_receipt_guards(&fixture, &before);
    let (again, resume_work) = attribution::with_observation(|| {
        fixture.app.embeddings_sync(&ordinary_settings(), &limited)
    });
    let again = again.unwrap_err();
    assert_eq!(resume_work.prior_accounting_discovery_calls, 0);
    assert_eq!(again.code, ErrorCode::BudgetExceeded);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    assert_eq!(retained_marker_count(&fixture), 1);
    assert_eq!(job.inspect().unwrap().attempts, before.attempts);
    limited.limits.requests = 3;
    job.amend_retained_limits(
        limited.limits.clone(),
        before.effective_deadline_utc_ms,
        "Explicit third guard-constrained receipt".into(),
    )
    .unwrap();
    let (report, amended_work) = attribution::with_observation(|| {
        fixture.app.embeddings_sync(&ordinary_settings(), &limited)
    });
    let report = report.unwrap();
    assert_eq!(amended_work.prior_accounting_discovery_calls, 0);
    complete(&report, 6);
    assert_eq!(report.run_id, Some(before.spec.run_id.clone()));
    assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        responses.items.load(Ordering::SeqCst),
        4,
        "three unique Page inputs plus one shared Source input"
    );
    assert_eq!(retained_marker_count(&fixture), 1);
    let after = job.inspect().unwrap();
    assert_eq!(&after.attempts[..2], before.attempts.as_slice());
    assert_eq!(after.attempts.len(), 3);
    assert_eq!(after.budget.unknown_attempts.len(), 3);
    assert_receipt_guards(&fixture, &after);
    cached_noop(&fixture, &ordinary_settings(), 6);
}

#[test]
fn inventory_automatic_guard_received_same_page_replays_before_pending_without_resend() {
    let fixture = normalized();
    for index in 0..3 {
        fs::write(
            fixture
                .fs
                .root()
                .path()
                .join(format!("received-{index}.md")),
            page_bytes(
                &format!("page_received_{index}"),
                None,
                &format!("A unique Received supplier {index} stores amber tokens.\n"),
            ),
        )
        .unwrap();
    }
    fixture.app.index_sync(false).unwrap();
    let fault = Arc::new(ReceivedOnce {
        armed: AtomicBool::new(true),
    });
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let mut interrupted = runtime(&fixture.service, &dispatch);
    interrupted.job_options.fault = Some(fault);
    let stopped = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &interrupted)
        .unwrap_err();
    let run = serde_json::from_value(stopped.details["run_id"].clone()).unwrap();
    let job = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        run,
        options(),
    )
    .unwrap();
    let before = job.inspect().unwrap();
    assert_eq!(before.tasks.len(), 3);
    assert_eq!(before.attempts.len(), 1);
    assert_eq!(before.attempts[0].phase, AttemptPhase::Received);
    let received_spool = before.attempts[0]
        .spool
        .as_ref()
        .expect("durable Received spool");
    for reference in [&received_spool.response, &received_spool.metadata] {
        let bytes = fs::read(fixture.fs.root().path().join(reference.path.as_str())).unwrap();
        assert_eq!(bytes.len() as u64, reference.byte_len);
        assert_eq!(Blake3Hash::digest(&bytes), reference.hash);
    }
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
    let resumed = runtime(&fixture.service, &dispatch);
    let report = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &resumed)
        .unwrap();
    complete(&report, 3);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
    assert_eq!(retained_marker_count(&fixture), 1);
    let after = job.inspect().unwrap();
    assert_eq!(after.attempts.len(), 3);
    assert_eq!(after.attempts[0].attempt, before.attempts[0].attempt);
    assert!(
        after.attempts[0].spool.is_none(),
        "verified settled response is pruned"
    );
    let history =
        crate::jobs::checkpoint::storage_run_history(&fixture.fs, &after.spec.run_id).unwrap();
    assert!(
        history.frames.iter().any(|frame| matches!(
            &frame.event.payload, EventPayload::SpoolRemoved { attempt }
                if attempt == &before.attempts[0].attempt
        )),
        "head-authenticated history must record exact spool removal"
    );
    assert!(
        !fixture
            .fs
            .root()
            .path()
            .join(received_spool.response.path.as_str())
            .exists()
    );
    assert!(
        !fixture
            .fs
            .root()
            .path()
            .join(received_spool.metadata.path.as_str())
            .exists()
    );
    let receipt = crate::jobs::checkpoint::receipt(
        &fixture.fs,
        after.attempts[0]
            .receipt
            .as_ref()
            .expect("retained receipt survives cleanup"),
    )
    .unwrap();
    assert_eq!(receipt.attempt, before.attempts[0].attempt);
    assert_eq!(receipt.output_disposition, OutputDisposition::Validated);
    assert_eq!(receipt.cache_outputs, after.attempts[0].cache_outputs);
    assert!(!receipt.cache_outputs.is_empty());
    assert_eq!(
        after.tasks[&before.attempts[0].attempt.task_key].cache_outputs,
        receipt.cache_outputs
    );
    assert_eq!(after.attempts[0].phase, AttemptPhase::Settled);
    assert_eq!(after.budget.dispatched_requests, 3);
    assert_eq!(after.budget.unknown_attempts.len(), 3);
    assert_eq!(
        after.attempts[0].billing,
        BillingDisposition::UnknownReserved
    );
    assert_eq!(after.attempts[0].allowance, before.attempts[0].allowance);
    assert_receipt_guards(&fixture, &after);
    cached_noop(&fixture, &ordinary_settings(), 3);
}

#[test]
fn inventory_automatic_guard_changed_received_supplier_cannot_borrow_unchanged_duplicate_proof() {
    let fixture = normalized();
    for name in ["a-supplier", "z-duplicate"] {
        fs::write(
            fixture.fs.root().path().join(format!("{name}.md")),
            page_bytes(&format!("page_{}", name.replace('-', "_")), None, FIRST),
        )
        .unwrap();
    }
    fixture.app.index_sync(false).unwrap();
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let mut interrupted = runtime(&fixture.service, &dispatch);
    interrupted.job_options.fault = Some(Arc::new(ReceivedOnce {
        armed: AtomicBool::new(true),
    }));
    let stopped = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &interrupted)
        .unwrap_err();
    let run = serde_json::from_value(stopped.details["run_id"].clone()).unwrap();
    let job = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        run,
        options(),
    )
    .unwrap();
    let before = job.inspect().unwrap();
    let original_paid_inputs = paid_inputs(&fixture, &before);
    let task = &before.tasks.values().next().unwrap().spec;
    assert!(
        task.source_bindings
            .iter()
            .any(|guard| guard.path == rel("a-supplier.md"))
    );
    assert!(
        !task
            .source_bindings
            .iter()
            .any(|guard| guard.path == rel("z-duplicate.md"))
    );
    fs::write(
        fixture.fs.root().path().join("a-supplier.md"),
        page_bytes("page_a_supplier", None, SECOND),
    )
    .unwrap();
    fixture.app.index_sync(false).unwrap();
    let resumed = runtime(&fixture.service, &dispatch);
    let replacement = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &resumed)
        .unwrap();
    complete(&replacement, 2);
    assert_eq!(replacement.generated_inputs, 2);
    assert!(
        replacement
            .warnings
            .iter()
            .any(|warning| warning.contains("rejected"))
    );
    assert_ne!(replacement.run_id.as_ref(), Some(&before.spec.run_id));
    assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
    let replacement_ledger = ledger(&fixture, &replacement).inspect().unwrap();
    let replacement_inputs = paid_inputs(&fixture, &replacement_ledger);
    assert_eq!(
        replacement_inputs.len(),
        2,
        "old supplier authority cannot fund the unchanged duplicate"
    );
    assert!(
        replacement_inputs
            .iter()
            .any(|input| input.utf8.contains("17 amber tokens"))
    );
    assert!(
        replacement_inputs
            .iter()
            .any(|input| input.utf8.contains("29 violet tokens"))
    );
    let after = job.inspect().unwrap();
    assert_eq!(after.state, RunState::Failed);
    assert_eq!(
        after.tasks.values().next().unwrap().state,
        TaskState::Failed
    );
    assert_eq!(after.effective_limits, before.effective_limits);
    assert_eq!(
        after.effective_deadline_utc_ms,
        before.effective_deadline_utc_ms
    );
    assert_eq!(after.attempts[0].attempt, before.attempts[0].attempt);
    assert_eq!(after.attempts[0].phase, AttemptPhase::Settled);
    assert!(after.attempts[0].cache_outputs.is_empty());
    let receipt =
        crate::jobs::checkpoint::receipt(&fixture.fs, after.attempts[0].receipt.as_ref().unwrap())
            .unwrap();
    assert_eq!(receipt.output_disposition, OutputDisposition::Rejected);
    assert_eq!(
        after.budget.unknown_attempts,
        before.budget.unknown_attempts
    );
    assert_eq!(after.budget.outstanding, before.budget.outstanding);
    assert!(
        VectorStore::open(&fixture.fs, None)
            .unwrap()
            .active()
            .unwrap()
            .is_some()
    );
    for path in ["a-supplier.md", "z-duplicate.md"] {
        let (binding, _) = owner_state(&fixture, &ordinary_settings(), &rel(path));
        assert!(
            VectorStore::open(&fixture.fs, None)
                .unwrap()
                .owner_binding_ready(&fixture.spec().id().unwrap(), &binding)
                .unwrap()
        );
    }
    assert_eq!(
        fs::read(fixture.fs.root().resolve(&task.input.path).unwrap())
            .unwrap()
            .len() as u64,
        task.input.byte_len
    );
    assert!(paid_inputs(&fixture, &after) == original_paid_inputs);
    complete(
        &fixture
            .app
            .embeddings_sync(&ordinary_settings(), &resumed)
            .unwrap(),
        2,
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
    assert_eq!(job.inspect().unwrap().attempts, after.attempts);
}

#[test]
fn inventory_automatic_guard_packing_reserved_scope_conflicts_and_encoded_singletons() {
    use crate::changes::ReadDependency;
    use crate::vault::ExpectedState;
    let mut fixture = normalized();
    let (dispatch, responses) = batch_dispatch(&mut fixture);
    let runtime = runtime(&fixture.service, &dispatch);
    let input = |text: &str| EmbeddingInput {
        utf8: text.into(),
        input_hash: Blake3Hash::digest(text.as_bytes()),
    };
    let inputs = vec![input("first"), input("second")];
    let shared: Vec<_> = (0..126)
        .map(|index| ReadDependency {
            path: rel(&format!("shared-{index:03}.md")),
            expected: ExpectedState::Absent,
        })
        .collect();
    let mut bindings = BTreeMap::new();
    for (index, item) in inputs.iter().enumerate() {
        let mut guards = shared.clone();
        guards.push(ReadDependency {
            path: rel(&format!("unique-{index}.md")),
            expected: ExpectedState::Absent,
        });
        bindings.insert(item.input_hash.clone(), guards);
    }
    let packed = fixture
        .app
        .embedding_guard_batches(&runtime, &inputs, Some(&bindings), &[])
        .unwrap();
    assert_eq!(
        packed.len(),
        2,
        "128 distinct source guards plus the reserved Run guard cannot share one receipt"
    );
    let one = std::slice::from_ref(&inputs[0]);
    assert_eq!(
        fixture
            .app
            .embedding_guard_batches(&runtime, one, Some(&bindings), &shared[..1])
            .unwrap()
            .len(),
        1,
        "a scope guard already in the task union counts once"
    );
    let scope = [ReadDependency {
        path: rel("scope-only.md"),
        expected: ExpectedState::Absent,
    }];
    assert_eq!(
        fixture
            .app
            .embedding_guard_batches(&runtime, one, Some(&bindings), &scope)
            .err()
            .expect("packing must reject the invalid scope or input")
            .code,
        ErrorCode::BudgetExceeded
    );
    bindings.get_mut(&inputs[1].input_hash).unwrap()[0].expected =
        ExpectedState::Hash(Blake3Hash::digest(b"changed"));
    assert_eq!(
        fixture
            .app
            .embedding_guard_batches(&runtime, &inputs, Some(&bindings), &[])
            .err()
            .expect("packing must reject the invalid scope or input")
            .code,
        ErrorCode::FreshnessConflict
    );
    let escaped = input(&"\"\\\n".repeat(60_000));
    assert_eq!(
        fixture
            .app
            .embedding_guard_batches(&runtime, &[inputs[0].clone(), escaped], None, &[])
            .err()
            .expect("packing must reject the invalid scope or input")
            .code,
        ErrorCode::BudgetExceeded,
        "a singleton that follows a full encoded batch must be checked before any dispatch"
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn inventory_automatic_guard_mixed_stale_received_continues_same_run_and_retains_holds() {
    let fixture = normalized();
    for (path, id, body) in [
        ("a-mixed.md", "page_a_mixed", FIRST),
        ("z-mixed.md", "page_z_mixed", SECOND),
    ] {
        fs::write(
            fixture.fs.root().path().join(path),
            page_bytes(id, None, body),
        )
        .unwrap();
    }
    fixture.app.index_sync(false).unwrap();
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let mut interrupted = runtime(&fixture.service, &dispatch);
    interrupted.job_options.fault = Some(Arc::new(ReceivedOnce {
        armed: AtomicBool::new(true),
    }));
    let stopped = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &interrupted)
        .unwrap_err();
    let run = serde_json::from_value(stopped.details["run_id"].clone()).unwrap();
    let job = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        run,
        options(),
    )
    .unwrap();
    let before = job.inspect().unwrap();
    assert_eq!(before.tasks.len(), 2);
    assert_eq!(before.attempts.len(), 1);
    assert_eq!(before.attempts[0].phase, AttemptPhase::Received);
    let task = &before.tasks[&before.attempts[0].attempt.task_key].spec;
    let supplier = task
        .source_bindings
        .iter()
        .find(|guard| guard.path == rel("a-mixed.md") || guard.path == rel("z-mixed.md"))
        .unwrap();
    let id = if supplier.path == rel("a-mixed.md") {
        "page_a_mixed"
    } else {
        "page_z_mixed"
    };
    fs::write(
        fixture.fs.root().path().join(supplier.path.as_str()),
        page_bytes(
            id,
            None,
            "A changed supplier now stores 83 copper tokens.\n",
        ),
    )
    .unwrap();
    fixture.app.index_sync(false).unwrap();
    let resumed = runtime(&fixture.service, &dispatch);
    let report = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &resumed)
        .unwrap();
    complete(&report, 2);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("rejected"))
    );
    let after = job.inspect().unwrap();
    assert_eq!(after.state, RunState::Failed);
    assert_eq!(
        after.attempts.len(),
        2,
        "unchanged sibling uses the original Run"
    );
    assert_eq!(after.attempts[0].attempt, before.attempts[0].attempt);
    assert_eq!(after.attempts[0].phase, AttemptPhase::Settled);
    assert!(after.attempts[0].cache_outputs.is_empty());
    assert_eq!(after.budget.unknown_attempts.len(), 2);
    assert_eq!(after.budget.outstanding.requests, 2);
    assert_eq!(
        after
            .tasks
            .values()
            .filter(|task| task.state == TaskState::Failed)
            .count(),
        1
    );
    assert_eq!(
        after
            .tasks
            .values()
            .filter(|task| task.state == TaskState::Completed)
            .count(),
        1
    );
    assert_eq!(after.effective_limits, before.effective_limits);
    assert_eq!(
        after.effective_deadline_utc_ms,
        before.effective_deadline_utc_ms
    );
    assert_eq!(
        responses.calls.load(Ordering::SeqCst),
        3,
        "original response, unchanged sibling, changed input replacement"
    );
    assert_eq!(retained_marker_count(&fixture), 2);
    let ended = job.complete_run().unwrap();
    assert_eq!(job.complete_run().unwrap(), ended);
    let failed_key = after
        .tasks
        .values()
        .find(|task| task.state == TaskState::Failed)
        .unwrap()
        .spec
        .key
        .clone();
    let event = job.finish_rejected_remote_task(&failed_key).unwrap();
    assert_eq!(job.finish_rejected_remote_task(&failed_key).unwrap(), event);
    let repeated = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &resumed)
        .unwrap();
    complete(&repeated, 2);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
    assert_eq!(job.inspect().unwrap().last_event, after.last_event);
}

struct SecondTimeout {
    exposures: AtomicUsize,
    successes: Responses,
}
impl crate::providers::types::Transport for SecondTimeout {
    fn execute<'a>(
        &'a self,
        request: crate::providers::types::AuthenticatedRequest<'a>,
        context: crate::providers::types::TransportContext,
    ) -> crate::providers::types::TransportFuture<'a> {
        if self.exposures.fetch_add(1, Ordering::SeqCst) == 1 {
            Box::pin(async {
                Err(crate::providers::types::TransportFailure::new(
                    crate::providers::types::TransportFailureCode::Timeout,
                ))
            })
        } else {
            crate::providers::types::Transport::execute(&self.successes, request, context)
        }
    }
}
fn timeout_dispatch(
    fixture: &Fixture,
    transport: Arc<SecondTimeout>,
) -> crate::providers::dispatcher::Dispatcher {
    crate::providers::dispatcher::Dispatcher::new(
        fixture.fs.clone(),
        crate::providers::types::DispatchOptions {
            broker: Arc::new(crate::providers::credentials::CredentialBroker::new(
                crate::providers::credentials::CredentialOptions {
                    clock: Arc::new(common::TestClock),
                    inputs: Arc::new(crate::providers::credentials::NativeSecretInputs),
                    runner: Arc::new(crate::providers::credentials::NativeHelperRunner),
                },
            )),
            transport,
            jitter: Arc::new(NoBatchJitter),
        },
    )
}
fn two_distinct_guard_pages(fixture: &Fixture) {
    for (name, body) in [("a-terminal", FIRST), ("z-terminal", SECOND)] {
        fs::write(
            fixture.fs.root().path().join(format!("{name}.md")),
            page_bytes(&format!("page_{}", name.replace('-', "_")), None, body),
        )
        .unwrap();
    }
    fixture.app.index_sync(false).unwrap();
}

#[test]
fn inventory_automatic_guard_terminal_reconciled_attempt_keeps_run_budget_until_explicit_amendment()
{
    use crate::jobs::DispatcherLedgerApi;
    let fixture = normalized();
    two_distinct_guard_pages(&fixture);
    let transport = Arc::new(SecondTimeout {
        exposures: AtomicUsize::new(0),
        successes: Responses::new(),
    });
    let dispatch = timeout_dispatch(&fixture, transport.clone());
    let mut limited = runtime(&fixture.service, &dispatch);
    limited.limits.requests = 2;
    limited.limits.attempts_per_task = 2;
    let stopped = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &limited)
        .unwrap_err();
    assert_eq!(transport.exposures.load(Ordering::SeqCst), 2);
    let run: RecordId = serde_json::from_value(stopped.details["run_id"].clone()).unwrap();
    let job = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        run.clone(),
        options(),
    )
    .unwrap();
    let unknown = job.inspect().unwrap();
    assert_eq!(unknown.tasks.len(), 2);
    assert_eq!(unknown.attempts.len(), 2);
    assert_eq!(
        unknown
            .tasks
            .values()
            .filter(|task| task.state == TaskState::Completed)
            .count(),
        1
    );
    let unresolved = &unknown.attempts[1];
    assert_eq!(unresolved.phase, AttemptPhase::DispatchIntent);
    assert_eq!(unresolved.remote_exposure, RemoteExposure::PossiblyInFlight);
    assert!(unresolved.spool.is_none() && unresolved.receipt.is_none());
    // Use the actual reconciliation entry point; do not synthesize or rewrite
    // operational event bytes or fabricate a receipt/output.
    job.reconcile(
        &unresolved.attempt,
        true,
        KnownOrUnknown::Unknown,
        KnownOrUnknown::Unknown,
        "operator_terminal_confirmation",
    )
    .unwrap();
    let reconciled = job.inspect().unwrap();
    assert_eq!(reconciled.attempts[1].phase, AttemptPhase::DispatchIntent);
    assert_eq!(
        reconciled.attempts[1].remote_exposure,
        RemoteExposure::TerminalConfirmed
    );
    assert_eq!(
        reconciled.budget.unknown_attempts,
        unknown.budget.unknown_attempts
    );
    assert_eq!(reconciled.budget.outstanding, unknown.budget.outstanding);
    let without_retry = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &limited)
        .unwrap_err();
    assert_eq!(without_retry.code, ErrorCode::RecoveryRequired);
    assert_eq!(
        without_retry.details["run_id"],
        serde_json::to_value(&run).unwrap()
    );
    assert_eq!(transport.exposures.load(Ordering::SeqCst), 2);
    assert_eq!(retained_marker_count(&fixture), 1);
    assert_eq!(job.inspect().unwrap().attempts, reconciled.attempts);
    // The existing retry policy is still explicit after terminal confirmation.
    limited.job_options.policy.retry_uncertain = true;
    let (exhausted, work) = attribution::with_observation(|| {
        fixture.app.embeddings_sync(&ordinary_settings(), &limited)
    });
    let exhausted = exhausted.unwrap_err();
    assert_eq!(exhausted.code, ErrorCode::BudgetExceeded);
    assert_eq!(
        exhausted.details["run_id"],
        serde_json::to_value(&run).unwrap()
    );
    assert_eq!(work.prior_accounting_discovery_calls, 0);
    assert_eq!(transport.exposures.load(Ordering::SeqCst), 2);
    let blocked = job.inspect().unwrap();
    assert_eq!(blocked.state, RunState::Paused);
    assert_eq!(blocked.attempts, reconciled.attempts);
    assert_eq!(
        blocked.budget.unknown_attempts,
        reconciled.budget.unknown_attempts
    );
    assert_eq!(blocked.budget.outstanding, reconciled.budget.outstanding);
    assert_eq!(retained_marker_count(&fixture), 1);
    limited.limits.requests = 3;
    job.amend_retained_limits(
        limited.limits.clone(),
        blocked.effective_deadline_utc_ms,
        "Explicit additional exposure within the same reconciled Run".into(),
    )
    .unwrap();
    let (report, work) = attribution::with_observation(|| {
        fixture.app.embeddings_sync(&ordinary_settings(), &limited)
    });
    let report = report.unwrap();
    complete(&report, 2);
    assert_eq!(report.run_id, Some(run));
    assert_eq!(work.prior_accounting_discovery_calls, 0);
    assert_eq!(transport.exposures.load(Ordering::SeqCst), 3);
    assert_eq!(retained_marker_count(&fixture), 1);
    let after = job.inspect().unwrap();
    assert_eq!(after.attempts.len(), 3);
    assert_eq!(&after.attempts[..2], reconciled.attempts.as_slice());
    assert_eq!(
        after.attempts[2].attempt.task_key,
        unresolved.attempt.task_key
    );
    assert_eq!(after.attempts[2].attempt.number, 2);
    assert_eq!(after.budget.dispatched_requests, 3);
    assert_eq!(after.budget.unknown_attempts.len(), 3);
    assert_eq!(after.attempts[1].allowance, unresolved.allowance);
    assert!(after.attempts[1].receipt.is_none());
    cached_noop(&fixture, &ordinary_settings(), 2);
}

struct SecondBeforeSendCut {
    reached: AtomicUsize,
}
impl LedgerFault for SecondBeforeSendCut {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        if point == LedgerCheckpoint::BeforeSendAuthorityFlush
            && self.reached.fetch_add(1, Ordering::SeqCst) == 1
        {
            Err(WikiError::new(
                ErrorCode::Internal,
                "stop before the second send authority flush",
            ))
        } else {
            Ok(())
        }
    }
}

#[test]
fn inventory_automatic_guard_released_not_sent_retries_existing_run_without_receipt_lookup() {
    let fixture = normalized();
    two_distinct_guard_pages(&fixture);
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let mut interrupted = runtime(&fixture.service, &dispatch);
    interrupted.limits.requests = 2;
    interrupted.job_options.fault = Some(Arc::new(SecondBeforeSendCut {
        reached: AtomicUsize::new(0),
    }));
    let stopped = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &interrupted)
        .unwrap_err();
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
    let run: RecordId = serde_json::from_value(stopped.details["run_id"].clone()).unwrap();
    let job = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        run.clone(),
        options(),
    )
    .unwrap();
    let before = job.inspect().unwrap();
    assert_eq!(before.tasks.len(), 2);
    assert_eq!(before.attempts.len(), 2);
    assert_eq!(before.attempts[1].phase, AttemptPhase::Settled);
    assert_eq!(
        before.attempts[1].billing,
        BillingDisposition::ReleasedNotSent
    );
    assert!(before.attempts[1].receipt.is_none());
    assert_eq!(
        before
            .tasks
            .values()
            .filter(|task| task.state == TaskState::Completed)
            .count(),
        1
    );
    let mut resumed = runtime(&fixture.service, &dispatch);
    resumed.limits.requests = 2;
    let exhausted = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &resumed)
        .unwrap_err();
    assert_eq!(
        exhausted.code,
        ErrorCode::BudgetExceeded,
        "the original task attempt ceiling still applies"
    );
    assert_eq!(
        exhausted.details["run_id"],
        serde_json::to_value(&run).unwrap()
    );
    let blocked = job.inspect().unwrap();
    assert_eq!(blocked.state, RunState::Paused);
    assert_eq!(blocked.attempts, before.attempts);
    assert_eq!(blocked.budget.outstanding, before.budget.outstanding);
    assert_eq!(
        blocked.budget.unknown_attempts,
        before.budget.unknown_attempts
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
    assert_eq!(retained_marker_count(&fixture), 1);
    resumed.limits.attempts_per_task = 2;
    job.amend_retained_limits(
        resumed.limits.clone(),
        blocked.effective_deadline_utc_ms,
        "Explicit retry of the proven pre-send release".into(),
    )
    .unwrap();
    let report = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &resumed)
        .unwrap();
    complete(&report, 2);
    assert_eq!(report.run_id, Some(run));
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    assert_eq!(retained_marker_count(&fixture), 1);
    let after = job.inspect().unwrap();
    assert_eq!(&after.attempts[..2], before.attempts.as_slice());
    assert_eq!(after.attempts.len(), 3);
    assert_eq!(after.attempts[2].attempt.number, 2);
    assert_eq!(after.budget.dispatched_requests, 2);
    assert_eq!(after.budget.unknown_attempts.len(), 2);
    cached_noop(&fixture, &ordinary_settings(), 2);
}

#[test]
fn inventory_invocation_budget_129_owners_keeps_four_request_operations_and_amends_original_run() {
    let mut fixture = normalized();
    for index in 0..129 {
        fs::write(
            fixture
                .fs
                .root()
                .path()
                .join(format!("invocation-{index:03}.md")),
            page_bytes(
                &format!("page_invocation_{index:03}"),
                None,
                &format!(
                    "Distinct owner {index:03} stores {} violet tokens.\n",
                    index + 11
                ),
            ),
        )
        .unwrap();
    }
    fixture.app.index_sync(false).unwrap();
    let (dispatch, responses) = batch_dispatch(&mut fixture);
    let mut limited = runtime(&fixture.service, &dispatch);
    limited.limits.requests = 4;
    let error = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &limited)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert_eq!(error.details["budget_scope"], "invocation");
    assert!(
        error.details["next_action"]
            .as_str()
            .unwrap()
            .contains("Continue explicitly")
    );
    assert_eq!(error.hint.as_deref(), error.details["next_action"].as_str());
    assert_eq!(responses.calls.load(Ordering::SeqCst), 4);
    // New tasks retain at most 16 actual supplier owners, even though this
    // provider permits 32 items. The first page therefore declares eight tasks;
    // four requests acquire 64 distinct inputs, not the old hypothetical 128.
    assert_eq!(responses.items.load(Ordering::SeqCst), 64);
    assert_eq!(error.details["generated_inputs"], 64);
    assert_eq!(retained_marker_count(&fixture), 1);
    let run: RecordId = serde_json::from_value(error.details["run_id"].clone()).unwrap();
    let original = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        run,
        options(),
    )
    .unwrap();
    let before = original.inspect().unwrap();
    assert_eq!(before.state, RunState::Paused);
    assert_eq!(before.tasks.len(), 8);
    assert_eq!(before.attempts.len(), 4);
    assert_eq!(before.budget.dispatched_requests, 4);
    assert_eq!(before.budget.unknown_attempts.len(), 4);
    assert_eq!(before.spec.created_at_utc_ms, limited.created_at_utc_ms);
    assert_eq!(before.spec.deadline_utc_ms, limited.deadline_utc_ms);
    assert_eq!(before.effective_limits.requests, 4);
    assert_eq!(paid_inputs(&fixture, &before).len(), 128);
    assert_receipt_guards(&fixture, &before);

    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let policy =
        RenderPolicyId::for_settings(&reader.snapshot().parser_fingerprint, &ordinary_settings())
            .unwrap();
    let store = VectorStore::open(&fixture.fs, None).unwrap();
    let space = fixture.spec().id().unwrap();
    let incarnation = crate::retrieval::unit_inventory_types::catalog_incarnation(
        reader.vault_id(),
        reader.snapshot(),
    )
    .unwrap();
    assert!(
        store
            .preparation_cursor(&space, &policy, &incarnation)
            .unwrap()
            .is_none()
    );
    let bindings: Vec<_> = (0..129)
        .map(|index| {
            reader
                .unit_owner_binding(&policy, &rel(&format!("invocation-{index:03}.md")))
                .unwrap()
                .unwrap()
        })
        .collect();
    for binding in &bindings {
        assert!(
            !store.owner_binding_ready(&space, binding).unwrap(),
            "an incomplete paid scheduling page cannot acknowledge its owners"
        );
    }
    drop(store);
    drop(reader);

    // A new four-request operation cannot replenish the original Run's four
    // cumulative requests or create a replacement Run for its pending tasks.
    let repeated = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &limited)
        .unwrap_err();
    assert_eq!(repeated.code, ErrorCode::BudgetExceeded);
    assert_eq!(
        repeated.details["run_id"],
        serde_json::to_value(&before.spec.run_id).unwrap()
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 4);
    assert_eq!(retained_marker_count(&fixture), 1);
    let blocked = original.inspect().unwrap();
    assert_eq!(blocked.attempts, before.attempts);
    assert_eq!(
        blocked.budget.dispatched_requests,
        before.budget.dispatched_requests
    );
    assert_eq!(blocked.budget.outstanding, before.budget.outstanding);
    assert_eq!(
        blocked.budget.unknown_attempts,
        before.budget.unknown_attempts
    );

    let mut amended = blocked.effective_limits.clone();
    amended.requests = 8;
    original
        .amend_retained_limits(
            amended,
            blocked.effective_deadline_utc_ms,
            "Explicitly allow the existing first-page Run's four remaining supplier tasks".into(),
        )
        .unwrap();
    let after_amendment = original.inspect().unwrap();
    assert_eq!(after_amendment.spec, before.spec);
    assert_eq!(after_amendment.effective_limits.requests, 8);
    assert_eq!(after_amendment.attempts, before.attempts);
    assert_eq!(
        after_amendment.budget.outstanding,
        before.budget.outstanding
    );
    assert_eq!(
        after_amendment.budget.unknown_attempts,
        before.budget.unknown_attempts
    );

    // An explicit retained requests=4 override conflicts with the amended 8.
    // Refuse it before any dispatch; this is not a successful explicit CLI
    // --max-requests 4 continuation under an eight-request retained Run.
    limited.requested_limits = Some(crate::app::remote::RequestedJobLimits {
        limits: limited.limits.clone(),
        specified: std::collections::BTreeSet::from(["requests"]),
        deadline_ms: None,
    });
    let conflicting = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &limited)
        .unwrap_err();
    assert_eq!(conflicting.code, ErrorCode::Usage);
    assert_eq!(
        conflicting.details["reason"],
        "retained_limits_require_amendment"
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 4);
    let after_conflict = original.inspect().unwrap();
    assert_eq!(after_conflict.attempts, before.attempts);
    assert_eq!(after_conflict.budget.outstanding, before.budget.outstanding);
    assert_eq!(
        after_conflict.budget.unknown_attempts,
        before.budget.unknown_attempts
    );

    // Public API composition control: the fixture's operation-default request
    // allowance stays 4, with no explicit override of the retained Run's amended 8.
    // The independent invocation budget must span recovery and the next page.
    limited.requested_limits.as_mut().unwrap().specified.clear();
    assert_eq!(limited.limits.requests, 4);
    let continued = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &limited)
        .unwrap_err();
    assert_eq!(continued.code, ErrorCode::BudgetExceeded);
    assert_eq!(continued.details["budget_scope"], "invocation");
    assert_eq!(continued.details["generated_inputs"], 64);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 8);
    assert_eq!(responses.items.load(Ordering::SeqCst), 128);
    assert_eq!(retained_marker_count(&fixture), 2);
    let completed = original.inspect().unwrap();
    assert_eq!(completed.state, RunState::Completed);
    assert_eq!(completed.spec, before.spec);
    assert_eq!(&completed.attempts[..4], before.attempts.as_slice());
    assert_eq!(completed.attempts.len(), 8);
    assert_eq!(completed.budget.dispatched_requests, 8);
    assert_eq!(completed.budget.unknown_attempts.len(), 8);
    assert_receipt_guards(&fixture, &completed);
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let store = VectorStore::open(&fixture.fs, None).unwrap();
    for binding in &bindings[..128] {
        assert_eq!(
            reader
                .unit_owner_binding(&policy, &binding.owner)
                .unwrap()
                .as_ref(),
            Some(binding)
        );
        assert!(store.owner_binding_ready(&space, binding).unwrap());
    }
    assert!(!store.owner_binding_ready(&space, &bindings[128]).unwrap());
    let prefix = store
        .preparation_cursor(&space, &policy, &incarnation)
        .unwrap()
        .unwrap();
    assert!(!prefix.complete);
    assert_eq!(prefix.after.as_ref().unwrap().owner, bindings[127].owner);
    drop(store);
    drop(reader);
    let pending_run: RecordId =
        serde_json::from_value(continued.details["run_id"].clone()).unwrap();
    assert_ne!(pending_run, before.spec.run_id);
    let pending = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        pending_run,
        options(),
    )
    .unwrap();
    let pending_before = pending.inspect().unwrap();
    assert_eq!(pending_before.state, RunState::Paused);
    assert_eq!(pending_before.tasks.len(), 1);
    assert!(pending_before.attempts.is_empty());
    assert_eq!(pending_before.budget.dispatched_requests, 0);
    assert_eq!(pending_before.effective_limits.requests, 4);
    let report = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &limited)
        .unwrap();
    complete(&report, 129);
    assert_eq!(report.generated_inputs, 1);
    assert_eq!(report.run_id.as_ref(), Some(&pending_before.spec.run_id));
    assert_eq!(responses.calls.load(Ordering::SeqCst), 9);
    assert_eq!(responses.items.load(Ordering::SeqCst), 129);
    assert_eq!(retained_marker_count(&fixture), 2);
    let after = pending.inspect().unwrap();
    assert_eq!(after.state, RunState::Completed);
    assert_eq!(after.spec, pending_before.spec);
    assert_eq!(after.attempts.len(), 1);
    assert_eq!(after.budget.dispatched_requests, 1);
    assert_eq!(after.budget.unknown_attempts.len(), 1);
    assert_eq!(original.inspect().unwrap().attempts, completed.attempts);
    assert_receipt_guards(&fixture, &after);
    cached_noop(&fixture, &ordinary_settings(), 129);
}

struct AfterCommittedCut {
    fired: AtomicBool,
}
impl LedgerFault for AfterCommittedCut {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        if point == LedgerCheckpoint::AfterOutputsCommitted
            && !self.fired.swap(true, Ordering::SeqCst)
        {
            return Err(WikiError::new(
                ErrorCode::Internal,
                "cut after committed validated receipt",
            ));
        }
        Ok(())
    }
}
struct SecondReceivedCut {
    count: AtomicUsize,
}
impl LedgerFault for SecondReceivedCut {
    fn check(&self, point: LedgerCheckpoint) -> Result<()> {
        if point == LedgerCheckpoint::AfterReceived
            && self.count.fetch_add(1, Ordering::SeqCst) == 1
        {
            return Err(WikiError::new(
                ErrorCode::Internal,
                "cut after first task completed",
            ));
        }
        Ok(())
    }
}
#[test]
fn inventory_mixed_stale_validated_committed_and_completed_history_continue_siblings() {
    for completed in [false, true] {
        let fixture = normalized();
        for (path, id, body) in [
            ("a-history.md", "page_a_history", FIRST),
            ("z-history.md", "page_z_history", SECOND),
        ] {
            fs::write(
                fixture.fs.root().path().join(path),
                page_bytes(id, None, body),
            )
            .unwrap();
        }
        fixture.app.index_sync(false).unwrap();
        let responses = Arc::new(Responses::new());
        let dispatch = dispatcher(&fixture.fs, responses.clone());
        let mut interrupted = runtime(&fixture.service, &dispatch);
        interrupted.job_options.fault = Some(if completed {
            Arc::new(SecondReceivedCut {
                count: AtomicUsize::new(0),
            }) as Arc<dyn LedgerFault>
        } else {
            Arc::new(AfterCommittedCut {
                fired: AtomicBool::new(false),
            }) as Arc<dyn LedgerFault>
        });
        let stopped = fixture
            .app
            .embeddings_sync(&ordinary_settings(), &interrupted)
            .unwrap_err();
        let run = serde_json::from_value(stopped.details["run_id"].clone()).unwrap();
        let job = JobLedger::new(
            fixture.fs.clone(),
            fixture.app.vault_id().clone(),
            run,
            options(),
        )
        .unwrap();
        let before = job.inspect().unwrap();
        let paid = &before.attempts[0];
        assert_eq!(
            paid.phase,
            if completed {
                AttemptPhase::Settled
            } else {
                AttemptPhase::OutputCommitted
            }
        );
        let task = &before.tasks[&paid.attempt.task_key].spec;
        let supplier = task
            .source_bindings
            .iter()
            .find(|guard| guard.path == rel("a-history.md") || guard.path == rel("z-history.md"))
            .unwrap();
        let id = if supplier.path == rel("a-history.md") {
            "page_a_history"
        } else {
            "page_z_history"
        };
        let receipt = paid.receipt.clone();
        fs::write(
            fixture.fs.root().path().join(supplier.path.as_str()),
            page_bytes(
                id,
                None,
                "Changed historical owner stores 83 copper tokens.\n",
            ),
        )
        .unwrap();
        fixture.app.index_sync(false).unwrap();
        let resumed = runtime(&fixture.service, &dispatch);
        let report = fixture
            .app
            .embeddings_sync(&ordinary_settings(), &resumed)
            .unwrap();
        complete(&report, 2);
        let after = job.inspect().unwrap();
        assert_eq!(after.state, RunState::Completed);
        assert_eq!(after.attempts.len(), 2);
        assert_eq!(after.attempts[0].attempt, paid.attempt);
        assert_eq!(after.attempts[0].receipt, receipt);
        assert_eq!(after.effective_limits, before.effective_limits);
        assert_eq!(
            after.effective_deadline_utc_ms,
            before.effective_deadline_utc_ms
        );
        assert_eq!(after.budget.unknown_attempts.len(), 2);
        assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
        fixture
            .app
            .embeddings_sync(&ordinary_settings(), &resumed)
            .unwrap();
        assert_eq!(responses.calls.load(Ordering::SeqCst), 3);
    }
}

#[test]
fn inventory_mixed_stale_budget_pause_preserves_siblings_for_same_run_continuation() {
    let fixture = normalized();
    for index in 0..3 {
        fs::write(
            fixture
                .fs
                .root()
                .path()
                .join(format!("stale-budget-{index}.md")),
            page_bytes(
                &format!("page_stale_budget_{index}"),
                None,
                &format!(
                    "Distinct stale budget owner {index} holds {} amber tokens.\n",
                    index + 31
                ),
            ),
        )
        .unwrap();
    }
    fixture.app.index_sync(false).unwrap();
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let mut interrupted = runtime(&fixture.service, &dispatch);
    interrupted.job_options.fault = Some(Arc::new(ReceivedOnce {
        armed: AtomicBool::new(true),
    }));
    let stopped = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &interrupted)
        .unwrap_err();
    let run = serde_json::from_value(stopped.details["run_id"].clone()).unwrap();
    let job = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        run,
        options(),
    )
    .unwrap();
    let before = job.inspect().unwrap();
    assert_eq!(before.tasks.len(), 3);
    let first = &before.tasks[&before.attempts[0].attempt.task_key].spec;
    let supplier = first
        .source_bindings
        .iter()
        .find(|guard| guard.path.as_str().starts_with("stale-budget-"))
        .unwrap();
    let supplier_index: usize = supplier
        .path
        .as_str()
        .trim_start_matches("stale-budget-")
        .trim_end_matches(".md")
        .parse()
        .unwrap();
    fs::write(
        fixture.fs.root().path().join(supplier.path.as_str()),
        page_bytes(
            &format!("page_stale_budget_{supplier_index}"),
            None,
            "Changed stale budget owner holds 97 copper tokens.\n",
        ),
    )
    .unwrap();
    fixture.app.index_sync(false).unwrap();
    let mut limited = runtime(&fixture.service, &dispatch);
    limited.limits.requests = 1;
    // No retained override is requested; this is an independent invocation
    // envelope around inherited Run authority, as for default CLI limits.
    limited.requested_limits = Some(crate::app::remote::RequestedJobLimits {
        limits: limited.limits.clone(),
        specified: Default::default(),
        deadline_ms: None,
    });
    let stopped = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &limited)
        .unwrap_err();
    assert_eq!(stopped.code, ErrorCode::BudgetExceeded);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    let paused = job.inspect().unwrap();
    assert_eq!(paused.state, RunState::Paused);
    assert_eq!(paused.effective_limits, before.effective_limits);
    assert_eq!(paused.spec, before.spec);
    assert_eq!(paused.attempts.len(), 2);
    assert_eq!(
        paused
            .tasks
            .values()
            .filter(|task| task.state == TaskState::Failed)
            .count(),
        1
    );
    assert_eq!(
        paused
            .tasks
            .values()
            .filter(|task| task.state == TaskState::Completed)
            .count(),
        1
    );
    assert_eq!(
        paused
            .tasks
            .values()
            .filter(|task| task.state == TaskState::Pending)
            .count(),
        1
    );
    assert_eq!(retained_marker_count(&fixture), 1);
    let resumed = runtime(&fixture.service, &dispatch);
    let report = fixture
        .app
        .embeddings_sync(&ordinary_settings(), &resumed)
        .unwrap();
    complete(&report, 3);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 4);
    let after = job.inspect().unwrap();
    assert_eq!(after.state, RunState::Failed);
    assert_eq!(after.attempts.len(), 3);
    assert_eq!(after.attempts[0].attempt, before.attempts[0].attempt);
    assert_eq!(after.budget.unknown_attempts.len(), 3);
    assert_eq!(after.spec, before.spec);
    assert_eq!(retained_marker_count(&fixture), 2);
}

#[test]
fn supplier_packing_bounds_owners_without_splitting_multiunit_owner_unnecessarily() {
    use crate::changes::ReadDependency;
    use crate::retrieval::render::{RenderedUnit, TargetKind};
    use crate::vault::ExpectedState;
    let mut fixture = normalized();
    let (dispatch, responses) = batch_dispatch(&mut fixture);
    let runtime = runtime(&fixture.service, &dispatch);
    let units: Vec<_> = (0..32)
        .map(|index| {
            let text = format!("supplier {index}");
            RenderedUnit {
                unit_id: Blake3Hash::digest(format!("unit{index}")),
                owner: rel(&format!("supplier-{index:02}.md")),
                target: TargetKind::Document,
                target_id: None,
                source_hash: Blake3Hash::digest(&text),
                source_span: None,
                dependency_fingerprint: Blake3Hash::digest(b"proof"),
                input_hash: Blake3Hash::digest(&text),
                utf8: text,
            }
        })
        .collect();
    let inputs: Vec<_> = units.iter().map(RenderedUnit::input).collect();
    let bindings: BTreeMap<_, _> = units
        .iter()
        .map(|unit| {
            (
                unit.input_hash.clone(),
                vec![ReadDependency {
                    path: unit.owner.clone(),
                    expected: ExpectedState::Absent,
                }],
            )
        })
        .collect();
    for count in [15, 16, 17] {
        let batches = fixture
            .app
            .embedding_supplier_batches(&runtime, &inputs[..count], &bindings, &units[..count])
            .unwrap();
        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            if count <= 16 {
                vec![count]
            } else {
                vec![16, 1]
            }
        );
    }
    let one_owner: Vec<_> = units
        .iter()
        .cloned()
        .map(|mut unit| {
            unit.owner = rel("one-owner.md");
            unit
        })
        .collect();
    assert_eq!(
        fixture
            .app
            .embedding_supplier_batches(&runtime, &inputs, &bindings, &one_owner)
            .unwrap()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![32]
    );
    let mut ambiguous = units[..1].to_vec();
    let mut duplicate = ambiguous[0].clone();
    duplicate.owner = rel("duplicate.md");
    ambiguous.push(duplicate);
    assert_eq!(
        fixture
            .app
            .embedding_supplier_batches(&runtime, &inputs[..1], &bindings, &ambiguous)
            .err()
            .expect("ambiguous supplier must be refused")
            .code,
        ErrorCode::FreshnessConflict
    );
    assert_eq!(responses.calls.load(Ordering::SeqCst), 0);
}
