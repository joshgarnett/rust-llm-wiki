//! Disposable ordinary cache maintenance workflows; no native child or live provider.
#[path = "../../tests/fixtures/p17/common.rs"]
mod common;
use crate::{
    catalog::{
        query_types::{QueryCatalog, QueryReadLimits},
        Catalog,
    },
    domain::*,
    jobs::{JobLedger, JobLedgerApi},
    retrieval::{
        discovery::DiscoveryManifest, spaces::EmbeddingSettings, types::*,
        unit_inventory_types::RenderPolicyId, vectors::VectorStore, ContextRequest, ContextScope,
    },
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
};
use common::{dispatcher, options, runtime, Fixture, Responses};
use std::sync::{atomic::Ordering, Arc};
const TITLE: &str = "Retained capture title";
const FIRST: &str = "Signalneedle stores 17 amber tokens. Unicode café 東京 🦀.\n";
const SECOND: &str = "Signalneedle stores 29 violet tokens. Unicode café 東京 🦀.\n";
fn capture(origin: &str, body: &str) -> CaptureRequest {
    CaptureRequest {
        title: TITLE.into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: origin.into(),
        original: body.as_bytes().to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: None,
    }
}
fn add(f: &Fixture, origin: &str, body: &str) -> (RecordId, RecordId) {
    let report = f.app.source_add(capture(origin, body)).unwrap();
    (
        report.allocated_ids["source"].clone(),
        report.allocated_ids["revision"].clone(),
    )
}
fn plan(source: Option<&RecordId>) -> QueryPlan {
    QueryPlan {
        mode: SearchMode::Semantic,
        filters: SearchFilters {
            source_ids: source.cloned().into_iter().collect(),
            ..Default::default()
        },
        limits: SearchLimits {
            hits: 8,
            candidates: 8,
            excerpt_bytes: 256,
        },
        cursor: None,
    }
}
fn selected(f: &Fixture, space: &Blake3Hash) -> DiscoveryManifest {
    let catalog = Catalog::new(f.fs.clone(), f.app.vault_id().clone());
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let policy = RenderPolicyId::for_settings(
        &reader.snapshot().parser_fingerprint,
        &EmbeddingSettings::default(),
    )
    .unwrap();
    reader
        .discovery_state(space, &policy)
        .unwrap()
        .unwrap()
        .manifest
        .unwrap()
}
fn ready(f: &Fixture) {
    let report = f
        .offline()
        .embeddings_sync_cached(&EmbeddingSettings::default())
        .unwrap();
    assert!(!report.network_used);
    assert_eq!(report.generated_inputs, 0);
    assert_eq!(report.coverage.missing_units, 0);
    assert_eq!(report.discovery.unwrap()["published"], true);
}
fn exact(f: &Fixture, source: &RecordId, revision: &RecordId, body: &str) {
    let offline = f.offline();
    let request = plan(Some(source));
    let hits = offline
        .semantic_search_selected("Signalneedle", &request, None, false)
        .unwrap();
    assert!(!hits.network_used);
    assert_eq!(hits.hits.len(), 1);
    let hit = &hits.hits[0];
    assert_eq!(hit.source_id.as_ref(), Some(source));
    assert_eq!(hit.owner_revision.as_ref(), Some(revision));
    let path = VaultRelativePath::new(format!("sources/{source}/revisions/{revision}/content.md"))
        .unwrap();
    assert_eq!(hit.locator.path, path);
    for excerpt in std::iter::once(&hit.excerpt).chain(&hit.secondary_excerpts) {
        let slice = body
            .get(excerpt.span.start() as usize..excerpt.span.end() as usize)
            .unwrap();
        assert_eq!(slice, excerpt.text);
        let Some(CitationRef::Source(reference)) = &excerpt.citation else {
            panic!("Source citation required")
        };
        assert_eq!(&reference.source_id, source);
        assert_eq!(&reference.source_revision, revision);
        assert_eq!(reference.quote_hash, Blake3Hash::digest(slice.as_bytes()));
    }
    let context = offline
        .semantic_context(
            "Signalneedle",
            &ContextRequest {
                scope: ContextScope::IndexedDocuments,
                documents: request,
                ..Default::default()
            },
            None,
            true,
            false,
        )
        .unwrap();
    assert!(!context.network_used);
    assert!(!context.passages().is_empty());
    for passage in context.passages() {
        assert_eq!(passage.locator.path, path);
        assert_eq!(
            body.get(passage.span.start() as usize..passage.span.end() as usize),
            Some(passage.text.as_str())
        );
        assert!(!passage.citations.is_empty());
        for citation in &passage.citations {
            let CitationRef::Source(reference) = citation else {
                panic!("Source citation required")
            };
            assert_eq!(&reference.source_id, source);
            assert_eq!(&reference.source_revision, revision);
            assert_eq!(
                reference.quote_hash,
                Blake3Hash::digest(passage.text.as_bytes())
            );
        }
    }
}

