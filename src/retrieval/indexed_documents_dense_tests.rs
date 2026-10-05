//! Mock vectors validate cache/proof mechanics only. No provider compatibility
//! or answer-quality claim follows from these fixtures.
use super::*;
use crate::{
    app::{OfflineApp, OperationOptions, offline},
    retrieval::spaces::{EmbeddingSettings, RENDER_VERSION, SpaceSpec},
    sources::{CaptureRequest, CitationScope, ExtractionInput, SourceOrigin, SourceView},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use std::time::Duration;
const QUERY: &str = "recover archive";
const HIDDEN: &str =
    "The restoration procedure for frozen storage requires a violet permit. Café 東京.\n";
fn capture(title: &str, body: &str) -> CaptureRequest {
    CaptureRequest {
        title: title.into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: format!("{title}.txt"),
        original: body.as_bytes().to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: Some("text/plain".into()),
    }
}
fn spec() -> SpaceSpec {
    SpaceSpec {
        version: 1,
        endpoint_fingerprint: Blake3Hash::digest(b"mock endpoint"),
        profile_id: "mechanics".into(),
        service_id: "mock".into(),
        model: "mock fixed vectors".into(),
        revision: None,
        dimensions: Some(2),
        metric: "cosine".into(),
        normalization: "float64-l2-to-f32-le-v1".into(),
        render_version: RENDER_VERSION.into(),
        settings: EmbeddingSettings {
            max_input_bytes: 512,
            quality_target_bytes: Some(256),
            ..Default::default()
        },
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    app: OfflineApp,
    catalog: Catalog,
    hidden: RecordId,
    hidden_path: VaultRelativePath,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("vault");
        offline::init(&root, "Dense mechanics", OperationOptions::default()).unwrap();
        let app = OfflineApp::new(
            VaultFs::new(VaultRoot::explicit(&root).unwrap()),
            OperationOptions::default(),
        )
        .unwrap();
        let outcome = app
            .source_add(capture("Restoration procedure", HIDDEN))
            .unwrap();
        let hidden = outcome.allocated_ids["source"].clone();
        let revision = outcome.allocated_ids["revision"].clone();
        let hidden_path =
            VaultRelativePath::new(format!("sources/{hidden}/revisions/{revision}/content.md"))
                .unwrap();
        app.source_add(capture(
            "Unhelpful wording",
            "recover archive indexing has no permit instructions.\n",
        ))
        .unwrap();
        let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
        let writer = WriterPermit::acquire(app.fs().root(), Duration::from_secs(1)).unwrap();
        let legacy = catalog.canonical_snapshot().unwrap();
        let units = render::corpus(&legacy, &spec().settings).unwrap();
        Self::publish(&catalog, &writer, legacy.snapshot(), &units, &hidden_path);
        catalog.rebuild_normalized(&writer).unwrap();
        drop(writer);
        Self {
            _temp: temp,
            app,
            catalog,
            hidden,
            hidden_path,
        }
    }
    fn publish(
        catalog: &Catalog,
        writer: &WriterPermit,
        snapshot: &ReadSnapshot,
        units: &[render::RenderedUnit],
        hidden: &VaultRelativePath,
    ) {
        let mut store = vectors::VectorStore::open(catalog.fs(), Some(writer)).unwrap();
        let space = store.prepare_space(&spec()).unwrap();
        let inputs = units
            .iter()
            .map(|unit| unit.input_hash.clone())
            .collect::<Vec<_>>();
        let values = units
            .iter()
            .map(|unit| {
                if &unit.owner == hidden {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect::<Vec<_>>();
        store
            .put_batch(
                &space,
                &inputs,
                &values,
                true,
                &Blake3Hash::digest(b"diagnostic mock preparation"),
            )
            .unwrap();
        let query = spec().query(QUERY).unwrap();
        store
            .put_batch(
                &space,
                &[query.input_hash],
                &[vec![1.0, 0.0]],
                false,
                &Blake3Hash::digest(b"diagnostic query cache"),
            )
            .unwrap();
        store.memberships(&space, snapshot, units, true).unwrap();
    }
    fn run(&self, arm: &str) -> (Value, Diagnostics) {
        let mut stats = Diagnostics::default();
        let result = run(
            &self.catalog,
            QUERY,
            &pinned_request(),
            arm,
            false,
            &mut stats,
            || Ok(()),
        )
        .unwrap();
        (result, stats)
    }
    /// Explicit test-only selected rerender refreshes mock retained bindings.
    /// This is NOT a successful public normalized embeddings sync.
    fn diagnostic_reprepare(&self) {
        let reader = self
            .catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let mut statement = reader.connection().prepare("SELECT path FROM documents WHERE owner_revision IS NOT NULL AND eligibility='current' ORDER BY path").unwrap();
        let paths = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .map(|path| VaultRelativePath::new(path).unwrap())
            .collect::<Vec<_>>();
        drop(statement);
        let proof = selected_documents::authenticate(
            &self.catalog,
            &reader,
            &paths,
            &pinned_request().verification_budget,
        )
        .unwrap();
        let mut units = Vec::new();
        for document in proof.documents.values() {
            units.extend(
                render::render_selected_document_for_test(
                    document,
                    &spec().settings,
                    proof.fingerprint.clone(),
                )
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap(),
            );
        }
        let writer =
            WriterPermit::acquire(self.catalog.fs().root(), Duration::from_secs(1)).unwrap();
        let hidden = paths
            .iter()
            .find(|path| path.as_str().contains(self.hidden.as_str()))
            .unwrap_or(&self.hidden_path);
        Self::publish(&self.catalog, &writer, reader.snapshot(), &units, hidden);
    }
    fn cache(&self) -> PathBuf {
        self.catalog
            .fs()
            .root()
            .path()
            .join(".wiki/cache/embeddings.sqlite3")
    }
}
fn current_citations(fixture: &Fixture, payload: &Value) -> Vec<CitationRef> {
    let view = SourceView::from_fs(fixture.catalog.fs()).unwrap();
    let mut citations = Vec::new();
    for passage in payload["passages"].as_array().unwrap() {
        for value in passage["citations"].as_array().unwrap() {
            let citation: CitationRef = serde_json::from_value(value.clone()).unwrap();
            let verified = view.verify(&citation, CitationScope::Current).unwrap();
            assert_eq!(verified.quote, passage["text"].as_str().unwrap().as_bytes());
            citations.push(citation);
        }
    }
    citations
}

#[test]
fn dense_mechanics_01_paraphrased_discovery_exact_citations() {
    let fixture = Fixture::new();
    let (lexical, _) = fixture.run("L");
    assert!(!lexical["text"].as_str().unwrap().contains("violet permit"));
    for arm in ["S", "H"] {
        let (payload, stats) = fixture.run(arm);
        assert!(payload["text"].as_str().unwrap().contains("violet permit"));
        assert_eq!(payload["network_used"], false);
        assert!(stats.ranked_unit_selection_used);
        assert!(stats.selected_units_scored > 0 && stats.retained_work.vector_bytes_scanned > 0);
        assert!(stats.retained_work.metadata_rows_decoded <= 4096);
        assert!(payload["usage"]["rendered_bytes"].as_u64().unwrap() <= 6000);
        assert!(!current_citations(&fixture, &payload).is_empty());
    }
}
#[test]
fn dense_mechanics_02_title_and_content_refresh_reprepare() {
    let fixture = Fixture::new();
    let before_reader = fixture
        .catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let before = selected_documents::authenticate(
        &fixture.catalog,
        &before_reader,
        &[fixture.hidden_path.clone()],
        &pinned_request().verification_budget,
    )
    .unwrap();
    let before_document = before.documents[&fixture.hidden_path].clone();
    let revision = before_document.owner_revision.clone().unwrap();
    let settings = spec().settings;
    let before_units = render::render_selected_document_for_test(
        &before_document,
        &settings,
        before.fingerprint.clone(),
    )
    .unwrap()
    .collect::<Result<Vec<_>>>()
    .unwrap();
    let refreshed = fixture
        .app
        .source_refresh_with_title(
            fixture.hidden.clone(),
            capture("Restoration procedure", HIDDEN),
            Some("Changed restoration title"),
        )
        .unwrap();
    assert!(refreshed.reused);
    assert_eq!(refreshed.allocated_ids["revision"], revision);
    let after_reader = fixture
        .catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let after = selected_documents::authenticate(
        &fixture.catalog,
        &after_reader,
        &[fixture.hidden_path.clone()],
        &pinned_request().verification_budget,
    )
    .unwrap();
    assert_eq!(
        after.records[&fixture.hidden].record.title(),
        "Changed restoration title"
    );
    assert_ne!(
        before.records[&fixture.hidden].record.title(),
        after.records[&fixture.hidden].record.title()
    );
    assert_eq!(
        before.records[&revision].record.title(),
        after.records[&revision].record.title()
    );
    assert_eq!(
        before.records[&revision].hash,
        after.records[&revision].hash
    );
    let after_document = &after.documents[&fixture.hidden_path];
    assert_eq!(after_document.owner_revision.as_ref(), Some(&revision));
    assert_eq!(before_document.title, after_document.title);
    assert_eq!(before_document.hash, after_document.hash);
    assert_eq!(before_document.raw_text, after_document.raw_text);
    let after_units = render::render_selected_document_for_test(
        after_document,
        &settings,
        after.fingerprint.clone(),
    )
    .unwrap()
    .collect::<Result<Vec<_>>>()
    .unwrap();
    assert_eq!(before_units.len(), after_units.len());
    for (old, current) in before_units.iter().zip(&after_units) {
        assert_eq!(old.unit_id, current.unit_id);
        assert_eq!(old.input_hash, current.input_hash);
        assert_eq!(old.utf8, current.utf8);
        render::require_selected_unit_agreement(old, current).unwrap();
    }
    // Diagnostic clone tests a genuinely changed rendered title without
    // rewriting an immutable Revision or claiming the clone is evidence.
    let mut changed_title = after_document.clone();
    changed_title.title = "Diagnostic revised embedding header".into();
    let changed_units = render::render_selected_document_for_test(
        &changed_title,
        &settings,
        after.fingerprint.clone(),
    )
    .unwrap()
    .collect::<Result<Vec<_>>>()
    .unwrap();
    assert_eq!(after_units.len(), changed_units.len());
    for (current, changed) in after_units.iter().zip(&changed_units) {
        assert_eq!(current.source_hash, changed.source_hash);
        assert_eq!(current.source_span, changed.source_span);
        assert_eq!(current.unit_id, changed.unit_id);
        assert_ne!(current.input_hash, changed.input_hash);
        assert_eq!(
            render::require_selected_unit_agreement(current, changed)
                .unwrap_err()
                .code,
            ErrorCode::FreshnessConflict
        );
    }
    for arm in ["S", "H"] {
        let (payload, stats) = fixture.run(arm);
        assert!(payload["text"].as_str().unwrap().contains("violet permit"));
        assert!(stats.ranked_unit_selection_used);
        current_citations(&fixture, &payload);
    }
    fixture.diagnostic_reprepare();
    assert!(
        fixture.run("S").0["text"]
            .as_str()
            .unwrap()
            .contains("violet permit")
    );
    let content_refresh = fixture
        .app
        .source_refresh(
            fixture.hidden.clone(),
            capture(
                "Changed restoration title",
                "recover archive now requires the orange permit.\n",
            ),
        )
        .unwrap();
    assert!(!content_refresh.reused);
    assert_ne!(content_refresh.allocated_ids["revision"], revision);
    let (payload, stats) = fixture.run("H");
    assert!(!payload["text"].as_str().unwrap().contains("violet permit"));
    assert!(stats.published_owners_without_retained_matching_units > 0);
    assert!(!stats.missing_retained_owners.is_empty() || !stats.filtered_units.eq(&0));
    assert!(payload["text"].as_str().unwrap().contains("orange permit"));
    fixture.diagnostic_reprepare();
    let (payload, _) = fixture.run("S");
    assert!(payload["text"].as_str().unwrap().contains("orange permit"));
    current_citations(&fixture, &payload);
}
#[test]
fn dense_mechanics_03_new_owner_hybrid_and_restrictive_filters() {
    let fixture = Fixture::new();
    let new = fixture
        .app
        .source_add(capture(
            "New restoration memo",
            "recover archive requires an amber checklist.\n",
        ))
        .unwrap()
        .allocated_ids["source"]
        .clone();
    let (payload, stats) = fixture.run("H");
    assert!(
        payload["text"]
            .as_str()
            .unwrap()
            .contains("amber checklist")
    );
    assert!(!stats.ranked_unit_selection_used);
    assert!(stats.published_owners_without_retained_matching_units > 0);
    let mut request = pinned_request();
    request.documents.filters.source_ids = vec![fixture.hidden.clone()];
    request.documents.limits.hits = 1;
    request.documents.limits.candidates = 1;
    let mut stats = Diagnostics::default();
    let payload = run(
        &fixture.catalog,
        QUERY,
        &request,
        "H",
        false,
        &mut stats,
        || Ok(()),
    )
    .unwrap();
    assert!(payload["text"].as_str().unwrap().contains("violet permit"));
    assert!(
        !payload["text"]
            .as_str()
            .unwrap()
            .contains("amber checklist")
    );
    assert!(
        payload["passages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|passage| !passage.to_string().contains(new.as_str()))
    );
}
#[test]
fn dense_mechanics_04_withdrawal_preserves_history_not_current() {
    let fixture = Fixture::new();
    let (before, _) = fixture.run("S");
    let old = current_citations(&fixture,&before).into_iter().find(|citation| matches!(citation,CitationRef::Source(reference) if reference.source_id==fixture.hidden)).unwrap();
    fixture
        .app
        .source_withdraw(fixture.hidden.clone(), "Superseded")
        .unwrap();
    let (after, stats) = fixture.run("H");
    assert!(!after["text"].as_str().unwrap().contains("violet permit"));
    assert!(stats.filtered_units > 0);
    let view = SourceView::from_fs(fixture.catalog.fs()).unwrap();
    assert!(view.verify(&old, CitationScope::Current).is_err());
    assert!(
        view.verify(&old, CitationScope::Historical)
            .unwrap()
            .quote
            .windows(b"violet permit".len())
            .any(|window| window == b"violet permit")
    );
}
#[test]
fn dense_mechanics_05_corrupt_vector_metadata_and_settings() {
    for kind in ["vector", "metadata", "settings", "space_id", "vector_hash"] {
        let fixture = Fixture::new();
        let old = vectors::VectorStore::open(fixture.catalog.fs(), None)
            .unwrap()
            .active()
            .unwrap()
            .unwrap();
        let connection = rusqlite::Connection::open(fixture.cache()).unwrap();
        match kind {
            "vector" => {
                connection
                    .execute("UPDATE embedding_vectors SET blob=X'0000000000000000'", [])
                    .unwrap();
            }
            "metadata" => {
                connection
                    .execute("UPDATE embedding_memberships SET metadata='{}'", [])
                    .unwrap();
            }
            "space_id" => {
                connection
                    .execute(
                        "UPDATE embedding_spaces SET id=?1",
                        ["x".repeat(2 * 1024 * 1024)],
                    )
                    .unwrap();
            }
            "vector_hash" => {
                connection
                    .execute(
                        "UPDATE embedding_vectors SET hash=?1",
                        ["x".repeat(2 * 1024 * 1024)],
                    )
                    .unwrap();
            }
            _ => {
                let mut changed = old.spec.clone();
                changed.settings.quality_target_bytes = Some(200);
                connection
                    .execute(
                        "UPDATE embedding_spaces SET spec=?1",
                        [serde_json::to_string(&changed).unwrap()],
                    )
                    .unwrap();
            }
        }
        drop(connection);
        if kind == "settings" {
            assert_eq!(
                vectors::RetainedMembershipReader::open(fixture.catalog.fs(), Some(&old), 5000)
                    .err()
                    .unwrap()
                    .code,
                ErrorCode::FreshnessConflict
            );
        } else {
            let mut stats = Diagnostics::default();
            let error = run(
                &fixture.catalog,
                QUERY,
                &pinned_request(),
                "S",
                false,
                &mut stats,
                || Ok(()),
            )
            .unwrap_err();
            assert!(matches!(
                error.code,
                ErrorCode::OfflineUnavailable | ErrorCode::IndexCorrupt
            ));
        }
    }
}
#[test]
fn dense_mechanics_06_held_readers_membership_and_active_replacement() {
    let fixture = Fixture::new();
    let old = vectors::VectorStore::open(fixture.catalog.fs(), None)
        .unwrap()
        .active()
        .unwrap()
        .unwrap();
    let retained =
        vectors::RetainedMembershipReader::open(fixture.catalog.fs(), Some(&old), 5000).unwrap();
    let units = retained.units().collect::<Result<Vec<_>>>().unwrap();
    let pinned = fixture
        .catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let old_snapshot = pinned.snapshot().clone();
    fixture
        .app
        .source_add(capture(
            "Later owner",
            "recover archive later membership.\n",
        ))
        .unwrap();
    assert_eq!(pinned.snapshot(), &old_snapshot);
    fixture.diagnostic_reprepare();
    assert_eq!(retained.units().collect::<Result<Vec<_>>>().unwrap(), units);
    assert_ne!(
        vectors::RetainedMembershipReader::open(fixture.catalog.fs(), Some(&old), 5000)
            .unwrap()
            .snapshot_key,
        retained.snapshot_key
    );
    retained.recheck_active(fixture.catalog.fs()).unwrap();
    let writer =
        WriterPermit::acquire(fixture.catalog.fs().root(), Duration::from_secs(1)).unwrap();
    let mut store = vectors::VectorStore::open(fixture.catalog.fs(), Some(&writer)).unwrap();
    let mut replacement = spec();
    replacement.model = "replacement model".into();
    let replacement_id = store.prepare_space(&replacement).unwrap();
    store
        .put_batch(
            &replacement_id,
            &units
                .iter()
                .map(|unit| unit.input_hash.clone())
                .collect::<Vec<_>>(),
            &units.iter().map(|_| vec![1.0, 0.0]).collect::<Vec<_>>(),
            true,
            &Blake3Hash::digest(b"replacement"),
        )
        .unwrap();
    store
        .memberships(&replacement_id, &old_snapshot, &units, true)
        .unwrap();
    assert_eq!(
        retained
            .recheck_active(fixture.catalog.fs())
            .unwrap_err()
            .code,
        ErrorCode::FreshnessConflict
    );
}
#[test]
fn dense_mechanics_07_cache_loss_offline_and_recovery() {
    let fixture = Fixture::new();
    let (baseline, _) = fixture.run("L");
    for suffix in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{suffix}", fixture.cache().display()));
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
    }
    for arm in ["S", "H"] {
        let mut stats = Diagnostics::default();
        assert_eq!(
            run(
                &fixture.catalog,
                QUERY,
                &pinned_request(),
                arm,
                false,
                &mut stats,
                || Ok(())
            )
            .unwrap_err()
            .code,
            ErrorCode::OfflineUnavailable
        );
    }
    let recovered = fixture.run("L").0;
    assert_eq!(baseline["text"], recovered["text"]);
    assert_eq!(baseline["passages"], recovered["passages"]);
    assert!(!fixture.cache().exists());
    fixture.diagnostic_reprepare();
    let (payload, _) = fixture.run("S");
    assert!(payload["text"].as_str().unwrap().contains("violet permit"));
    current_citations(&fixture, &payload);
}
#[test]
fn dense_mechanics_08_external_edit_exact_agreement_and_final_proof() {
    let fixture = Fixture::new();
    let target = fixture
        .catalog
        .fs()
        .root()
        .path()
        .join(fixture.hidden_path.as_str());
    let mut stats = Diagnostics::default();
    let error = run(
        &fixture.catalog,
        QUERY,
        &pinned_request(),
        "S",
        false,
        &mut stats,
        || {
            fs::write(&target, "externally changed canonical bytes\n").unwrap();
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::FreshnessConflict);
    let mut invalid = pinned_request();
    invalid.target = ContextTarget::Graph;
    assert!(
        run(
            &fixture.catalog,
            QUERY,
            &invalid,
            "S",
            false,
            &mut Diagnostics::default(),
            || Ok(())
        )
        .is_err()
    );
}
