//! Paired compatibility expectations frozen in the implementation report before
//! the QueryCatalog refactor. These use real source plans and both real builders.
use super::*;
use crate::{
    catalog::{
        Catalog, SnapshotVerification,
        file_types::{BuildIdentity, CatalogSelection},
        normalized_build::{BuildLimits, NormalizedBuilder},
        query_types::QueryReadLimits,
        scan, selector,
    },
    changes::ChangeDraft,
    retrieval::{context_selection_packet::SelectionAction, verification},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use std::{fs, path::Path, sync::Arc, time::Duration};

const VAULT: &str = "vault_00000000-0000-7000-8000-00000000001b";
const CLAIM: &str = "assertion_00000000-0000-7000-8000-00000000000a";
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy(&path, &target)
        } else {
            fs::copy(path, target).unwrap();
        }
    }
}
fn apply_fixture(root: &Path, draft: ChangeDraft) {
    for operation in draft.operations {
        let path = root.join(operation.target.as_str());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, operation.proposed.expect("fixture additions/refresh")).unwrap();
    }
}
fn capture(title: &str, body: &str) -> CaptureRequest {
    CaptureRequest {
        title: title.into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: format!("{title}.md"),
        original: body.as_bytes().to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: Some("text/markdown".into()),
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    catalog: Catalog,
    sources: Vec<RecordId>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bootstrap/vault"),
            temp.path(),
        );
        fs::write(temp.path().join("reviewed.md"), "---\nwiki_schema: '1'\nwiki_id: page_snapshot_reviewed\nwiki_kind: page\ntitle: Authored snapshot\nwiki_status: reviewed\naliases: [SnapshotExactAlias]\ntags: [snapshot_tag]\n---\n# Authored\n\nauthoredneedle: Café 東京 preserves reviewed Markdown.\n").unwrap();
        fs::write(temp.path().join("supported_entity.md"), format!("---\nwiki_schema: '1'\nwiki_id: entity_snapshot_supported\nwiki_kind: entity\ntitle: Supported snapshot entity\nwiki_status: active\nwiki_entity_type: concept\nwiki_depends_on_ids: [{CLAIM}]\n---\n# Supported\n\nauthoredneedle: entity explanation retains eligible original text.\n")).unwrap();
        fs::write(temp.path().join("historical_entity.md"), "---\nwiki_schema: '1'\nwiki_id: entity_snapshot_historical\nwiki_kind: entity\ntitle: Historical snapshot entity\nwiki_status: superseded\nwiki_entity_type: concept\n---\n# Historical\n\nauthoredneedle: historical entity explanation is retained.\n").unwrap();
        fs::write(temp.path().join("draft.md"), "---\nwiki_schema: '1'\nwiki_id: page_snapshot_draft\nwiki_kind: page\ntitle: Discovery draft\nwiki_status: draft\n---\nnocontextneedle: draft text\n").unwrap();
        fs::write(temp.path().join("unsupported_entity.md"), "---\nwiki_schema: '1'\nwiki_id: entity_snapshot_unsupported\nwiki_kind: entity\ntitle: Unsupported snapshot entity\nwiki_status: active\nwiki_entity_type: concept\n---\nnocontextneedle: unsupported description\n").unwrap();
        fs::write(
            temp.path().join("plain.md"),
            "nocontextneedle: plain text\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("malformed.md"),
            "---\nwiki_schema: '1'\nwiki_kind: page\n---\nnocontextneedle: malformed text\n",
        )
        .unwrap();
        let vaultfs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let store = SourceStore::new(vaultfs.clone());
        let filler = "The maintenance background covers ordinary procedures.\n\n".repeat(90);
        let first = store.plan_capture(capture("Operations one", &format!("# Operations\n\noperationsprobe blue: choose the Café 東京 cluster.\n\n{filler}# Retry\n\noperationsprobe retry: wait exactly 37 seconds.\n"))).unwrap();
        let first_id = first.source_id.clone();
        apply_fixture(temp.path(), first.draft.unwrap());
        let second = store.plan_capture(capture("Operations two", "# Exception\n\noperationsprobe exception: require the violet permit from the second source.\n")).unwrap();
        let second_id = second.source_id.clone();
        apply_fixture(temp.path(), second.draft.unwrap());
        // Real refresh preserves an older captured revision for Snapshot scope.
        let refreshed = store
            .plan_refresh(
                &second_id,
                capture(
                    "Operations two",
                    "# Exception\n\noperationsprobe exception: the current permit is amber.\n",
                ),
            )
            .unwrap();
        apply_fixture(temp.path(), refreshed.draft.unwrap());
        let catalog = Catalog::new(vaultfs, id(VAULT));
        let writer = WriterPermit::acquire(catalog.fs().root(), Duration::from_secs(1)).unwrap();
        catalog.sync(&writer).unwrap();
        drop(writer);
        Self {
            temp,
            catalog,
            sources: vec![first_id, second_id],
        }
    }
    fn publish(&self) {
        publish_catalog(&self.catalog);
    }
    fn run(&self, query: &str, request: &ContextRequest) -> ContextResult {
        verification::context(&self.catalog, None, query, request).unwrap()
    }
}
fn publish_catalog(catalog: &Catalog) {
    let writer = WriterPermit::acquire(catalog.fs().root(), Duration::from_secs(1)).unwrap();
    let identity = BuildIdentity {
        selection: CatalogSelection::new(id(VAULT), 1).unwrap(),
        origin: None,
        vector_cache_lost: false,
        vector_loss_unknown: false,
    };
    selector::prepare(catalog.fs(), &writer, &identity.selection).unwrap();
    let mut builder =
        NormalizedBuilder::begin(catalog.fs(), &writer, identity, BuildLimits::default()).unwrap();
    let input = scan::scan_input(catalog.fs(), &id(VAULT)).unwrap();
    let projection =
        scan::project_normalized_with_sink(catalog.fs(), &input, false, &mut builder).unwrap();
    let completed = builder.finish_normalized(&projection).unwrap();
    selector::publish(
        catalog.fs(),
        &writer,
        &completed.identity.selection,
        Duration::from_secs(1),
    )
    .unwrap();
}