#[test]
fn cache_only_delta_source_refresh_and_offline_catalog_rebuild_preserve_paid_vectors_and_citations()
{
    let fixture = Fixture::new();
    fixture.app.index_rebuild_normalized().unwrap();
    let (changing, old_revision) = add(&fixture, "changing.txt", FIRST);
    let (donor, donor_revision) = add(&fixture, "donor.txt", SECOND);
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    let initial = fixture
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &runtime)
        .unwrap();
    assert_eq!(initial.generated_inputs, 2);
    let space = initial.space.clone().unwrap();
    let ledger = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        initial.run_id.clone().unwrap(),
        options(),
    )
    .unwrap();
    let paid = ledger.inspect().unwrap();
    let refs: Vec<_> = paid
        .attempts
        .iter()
        .flat_map(|a| a.cache_outputs.clone())
        .collect();
    assert_eq!(refs.len(), 2);
    fixture.query_seed(&fixture.spec(), "Signalneedle", vec![1., 0.]);
    ready(&fixture);
    let first_manifest = selected(&fixture, &space);
    assert!(first_manifest.base.is_none());
    assert_eq!(first_manifest.delta.as_ref().unwrap().node_count, 2);
    exact(&fixture, &changing, &old_revision, FIRST);
    exact(&fixture, &donor, &donor_revision, SECOND);
    let hits = fixture
        .offline()
        .semantic_search_selected("Signalneedle", &plan(None), None, false)
        .unwrap();
    assert_eq!(
        hits.hits.len(),
        2,
        "unfiltered delta discovery must find both owners"
    );

    let refreshed = fixture
        .app
        .source_refresh(changing.clone(), capture("changing.txt", SECOND))
        .unwrap();
    let revision = refreshed.allocated_ids["revision"].clone();
    assert_ne!(revision, old_revision);
    assert_eq!(
        fixture
            .offline()
            .semantic_search_selected("Signalneedle", &plan(Some(&changing)), None, false)
            .unwrap_err()
            .code,
        ErrorCode::FreshnessConflict
    );
    ready(&fixture);
    exact(&fixture, &changing, &revision, SECOND);
    let updated = selected(&fixture, &space);
    assert!(updated.through_seq > first_manifest.through_seq);
    assert!(updated.base.is_none());
    let hits = fixture
        .offline()
        .semantic_search_selected("Signalneedle", &plan(None), None, false)
        .unwrap();
    assert_eq!(
        hits.hits.len(),
        2,
        "shared retained input remains independently discoverable per owner"
    );
    assert!(hits
        .hits
        .iter()
        .all(|h| h.owner_revision.as_ref() != Some(&old_revision)));

    // Rebuild removes the disposable unit and discovery inventories.
    // Cache-only preparation recreates it under the new catalog incarnation.
    fixture.offline().index_rebuild_normalized().unwrap();
    assert_eq!(
        fixture
            .offline()
            .semantic_search_selected("Signalneedle", &plan(None), None, false)
            .unwrap_err()
            .code,
        ErrorCode::OfflineUnavailable
    );
    ready(&fixture);
    let rebuilt = selected(&fixture, &space);
    assert_ne!(rebuilt.incarnation, updated.incarnation);
    exact(&fixture, &changing, &revision, SECOND);
    exact(&fixture, &donor, &donor_revision, SECOND);
    assert_eq!(responses.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        ledger.inspect().unwrap().budget.dispatched_requests,
        paid.budget.dispatched_requests
    );
    let store = VectorStore::open(&fixture.fs, None).unwrap();
    for reference in refs {
        assert!(store.verify_ref(&reference).unwrap());
    }
}

