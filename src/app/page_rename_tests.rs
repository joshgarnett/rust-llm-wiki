//! Ordinary local Page reorganization and request-only preview contracts.
use super::*;
use crate::{
    app::{OperationOptions, ReadRequest, RecordSelector, offline::init},
    changes::ChangeStatus,
    domain::{CitationRef, Eligibility, ErrorCode},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
    vault::{VaultFs, VaultRoot},
};
use clap::Parser;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn page(name: &str, body: &str) -> Vec<u8> {
    format!("---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: {name}\ntitle: {name}\nwiki_status: reviewed\n---\n{body}").into_bytes()
}
fn capture(text: &str) -> CaptureRequest {
    CaptureRequest {
        title: "Unmoved source café 東京".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "disposable-input.txt".into(),
        original: text.as_bytes().to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: None,
    }
}
#[derive(Debug, PartialEq, Eq)]
struct TreeEntry {
    directory: bool,
    modified: SystemTime,
    bytes: Vec<u8>,
}
// A small fixture-only observation includes the root and all directories,
// coordination sidecars and immutable files; access times are not a contract.
fn tree(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    fn visit(root: &Path, current: &Path, result: &mut BTreeMap<PathBuf, TreeEntry>) {
        let metadata = fs::symlink_metadata(current).unwrap();
        assert!(!metadata.file_type().is_symlink());
        result.insert(
            current.strip_prefix(root).unwrap().to_path_buf(),
            TreeEntry {
                directory: metadata.is_dir(),
                modified: metadata.modified().unwrap(),
                bytes: if metadata.is_file() {
                    fs::read(current).unwrap()
                } else {
                    vec![]
                },
            },
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(current).unwrap() {
                visit(root, &entry.unwrap().path(), result);
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    app: OfflineApp,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("ordinary Page vault with spaces");
        let options = OperationOptions {
            offline: true,
            lock_timeout_ms: 200,
            ..Default::default()
        };
        init(&root, "Page reorganization", options).unwrap();
        let app =
            OfflineApp::new(VaultFs::new(VaultRoot::explicit(&root).unwrap()), options).unwrap();
        app.index_rebuild_normalized().unwrap();
        app.page_put(
            path("pages/guide.md"),
            page("page_guide", "# Route\nMoveSignal café 東京 🦀.\n"),
            None,
        )
        .unwrap();
        app.page_put(
            path("pages/incoming.md"),
            page("page_incoming", "See [[pages/guide.md#Route|route]].\n"),
            None,
        )
        .unwrap();
        Self {
            _temp: temp,
            root,
            app,
        }
    }
    fn read(&self, selector: RecordSelector) -> crate::app::ReadOutcome {
        self.app
            .read(ReadRequest {
                selector,
                range: None,
                max_bytes: 8192,
            })
            .unwrap()
    }
    fn read_source(&self, payload: &VaultRelativePath) -> Value {
        // The public dispatcher owns selected-read freshness and citations;
        // OfflineApp::read remains the older uncited library body interface.
        let args = crate::cli::Arguments::try_parse_from([
            "lwiki",
            "--wiki",
            self.root.to_str().unwrap(),
            "--offline",
            "--json",
            "read",
            "--path",
            payload.as_str(),
            "--max-bytes",
            "8192",
        ])
        .unwrap();
        let (envelope, exit) = crate::cli::execute(&args);
        assert_eq!(exit, 0, "{}", serde_json::to_string(&envelope).unwrap());
        envelope.data
    }
}
fn assert_exact_source(read: &Value, expected: Eligibility) -> CitationRef {
    assert_eq!(
        read["source_citation"]["eligibility"],
        serde_json::to_value(expected).unwrap()
    );
    let citation: CitationRef =
        serde_json::from_value(read["source_citation"]["citation"].clone()).unwrap();
    let CitationRef::Source(reference) = &citation else {
        panic!("captured source reference")
    };
    assert_eq!(
        reference.span,
        serde_json::from_value::<crate::domain::ByteSpan>(read["range"].clone()).unwrap()
    );
    assert_eq!(
        reference.quote_hash,
        Blake3Hash::digest(read["body"].as_str().unwrap().as_bytes())
    );
    citation
}

#[test]
fn normalized_page_rename_public_lifecycle_preserves_source_history_and_rebuild() {
    let fixture = Fixture::new();
    let first = fixture
        .app
        .source_add(capture("First source café 東京 🦀.\n"))
        .unwrap();
    let source = first.allocated_ids["source"].clone();
    let revision = first.allocated_ids["revision"].clone();
    let old_payload = path(&format!("sources/{source}/revisions/{revision}/content.md"));
    let old_read = fixture.read_source(&old_payload);
    let exact_old = assert_exact_source(&old_read, Eligibility::Current);
    let refreshed = fixture
        .app
        .source_refresh(source.clone(), capture("Second source café 東京 🦀.\n"))
        .unwrap();
    let new_revision = refreshed.allocated_ids["revision"].clone();
    assert_ne!(revision, new_revision);
    let new_payload = path(&format!(
        "sources/{source}/revisions/{new_revision}/content.md"
    ));
    let historical = fixture.read_source(&old_payload);
    assert_eq!(
        assert_exact_source(&historical, Eligibility::Historical),
        exact_old
    );
    let current = fixture.read_source(&new_payload);
    let exact_current = assert_exact_source(&current, Eligibility::Current);
    let immutable_before = tree(&fixture.root.join("sources"));
    let guide = fixture.read(RecordSelector::Id(id("page_guide")));
    let moved = fixture
        .app
        .page_rename(
            id("page_guide"),
            path("handbook/Operations Guide.md"),
            guide.hash,
        )
        .unwrap();
    assert_eq!(moved.status, Some(ChangeStatus::Committed));
    assert!(moved.allocated_ids.is_empty());
    assert_eq!(tree(&fixture.root.join("sources")), immutable_before);
    let after = fixture.read(RecordSelector::Id(id("page_guide")));
    assert_eq!(after.path, path("handbook/Operations Guide.md"));
    assert_eq!(after.record.as_ref().unwrap().id(), &id("page_guide"));
    assert_eq!(after.body, guide.body);
    assert!(after.source_citation.is_none());
    assert!(
        fixture
            .read(RecordSelector::Id(id("page_incoming")))
            .body
            .contains("[[handbook/Operations Guide.md#Route|route]]")
    );
    assert_eq!(
        assert_exact_source(&fixture.read_source(&old_payload), Eligibility::Historical),
        exact_old
    );
    assert_eq!(
        assert_exact_source(&fixture.read_source(&new_payload), Eligibility::Current),
        exact_current
    );
    assert_eq!(
        fixture
            .app
            .read(ReadRequest {
                selector: RecordSelector::Path(path("pages/guide.md")),
                range: None,
                max_bytes: 8192
            })
            .err()
            .unwrap()
            .code,
        ErrorCode::RecordNotFound
    );
    let noop = fixture
        .app
        .page_rename(id("page_guide"), after.path.clone(), after.hash.clone())
        .unwrap();
    assert!(noop.reused && noop.change.is_none() && noop.plan.operations.is_empty());
    let replacement = page("page_guide", "# Route\nEdited MoveSignal café 東京 🦀.\n");
    fixture
        .app
        .page_put(after.path.clone(), replacement, Some(after.hash))
        .unwrap();
    let edited = fixture.read(RecordSelector::Id(id("page_guide")));
    assert!(edited.body.contains("Edited MoveSignal"));
    // Delete only rebuildable cache in this disposable owned vault.
    let canonical_before = tree(&fixture.root.join("sources"));
    fs::remove_dir_all(fixture.root.join(".wiki/cache")).unwrap();
    fixture.app.index_rebuild_normalized().unwrap();
    assert_eq!(tree(&fixture.root.join("sources")), canonical_before);
    let rebuilt = fixture.read(RecordSelector::Id(id("page_guide")));
    assert_eq!(rebuilt.path, edited.path);
    assert_eq!(rebuilt.body, edited.body);
    assert_eq!(rebuilt.hash, edited.hash);
    assert_eq!(
        assert_exact_source(&fixture.read_source(&old_payload), Eligibility::Historical),
        exact_old
    );
    assert_eq!(
        assert_exact_source(&fixture.read_source(&new_payload), Eligibility::Current),
        exact_current
    );
    fixture
        .app
        .source_withdraw(source, "Disposable lifecycle withdrawal")
        .unwrap();
    assert_eq!(
        assert_exact_source(&fixture.read_source(&old_payload), Eligibility::Withdrawn),
        exact_old
    );
    assert_eq!(
        assert_exact_source(&fixture.read_source(&new_payload), Eligibility::Withdrawn),
        exact_current
    );
    let checked = fixture.app.check().unwrap();
    assert!(checked.complete && checked.cache_matches_canonical == Some(true));
}

#[test]
fn normalized_page_rename_preview_and_external_edit_preserve_owned_state() {
    let fixture = Fixture::new();
    let hash = Blake3Hash::digest(fs::read(fixture.root.join("pages/guide.md")).unwrap());
    let preview = OfflineApp::new(
        fixture.app.fs().clone(),
        OperationOptions {
            dry_run: true,
            ..fixture.app.options().clone()
        },
    )
    .unwrap();
    let before = tree(&fixture.root);
    for target in [id("page_guide"), id("page_not_present")] {
        let outcome = preview
            .page_rename_indexed(target, path("handbook/Operations Guide.md"), hash.clone())
            .unwrap();
        assert!(outcome.plan.title.starts_with("Would rename Page"));
        assert!(outcome.plan.title.contains("unperformed"));
        assert!(outcome.plan.read_preconditions.is_empty() && outcome.plan.operations.is_empty());
        assert!(outcome.change.is_none() && outcome.snapshot.is_none() && outcome.status.is_none());
        assert!(!outcome.reused && outcome.allocated_ids.is_empty());
    }
    assert_eq!(
        tree(&fixture.root),
        before,
        "preview must preserve root/directory/file mtimes and WAL/SHM bytes"
    );
    // A selected incoming edit with identical length and restored mtime still
    // invalidates admission by hash, before creating the destination.
    let incoming = fixture.root.join("pages/incoming.md");
    let original = fs::read(&incoming).unwrap();
    let modified = fs::metadata(&incoming).unwrap().modified().unwrap();
    let changed = String::from_utf8(original.clone())
        .unwrap()
        .replace("See ", "SEE ")
        .into_bytes();
    assert_eq!(changed.len(), original.len());
    fs::write(&incoming, &changed).unwrap();
    fs::File::options()
        .write(true)
        .open(&incoming)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let guide_before = fs::read(fixture.root.join("pages/guide.md")).unwrap();
    let failure = fixture
        .app
        .page_rename(id("page_guide"), path("handbook/Operations Guide.md"), hash)
        .err()
        .unwrap();
    assert_eq!(failure.code, ErrorCode::ContentConflict);
    assert_eq!(fs::read(&incoming).unwrap(), changed);
    assert_eq!(
        fs::read(fixture.root.join("pages/guide.md")).unwrap(),
        guide_before
    );
    assert!(!fixture.root.join("handbook/Operations Guide.md").exists());
}