fn request() -> ContextRequest {
    let mut request = ContextRequest {
        scope: ContextScope::Snapshot,
        ..Default::default()
    };
    request.documents.limits.excerpt_bytes = 320;
    request.verification_budget.max_elapsed_ms = 30_000;
    request
}
fn assert_same_content(a: &ContextResult, b: &ContextResult) {
    assert_eq!(a.text(), b.text());
    assert_eq!(a.passages(), b.passages());
    assert_eq!(a.bundles(), b.bundles());
    assert_eq!(a.omissions(), b.omissions());
    assert_eq!(a.usage(), b.usage());
    assert_eq!(a.truncated(), b.truncated());
    assert_eq!(b.verification(), &SnapshotVerification::IndexSnapshot);
    assert_eq!(b.usage().verification_bytes, 0);
    assert_eq!(b.usage().verification_files, 0);
    assert_eq!(b.usage().verification_entries, 0);
    assert!(!b.network_used);
    assert!(
        b.passages()
            .iter()
            .all(|passage| passage.citations.is_empty())
    );
}

#[test]
fn paired_real_builders_preserve_authored_captured_distant_and_budget_context() {
    let fixture = Fixture::new();
    let mut filtered = request();
    filtered.documents.filters.tags = vec!["snapshot_tag".into()];
    let mut sources = request();
    sources.documents.filters.source_ids = fixture.sources.clone();
    let mut small = sources.clone();
    small.budget.max_bytes = 780;
    small.budget.max_tokens = 195;
    small.budget.instruction_bytes = 80;
    small.budget.instruction_tokens = 20;
    let cases = vec![
        ("authoredneedle", request()),
        ("authoredneedle", filtered),
        ("operationsprobe blue retry exception", sources),
        ("operationsprobe blue retry exception", small),
        ("nocontextneedle", request()),
    ];
    let legacy = cases
        .iter()
        .map(|(query, request)| fixture.run(query, request))
        .collect::<Vec<_>>();
    assert!(legacy[0].text().contains("Café 東京"));
    assert!(legacy[0].text().contains("entity explanation"));
    assert!(legacy[0].text().contains("historical entity explanation"));
    assert!(legacy[2].text().contains("37 seconds"));
    assert!(legacy[2].text().contains("violet permit"));
    assert!(legacy[2].text().contains("current permit is amber"));
    assert!(legacy[2].passages().iter().any(|p| p.span.start() > 4000));
    assert!(
        legacy[2]
            .passages()
            .iter()
            .all(|p| p.label == ExcerptLabel::CapturedSource)
    );
    assert!(legacy[3].truncated());
    assert!(!legacy[3].omissions().is_empty());
    assert!(legacy[4].passages().is_empty());
    fixture.publish();
    for ((query, request), expected) in cases.iter().zip(&legacy) {
        let actual = fixture.run(query, request);
        assert_same_content(expected, &actual);
        let reader = fixture
            .catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        for passage in actual.passages() {
            let document = reader.document(&passage.locator.path).unwrap().unwrap();
            assert_eq!(
                passage.span.slice(&document.raw_text).unwrap(),
                passage.text
            );
            assert_eq!(passage.locator.observed_hash, document.hash);
        }
        assert!(
            actual.usage().rendered_bytes + request.budget.instruction_bytes
                <= request.budget.max_bytes
        );
        assert!(
            actual.usage().estimated_tokens + request.budget.instruction_tokens
                <= request.budget.max_tokens
        );
    }
}