#[test]
fn corrupt_selected_delta_fails_unfiltered_search_without_changing_paid_cache_or_selection() {
    let fixture = Fixture::new();
    fixture.app.index_rebuild_normalized().unwrap();
    add(&fixture, "changing.txt", FIRST);
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    let initial = fixture
        .app
        .embeddings_sync(&EmbeddingSettings::default(), &runtime)
        .unwrap();
    let space = initial.space.clone().unwrap();
    let ledger = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        initial.run_id.unwrap(),
        options(),
    )
    .unwrap();
    let refs: Vec<_> = ledger
        .inspect()
        .unwrap()
        .attempts
        .iter()
        .flat_map(|a| a.cache_outputs.clone())
        .collect();
    fixture.query_seed(&fixture.spec(), "Signalneedle", vec![1., 0.]);
    ready(&fixture);
    let manifest = selected(&fixture, &space);
    let graph = manifest.delta.as_ref().unwrap();
    let hash = graph.root.as_str().strip_prefix("blake3:").unwrap();
    let path = fixture.fs.root().path().join(format!(
        ".wiki/cache/semantic/objects/{}/{}",
        &hash[..2],
        hash
    ));
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 1;
    std::fs::write(path, bytes).unwrap();
    assert_eq!(
        fixture
            .offline()
            .semantic_search_selected("Signalneedle", &plan(None), None, false)
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
    assert_eq!(selected(&fixture, &space), manifest);
    let store = VectorStore::open(&fixture.fs, None).unwrap();
    for reference in refs {
        assert!(store.verify_ref(&reference).unwrap());
    }
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn malformed_interrupted_stage_refuses_cache_sync_and_preserves_selected_manifest_and_paid_vectors()
{
    use crate::retrieval::unit_inventory_types::catalog_incarnation;
    let fixture = Fixture::new();
    fixture.app.index_rebuild_normalized().unwrap();
    let (source, _) = add(&fixture, "changing.txt", FIRST);
    let responses = Arc::new(Responses::new());
    let dispatch = dispatcher(&fixture.fs, responses.clone());
    let runtime = runtime(&fixture.service, &dispatch);
    let settings = EmbeddingSettings::default();
    let initial = fixture.app.embeddings_sync(&settings, &runtime).unwrap();
    let space = initial.space.clone().unwrap();
    let ledger = JobLedger::new(
        fixture.fs.clone(),
        fixture.app.vault_id().clone(),
        initial.run_id.unwrap(),
        options(),
    )
    .unwrap();
    let refs: Vec<_> = ledger
        .inspect()
        .unwrap()
        .attempts
        .iter()
        .flat_map(|a| a.cache_outputs.clone())
        .collect();
    ready(&fixture);
    let before = selected(&fixture, &space);
    fixture
        .app
        .source_refresh_with_title(
            source,
            capture("changing.txt", FIRST),
            Some("Changed mutable title"),
        )
        .unwrap();
    let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
    {
        let writer = fixture.writer();
        let inventory = catalog
            .prepare_unit_inventory_page(&writer, &settings, 128)
            .unwrap();
        assert!(inventory.complete);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let incarnation = catalog_incarnation(reader.vault_id(), reader.snapshot()).unwrap();
        drop(reader);
        let state = catalog
            .register_discovery(&writer, &space, &inventory.policy)
            .unwrap();
        // Deliberate corruption of a private interrupted continuation, never a
        // canonical source or paid vector. Public cached sync must reject it.
        catalog
            .save_discovery_stage(
                &writer,
                &space,
                &inventory.policy,
                &incarnation,
                state.required_seq,
                &serde_json::json!({"interrupted_stage":"malformed"}),
            )
            .unwrap();
    }
    assert_eq!(
        fixture
            .offline()
            .embeddings_sync_cached(&settings)
            .unwrap_err()
            .code,
        ErrorCode::IndexCorrupt
    );
    assert_eq!(selected(&fixture, &space), before);
    let store = VectorStore::open(&fixture.fs, None).unwrap();
    for reference in refs {
        assert!(store.verify_ref(&reference).unwrap());
    }
    assert_eq!(responses.calls.load(Ordering::SeqCst), 1);
}