#[test]
fn normalized_snapshot_keeps_cached_bytes_after_external_changes() {
    let fixture = Fixture::new();
    fixture.publish();
    let baseline = fixture.run("authoredneedle", &request());
    fs::write(fixture.temp.path().join("reviewed.md"), "external change\n").unwrap();
    fs::remove_file(fixture.temp.path().join("supported_entity.md")).unwrap();
    fs::write(
        fixture.temp.path().join("new.md"),
        "authoredneedle external new member\n",
    )
    .unwrap();
    assert_same_content(&baseline, &fixture.run("authoredneedle", &request()));
}

#[test]
fn normalized_snapshot_refuses_other_modes_scopes_graph_and_host_selection() {
    let fixture = Fixture::new();
    fixture.publish();
    let mut invalid = Vec::new();
    for mode in [
        SearchMode::Literal,
        SearchMode::Semantic,
        SearchMode::Hybrid,
    ] {
        let mut r = request();
        r.documents.mode = mode;
        invalid.push(r);
    }
    for scope in [ContextScope::Current, ContextScope::Historical] {
        let mut r = request();
        r.scope = scope;
        invalid.push(r);
    }
    for target in [ContextTarget::Graph, ContextTarget::Combined] {
        let mut r = request();
        r.target = target;
        r.graph = Some(Default::default());
        invalid.push(r);
    }
    let mut r = request();
    r.graph = Some(Default::default());
    invalid.push(r);
    for request in invalid {
        assert_eq!(
            verification::context(&fixture.catalog, None, "authoredneedle", &request)
                .unwrap_err()
                .code,
            ErrorCode::Usage
        );
    }
    let options = ContextOptions {
        selection: SelectionAction::Prepare,
        ..Default::default()
    };
    let err = verification::context_with_options(
        &fixture.catalog,
        None,
        "authoredneedle",
        &request(),
        &options,
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Usage);
    assert!(err.message.contains("host selection requires current"));
}

#[test]
fn cached_assembly_authenticates_identity_hash_filters_and_protocol() {
    let fixture = Fixture::new();
    fixture.publish();
    let reader = fixture
        .catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let normalized = validate_request("authoredneedle", &request()).unwrap();
    let hits = crate::retrieval::lexical::search_context_catalog(
        &reader,
        "authoredneedle",
        &normalized.documents,
        true,
    )
    .unwrap();
    assert!(!hits.hits.is_empty());
    let before = reader.usage();
    let baseline =
        assemble_snapshot_for_query(&reader, &normalized, &hits, "authoredneedle").unwrap();
    assert!(!baseline.passages().is_empty());
    let decoded = reader.usage().rows - before.rows;
    assert!(
        decoded <= hits.hits.len() * 2,
        "each owner must decode at most its document and record; got {decoded}"
    );
    let mut bad_hash = hits.clone();
    bad_hash.hits[0].locator.observed_hash = Blake3Hash::digest("wrong");
    assert!(
        assemble_snapshot_for_query(&reader, &normalized, &bad_hash, "authoredneedle").is_err()
    );
    let mut bad_identity = hits.clone();
    bad_identity.hits[0].locator.record = None;
    assert!(
        assemble_snapshot_for_query(&reader, &normalized, &bad_identity, "authoredneedle").is_err()
    );
    let mut bad_protocol = hits.clone();
    bad_protocol.dependency_fingerprint = Blake3Hash::digest("wrong domain");
    assert!(
        assemble_snapshot_for_query(&reader, &normalized, &bad_protocol, "authoredneedle").is_err()
    );
    let mut filtered = normalized;
    filtered.documents.filters.path_prefix = Some("absent/".into());
    let draft = assemble_snapshot_for_query(&reader, &filtered, &hits, "authoredneedle").unwrap();
    assert!(draft.passages().is_empty());
    assert!(!draft.omissions().is_empty());
}

struct RemoveAuthority(std::path::PathBuf);
impl ContextFault for RemoveAuthority {
    fn check(&self, checkpoint: ContextCheckpoint) -> Result<()> {
        assert_eq!(
            checkpoint,
            ContextCheckpoint::BeforeFinalVerification { attempt: 0 }
        );
        fs::remove_file(&self.0).unwrap();
        Ok(())
    }
}
#[test]
fn normalized_snapshot_final_check_refuses_disappeared_authority() {
    let fixture = Fixture::new();
    fixture.publish();
    let options = ContextOptions {
        fault: Some(Arc::new(RemoveAuthority(
            fixture.temp.path().join(".wiki/state/operations.json"),
        ))),
        ..Default::default()
    };
    assert!(
        verification::context_with_options(
            &fixture.catalog,
            None,
            "authoredneedle",
            &request(),
            &options
        )
        .is_err()
    );
}

struct ActivateCatalog(VaultFs);
impl ContextFault for ActivateCatalog {
    fn check(&self, checkpoint: ContextCheckpoint) -> Result<()> {
        assert_eq!(
            checkpoint,
            ContextCheckpoint::BeforeFinalVerification { attempt: 0 }
        );
        publish_catalog(&Catalog::new(self.0.clone(), id(VAULT)));
        Ok(())
    }
}
#[test]
fn legacy_snapshot_refuses_activation_while_its_reader_is_held() {
    let fixture = Fixture::new();
    let options = ContextOptions {
        fault: Some(Arc::new(ActivateCatalog(fixture.catalog.fs().clone()))),
        ..Default::default()
    };
    let error = verification::context_with_options(
        &fixture.catalog,
        None,
        "authoredneedle",
        &request(),
        &options,
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryRequired);
    assert!(error.message.contains("legacy context reader was held"));
}

#[test]
fn paired_real_builders_preserve_exact_alias_snapshot_context() {
    let fixture = Fixture::new();
    let query = "SnapshotExactAlias";
    let request = request();
    let normalized = validate_request(query, &request).unwrap();
    let legacy_reader = fixture.catalog.index_snapshot().unwrap();
    let legacy_hits = crate::retrieval::lexical::search_context(
        &legacy_reader,
        query,
        &normalized.documents,
        true,
    )
    .unwrap();
    assert!(
        legacy_hits
            .hits
            .iter()
            .any(|hit| hit.locator.path.as_str() == "reviewed.md"
                && hit.reasons.contains(&RetrievalReason::ExactAlias))
    );
    let legacy = fixture.run(query, &request);
    assert!(legacy.text().contains("Café 東京"));
    fixture.publish();
    let reader = fixture
        .catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let hits = crate::retrieval::lexical::search_context_catalog(
        &reader,
        query,
        &normalized.documents,
        true,
    )
    .unwrap();
    assert!(
        hits.hits
            .iter()
            .any(|hit| hit.locator.path.as_str() == "reviewed.md"
                && hit.reasons.contains(&RetrievalReason::ExactAlias))
    );
    assert_same_content(&legacy, &fixture.run(query, &request));
}

#[test]
fn normalized_snapshot_refuses_changed_vault_identity_before_cache_read() {
    let fixture = Fixture::new();
    fixture.publish();
    fs::write(fixture.temp.path().join("WIKI.md"),
        "---\nwiki_schema: '1'\nwiki_id: vault_other_snapshot\nwiki_kind: vault\ntitle: Other vault\n---\nOther vault marker\n").unwrap();
    let result = verification::context(&fixture.catalog, None, "authoredneedle", &request());
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .message
            .contains("vault identity changed")
    );
}
