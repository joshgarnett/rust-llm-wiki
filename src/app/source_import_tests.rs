//! Public import-controller workflows. All vaults/inputs are disposable; native
//! returned-error cuts establish reopen mechanics, never power-loss safety.
#![cfg(unix)]

#[path = "source_import_experiment.rs"]
mod source_import_experiment;
#[path = "source_import_replay_guard_tests.rs"]
mod source_import_replay_guard_tests;

use super::{OfflineApp, OperationOptions, ReadRequest, RecordSelector, offline::init};
use crate::{
    catalog::{
        Catalog,
        query_types::{QueryCatalog, QueryReadLimits},
    },
    changes::{ChangeStatus, PreparedChange},
    domain::{Blake3Hash, CitationRef, RecordId, VaultRelativePath},
    retrieval::{
        ContextRequest, ContextScope, QueryPlan, SearchFilters, lexical::search_indexed_sources,
        verification,
    },
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
    storage::{self, StorageOptions},
    vault::{DirectorySync, DurableIo, NativeIo, VaultFs, VaultRoot, WriterPermit},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

const KEY: &str = "controller test import 茶";
fn options() -> OperationOptions {
    OperationOptions {
        offline: true,
        lock_timeout_ms: 200,
        ..Default::default()
    }
}
fn rel(path: impl Into<String>) -> VaultRelativePath {
    VaultRelativePath::new(path).unwrap()
}
struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
    inputs: Vec<PathBuf>,
    bytes: Vec<Vec<u8>>,
    list: PathBuf,
    manifest: PathBuf,
}
impl Fixture {
    fn new(migrated: bool, count: usize) -> Self {
        let inputs: Vec<_> = (0..count)
            .map(|n| {
                (
                    format!("input {n}.txt"),
                    format!(
                        "Importneedle{n} vessel holds {} amber tokens. café 東京 🦀.\n",
                        17 + n
                    )
                    .into_bytes(),
                )
            })
            .collect();
        Self::with_inputs(migrated, inputs)
    }
    fn with_inputs(migrated: bool, inputs: Vec<(String, Vec<u8>)>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("import vault with spaces");
        init(&root, "Public import fixture", options()).unwrap();
        let vault = VaultFs::new(VaultRoot::explicit(&root).unwrap());
        if migrated {
            let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
            storage::cleanup(&vault, &writer, &StorageOptions::default()).unwrap();
        }
        let app = OfflineApp::new(vault, options()).unwrap();
        app.index_rebuild_normalized().unwrap();
        assert_eq!(storage::layout::active(app.fs().root()).unwrap(), migrated);
        let input_root = temp.path().join("outside inputs with spaces");
        fs::create_dir(&input_root).unwrap();
        let list = input_root.join("inputs.jsonl");
        let manifest = input_root.join("prepared manifest.jsonl");
        let mut list_file = File::create(&list).unwrap();
        let mut paths = vec![];
        let mut bytes = vec![];
        for (ordinal, (name, body)) in inputs.into_iter().enumerate() {
            let path = input_root.join(&name);
            fs::write(&path, &body).unwrap();
            serde_json::to_writer(&mut list_file, &json!({"path":name, "title":format!("Imported source {ordinal}"), "media_type":"application/test"})).unwrap();
            list_file.write_all(b"\n").unwrap();
            paths.push(path);
            bytes.push(body);
        }
        Self {
            temp,
            root,
            inputs: paths,
            bytes,
            list,
            manifest,
        }
    }
    fn app(&self) -> OfflineApp {
        self.with_options(options())
    }
    fn with_options(&self, opts: OperationOptions) -> OfflineApp {
        OfflineApp::new(VaultFs::new(VaultRoot::explicit(&self.root).unwrap()), opts).unwrap()
    }
    fn prepare(&self) -> Blake3Hash {
        let prepared = super::prepare_source_import(&self.list, &self.manifest, false).unwrap();
        assert!(!prepared.preview);
        assert_eq!(prepared.items, self.inputs.len() as u64);
        assert_eq!(
            prepared.input_bytes,
            self.bytes.iter().map(|b| b.len() as u64).sum::<u64>()
        );
        prepared.manifest_hash
    }
    fn remove_originals(&self) {
        for input in &self.inputs {
            fs::remove_file(input).unwrap();
        }
    }
    fn physical(&self, path: &VaultRelativePath) -> PathBuf {
        self.app().fs().root().resolve(path).unwrap()
    }
    fn assert_item(&self, ordinal: u64, source: &RecordId, revision: &RecordId, available: bool) {
        let app = self.app();
        let source_path = rel(format!("sources/{source}/source.md"));
        let read = app
            .read(ReadRequest {
                selector: RecordSelector::Id(source.clone()),
                range: None,
                max_bytes: 4096,
            })
            .unwrap();
        assert_eq!(read.path, source_path);
        let source_record = read.record.unwrap();
        assert_eq!(
            source_record.string("title"),
            Some(format!("Imported source {ordinal}").as_str())
        );
        assert_eq!(
            source_record.string("wiki_current_revision"),
            Some(revision.as_str())
        );
        let tree = format!("sources/{source}/revisions/{revision}");
        assert_eq!(
            fs::read(self.physical(&rel(format!("{tree}/original.bin")))).unwrap(),
            self.bytes[ordinal as usize]
        );
        let note = app
            .read(ReadRequest {
                selector: RecordSelector::Id(revision.clone()),
                range: None,
                max_bytes: 16_384,
            })
            .unwrap()
            .record
            .unwrap();
        assert_eq!(
            note.string("wiki_original_hash"),
            Some(
                Blake3Hash::digest(&self.bytes[ordinal as usize])
                    .to_string()
                    .as_str()
            )
        );
        let content = rel(format!("{tree}/content.md"));
        if available {
            let read = app
                .read(ReadRequest {
                    selector: RecordSelector::Path(content.clone()),
                    range: None,
                    max_bytes: 16_384,
                })
                .unwrap();
            assert_eq!(read.body.as_bytes(), self.bytes[ordinal as usize]);
            assert_eq!(read.hash, Blake3Hash::digest(&self.bytes[ordinal as usize]));
        } else {
            assert!(!self.physical(&content).exists());
            assert_eq!(note.string("wiki_extraction_status"), Some("unsupported"));
        }
    }
    fn assert_query(&self, ordinal: u64, source: &RecordId, revision: &RecordId) {
        let app = self.app();
        let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
        let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
        let plan = QueryPlan {
            filters: SearchFilters {
                source_ids: vec![source.clone()],
                ..Default::default()
            },
            ..Default::default()
        };
        let hits =
            search_indexed_sources(&reader, &format!("Importneedle{ordinal}"), &plan).unwrap();
        let hit = hits
            .hits
            .iter()
            .find(|hit| {
                hit.source_id.as_ref() == Some(source)
                    && hit.owner_revision.as_ref() == Some(revision)
            })
            .expect("imported current source missing from indexed lexical query");
        assert!(hit.excerpt.text.contains(&format!("Importneedle{ordinal}")));
        // Raw indexed search is generation-scoped and has no final evidence
        // seal. Obtain citations from the public verified context coordinator.
        drop(reader);
        let context = verification::context(
            &catalog,
            None,
            &format!("Importneedle{ordinal}"),
            &ContextRequest {
                scope: ContextScope::IndexedEvidence,
                documents: plan,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!context.network_used);
        let passage = context.passages.iter().find(|passage| passage.citations.iter().any(|citation| matches!(citation, CitationRef::Source(reference) if &reference.source_id == source && &reference.source_revision == revision))).expect("verified context omitted imported source evidence");
        let CitationRef::Source(citation) = passage.citations.iter().find(|citation| matches!(citation, CitationRef::Source(reference) if &reference.source_id == source && &reference.source_revision == revision)).unwrap() else { unreachable!() };
        assert_eq!(&citation.source_id, source);
        assert_eq!(&citation.source_revision, revision);
        let original = &self.bytes[ordinal as usize];
        let quote = &original[citation.span.start() as usize..citation.span.end() as usize];
        assert_eq!(Blake3Hash::digest(quote), citation.quote_hash);
        assert_eq!(quote, passage.text.as_bytes());
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Directory,
    File(Vec<u8>, SystemTime),
    Link(PathBuf),
}
// Disposable test-only filesystem oracle; never controller history discovery.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
    fn visit(root: &Path, directory: &Path, result: &mut BTreeMap<PathBuf, Entry>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let name = path.strip_prefix(root).unwrap().to_owned();
            if metadata.file_type().is_symlink() {
                result.insert(name, Entry::Link(fs::read_link(&path).unwrap()));
            } else if metadata.is_dir() {
                result.insert(name, Entry::Directory);
                visit(root, &path, result);
            } else {
                result.insert(
                    name,
                    Entry::File(fs::read(&path).unwrap(), metadata.modified().unwrap()),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}
fn preservation_snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
    // A real refused run may acquire/release the writer and rewrite its PID
    // diagnostic. Canonical/import/cache evidence must still remain identical.
    let mut result = snapshot(root);
    result.retain(|path, _| !path.ends_with(".wiki/state/writer.lock"));
    result
}
fn mapping(group: &super::SourceImportGroup) -> Vec<(u64, RecordId, RecordId)> {
    group
        .items
        .iter()
        .map(|item| {
            (
                item.ordinal,
                item.source_id.clone(),
                item.revision_id.clone(),
            )
        })
        .collect()
}

#[test]
fn public_import_one_four_and_eight_share_exactly_one_change_per_group() {
    for migrated in [false, true] {
        for group_size in [1, 4, 8] {
            let fixture = Fixture::new(migrated, 8);
            let hash = fixture.prepare();
            let mut result = fixture
                .app()
                .source_import_run(&fixture.manifest, KEY, group_size, 1)
                .unwrap();
            let mut all = vec![];
            let mut changes = BTreeSet::new();
            loop {
                assert_eq!(result.manifest_hash, hash);
                assert_eq!(result.total_items, 8);
                assert!(!result.preview);
                let group = result.last_group.as_ref().unwrap();
                assert_eq!(group.group, result.groups_committed - 1);
                assert_eq!(group.items.len(), group_size);
                assert!(changes.insert(group.change.change_id.clone()));
                let change = fixture
                    .app()
                    .changes_show(group.change.change_id.clone())
                    .unwrap();
                assert_eq!(change.prepared, group.change);
                assert_eq!(change.status, ChangeStatus::Committed);
                assert_eq!(change.manifest.operations.len(), group_size * 4);
                all.extend(mapping(group));
                assert_eq!(result.imported_items as usize, all.len());
                if result.completed {
                    break;
                }
                let before_groups = result.groups_committed;
                result = fixture.app().source_import_resume(KEY, 1).unwrap();
                assert_eq!(result.groups_committed, before_groups + 1);
            }
            assert_eq!(
                all.iter().map(|row| row.0).collect::<Vec<_>>(),
                (0..8).collect::<Vec<_>>()
            );
            assert_eq!(
                all.iter().map(|row| &row.1).collect::<BTreeSet<_>>().len(),
                8
            );
            assert_eq!(
                all.iter().map(|row| &row.2).collect::<BTreeSet<_>>().len(),
                8
            );
            for (ordinal, source, revision) in &all {
                fixture.assert_item(*ordinal, source, revision, true);
            }
            fixture.assert_query(all[0].0, &all[0].1, &all[0].2);
            let status = fixture.app().source_import_status(KEY).unwrap();
            assert!(status.completed);
            assert_eq!(status.groups_committed, 8 / group_size as u64);
            assert_eq!(status.pending_change, None);
        }
    }
}

#[test]
fn mixed_complete_empty_and_unsupported_sources_share_a_committed_group() {
    for migrated in [false, true] {
        let fixture = Fixture::with_inputs(
            migrated,
            vec![
                (
                    "complete.txt".into(),
                    b"Importneedle0 complete evidence\n".to_vec(),
                ),
                ("empty.txt".into(), vec![]),
                (
                    "unsupported.pdf".into(),
                    b"PDF-looking binary\x00\xff".to_vec(),
                ),
                ("invalid-utf8.txt".into(), vec![0xff, 0xfe, 0xfd]),
            ],
        );
        fixture.prepare();
        let result = fixture
            .app()
            .source_import_run(&fixture.manifest, KEY, 4, 1)
            .unwrap();
        assert!(result.completed);
        assert_eq!(result.imported_items, 4);
        assert_eq!(result.groups_committed, 1);
        let group = result.last_group.unwrap();
        assert_eq!(group.items.len(), 4);
        for item in &group.items {
            fixture.assert_item(
                item.ordinal,
                &item.source_id,
                &item.revision_id,
                item.ordinal < 2,
            );
        }
        let change = fixture.app().changes_show(group.change.change_id).unwrap();
        assert_eq!(change.manifest.operations.len(), 14);
        fixture.assert_query(0, &group.items[0].source_id, &group.items[0].revision_id);
    }
}

#[test]
fn completed_same_key_replay_and_status_survive_removed_originals_without_duplicates() {
    for migrated in [false, true] {
        let fixture = Fixture::new(migrated, 4);
        fixture.prepare();
        let first = fixture
            .app()
            .source_import_run(&fixture.manifest, KEY, 4, 1)
            .unwrap();
        let group = first.last_group.unwrap();
        fixture.remove_originals();
        let canonical_before = snapshot(&fixture.root.join("sources"));
        let replay = fixture
            .app()
            .source_import_run(&fixture.manifest, KEY, 4, 1)
            .unwrap();
        let resumed = fixture.app().source_import_resume(KEY, 1).unwrap();
        let status = fixture.app().source_import_status(KEY).unwrap();
        for outcome in [replay, resumed, status] {
            assert!(outcome.completed);
            assert_eq!(outcome.imported_items, 4);
            assert_eq!(outcome.groups_committed, 1);
            assert_eq!(outcome.last_group.as_ref().unwrap().change, group.change);
            assert_eq!(
                mapping(outcome.last_group.as_ref().unwrap()),
                mapping(&group)
            );
            assert_eq!(outcome.pending_change, None);
        }
        assert_eq!(snapshot(&fixture.root.join("sources")), canonical_before);
    }
}

#[test]
fn conflicting_manifest_or_group_size_preserves_completed_import() {
    let fixture = Fixture::new(false, 2);
    fixture.prepare();
    let original = fixture
        .app()
        .source_import_run(&fixture.manifest, KEY, 2, 1)
        .unwrap();
    let group = original.last_group.unwrap();
    let alternate_list = fixture.list.with_file_name("alternate.jsonl");
    fs::write(
        &alternate_list,
        format!(
            "{}\n",
            json!({"path":fixture.inputs[0], "title":"different request"})
        ),
    )
    .unwrap();
    let alternate = fixture.manifest.with_file_name("alternate.manifest.jsonl");
    super::prepare_source_import(&alternate_list, &alternate, false).unwrap();
    let before = preservation_snapshot(fixture.temp.path());
    assert!(
        fixture
            .app()
            .source_import_run(&alternate, KEY, 2, 1)
            .is_err()
    );
    assert_eq!(preservation_snapshot(fixture.temp.path()), before);
    assert!(
        fixture
            .app()
            .source_import_run(&fixture.manifest, KEY, 1, 1)
            .is_err()
    );
    assert_eq!(preservation_snapshot(fixture.temp.path()), before);
    let status = fixture.app().source_import_status(KEY).unwrap();
    assert_eq!(status.last_group.unwrap().change, group.change);
}

#[test]
fn changed_next_original_refuses_without_publishing_or_rewriting_committed_sources() {
    for migrated in [false, true] {
        let fixture = Fixture::new(migrated, 2);
        fixture.prepare();
        let first = fixture
            .app()
            .source_import_run(&fixture.manifest, KEY, 1, 1)
            .unwrap();
        assert_eq!(first.imported_items, 1);
        fs::write(&fixture.inputs[1], b"different bytes after manifest freeze").unwrap();
        let before = snapshot(&fixture.root.join("sources"));
        let error = fixture.app().source_import_resume(KEY, 1).unwrap_err();
        assert_eq!(error.details["import_item"]["ordinal"], 1);
        assert_eq!(
            error.details["import_item"]["path"],
            fs::canonicalize(&fixture.inputs[1])
                .unwrap()
                .to_str()
                .unwrap()
        );
        assert_eq!(error.details["import"]["key"], KEY);
        assert_eq!(error.details["import"]["imported_items"], 1);
        assert!(
            error
                .hint
                .as_deref()
                .unwrap()
                .contains("source import resume --key")
        );
        assert_eq!(snapshot(&fixture.root.join("sources")), before);
        let status = fixture.app().source_import_status(KEY).unwrap();
        assert_eq!(status.imported_items, 1);
        assert_eq!(status.groups_committed, 1);
        assert!(!status.completed);
        assert_eq!(status.pending_group, Some(1));
        assert_eq!(status.pending_items.len(), 1);
        assert_eq!(status.pending_items[0].ordinal, 1);
        assert_eq!(
            status.last_group.unwrap().change,
            first.last_group.unwrap().change
        );
    }
}

#[test]
fn pure_prepare_and_run_preview_do_not_write_even_with_a_held_writer() {
    let fixture = Fixture::new(false, 4);
    let existing_output = fixture.manifest.with_file_name("foreign output.jsonl");
    fs::write(&existing_output, b"foreign output stays intact").unwrap();
    let before = snapshot(fixture.temp.path());
    assert!(super::prepare_source_import(&fixture.list, &existing_output, true).is_err());
    assert_eq!(snapshot(fixture.temp.path()), before);
    let absent_output = fixture.manifest.with_file_name("preview-only output.jsonl");
    let preview = super::prepare_source_import(&fixture.list, &absent_output, true).unwrap();
    assert!(preview.preview);
    assert_eq!(preview.items, 4);
    assert_eq!(snapshot(fixture.temp.path()), before);
    fixture.prepare();
    let app = fixture.with_options(OperationOptions {
        dry_run: true,
        ..options()
    });
    let writer = WriterPermit::acquire(app.fs().root(), Duration::ZERO).unwrap();
    let before = snapshot(fixture.temp.path());
    let preview = app.source_import_run(&fixture.manifest, KEY, 4, 1).unwrap();
    assert!(preview.preview);
    assert_eq!(preview.imported_items, 0);
    assert_eq!(preview.groups_committed, 0);
    assert_eq!(preview.pending_change, None);
    assert_eq!(snapshot(fixture.temp.path()), before);
    drop(writer);
    assert!(
        fixture.app().source_import_status(KEY).is_err(),
        "preview installed import state"
    );
}

#[test]
fn invalid_bounds_and_stage_only_refuse_without_allocating_import_state() {
    let fixture = Fixture::new(false, 1);
    fixture.prepare();
    let before = snapshot(fixture.temp.path());
    for (group_size, max_groups) in [(0, 1), (9, 1), (1, 0), (1, 10_001)] {
        assert!(
            fixture
                .app()
                .source_import_run(&fixture.manifest, KEY, group_size, max_groups)
                .is_err()
        );
        assert_eq!(snapshot(fixture.temp.path()), before);
    }
    let staged = fixture.with_options(OperationOptions {
        stage_only: true,
        ..options()
    });
    assert!(
        staged
            .source_import_run(&fixture.manifest, KEY, 1, 1)
            .is_err()
    );
    assert_eq!(snapshot(fixture.temp.path()), before);
    assert!(fixture.app().source_import_status(KEY).is_err());
}

#[test]
fn completed_import_reports_historical_mapping_after_refresh_and_withdraw() {
    let fixture = Fixture::new(false, 1);
    fixture.prepare();
    let first = fixture
        .app()
        .source_import_run(&fixture.manifest, KEY, 1, 1)
        .unwrap()
        .last_group
        .unwrap();
    let item = &first.items[0];
    fixture
        .app()
        .source_refresh(
            item.source_id.clone(),
            CaptureRequest {
                title: "Refresh preserves import history".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "later-input.txt".into(),
                original: b"Updatedneedle refreshed canonical evidence\n".to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: None,
            },
        )
        .unwrap();
    fixture
        .app()
        .source_withdraw(item.source_id.clone(), "maintenance withdrawal")
        .unwrap();
    fixture.remove_originals();
    for outcome in [
        fixture.app().source_import_status(KEY).unwrap(),
        fixture.app().source_import_resume(KEY, 1).unwrap(),
    ] {
        assert!(outcome.completed);
        assert_eq!(outcome.last_group.as_ref().unwrap().change, first.change);
        assert_eq!(
            mapping(outcome.last_group.as_ref().unwrap()),
            mapping(&first)
        );
    }
    let source = fixture
        .app()
        .read(ReadRequest {
            selector: RecordSelector::Id(item.source_id.clone()),
            range: None,
            max_bytes: 4096,
        })
        .unwrap()
        .record
        .unwrap();
    assert_eq!(source.string("wiki_status"), Some("withdrawn"));
}

#[test]
fn aggregate_group_bound_splits_before_retention_and_large_scalar_exception_remains_supported() {
    let fixture = Fixture::with_inputs(
        false,
        vec![
            ("large-a.txt".into(), vec![b'a'; 2 * 1024 * 1024 + 1]),
            ("large-b.txt".into(), vec![b'b'; 2 * 1024 * 1024 + 1]),
        ],
    );
    fixture.prepare();
    let first = fixture
        .app()
        .source_import_run(&fixture.manifest, KEY, 8, 1)
        .unwrap();
    assert_eq!(first.imported_items, 1);
    assert_eq!(first.groups_committed, 1);
    assert!(!first.completed);
    let first_change = first.last_group.unwrap().change;
    assert_eq!(
        fixture
            .app()
            .changes_show(first_change.change_id.clone())
            .unwrap()
            .manifest
            .operations
            .len(),
        4
    );
    let last = fixture.app().source_import_resume(KEY, 1).unwrap();
    assert!(last.completed);
    assert_eq!(last.imported_items, 2);
    assert_eq!(last.groups_committed, 2);
    assert_ne!(
        last.last_group.unwrap().change.change_id,
        first_change.change_id
    );

    let scalar = Fixture::with_inputs(
        false,
        vec![("scalar.bin".into(), vec![b'c'; 5 * 1024 * 1024])],
    );
    scalar.prepare();
    let result = scalar
        .app()
        .source_import_run(&scalar.manifest, KEY, 8, 1)
        .unwrap();
    assert!(result.completed);
    assert_eq!(result.imported_items, 1);
    assert_eq!(result.groups_committed, 1);
    let change = scalar
        .app()
        .changes_show(result.last_group.unwrap().change.change_id)
        .unwrap();
    assert_eq!(change.status, ChangeStatus::Committed);
    assert_eq!(change.manifest.operations.len(), 3);
    // Large original admission does not waive derived-row budgets. Ordinary
    // capture and shared capture keep the same bound for duplicated text rows.
    let large_text = Fixture::with_inputs(
        false,
        vec![("large.txt".into(), vec![b'd'; 5 * 1024 * 1024])],
    );
    large_text.prepare();
    let error = large_text
        .app()
        .source_import_run(&large_text.manifest, KEY, 8, 1)
        .unwrap_err();
    assert_eq!(error.code, crate::domain::ErrorCode::BudgetExceeded);
    assert!(
        !large_text.root.join("sources").exists()
            || fs::read_dir(large_text.root.join("sources"))
                .unwrap()
                .next()
                .is_none()
    );
}

#[derive(Clone, Copy, Debug)]
enum Cut {
    BeforeManifestCopy,
    BeforeBegin,
    AfterBegin,
    BeforeProof,
    BeforeResultAppend,
    AfterResultAppend,
    BeforeCursorAcknowledgement,
    AfterIntentHeaderRename,
    OutcomeReceiptSyncError,
    OutcomeReceiptChangedDuringSync,
}

#[test]
fn missing_owned_manifest_reports_same_key_run_continuation_before_capture() {
    for migrated in [false, true] {
        let fixture = Fixture::new(migrated, 1);
        fixture.prepare();
        let adapter = Arc::new(ImportIo::new(Some(Cut::BeforeManifestCopy), vec![]));
        let app = OfflineApp::new(
            VaultFs::with_io(VaultRoot::explicit(&fixture.root).unwrap(), adapter.clone()),
            options(),
        )
        .unwrap();
        let failure = app
            .source_import_run(&fixture.manifest, KEY, 4, 1)
            .unwrap_err();
        assert!(adapter.fired.load(Ordering::SeqCst));
        assert_eq!(failure.details["import"]["key"], KEY);
        assert!(
            failure
                .hint
                .as_deref()
                .unwrap()
                .contains("source import run --manifest")
        );
        drop(app);
        let status = fixture.app().source_import_status(KEY).unwrap();
        assert_eq!(status.imported_items, 0);
        assert_eq!(status.pending_change, None);
        let failure = fixture.app().source_import_resume(KEY, 1).unwrap_err();
        assert!(failure.hint.as_deref().unwrap().contains("--group-size 4"));
        let result = fixture
            .app()
            .source_import_run(&fixture.manifest, KEY, 4, 1)
            .unwrap();
        assert!(result.completed);
        assert_eq!(result.imported_items, 1);
    }
}

fn prepared_import(
    fixture: &Fixture,
    cut: Cut,
) -> (
    PreparedChange,
    Vec<super::source_import_types::ImportPendingCapture>,
) {
    let adapter = Arc::new(ImportIo::new(Some(cut), vec![]));
    let app = OfflineApp::new(
        VaultFs::with_io(VaultRoot::explicit(&fixture.root).unwrap(), adapter.clone()),
        options(),
    )
    .unwrap();
    assert!(app.source_import_run(&fixture.manifest, KEY, 4, 1).is_err());
    assert!(adapter.fired.load(Ordering::SeqCst));
    drop(app);
    let store = super::source_import_state::ImportStore::new(
        fixture.app().fs().clone(),
        fixture.app().vault_id().clone(),
        KEY,
    )
    .unwrap();
    let (progress, _) = store.load().unwrap().unwrap();
    let pending = progress.pending.unwrap();
    let details = fixture
        .app()
        .changes_show(pending.change.change_id)
        .unwrap();
    assert_eq!(details.status, ChangeStatus::Prepared);
    (details.prepared, pending.captures)
}
fn advance_import_base(fixture: &Fixture) {
    fixture
        .app()
        .source_add(CaptureRequest {
            title: "Interleaved publication".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "interleaved.txt".into(),
            original: b"Independent source advances the catalog.\n".to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
}

#[test]
fn stale_prepared_capture_reuses_retained_inputs_and_preserves_source_revision_clock() {
    for migrated in [false, true] {
        for cut in [Cut::BeforeBegin, Cut::BeforeProof] {
            let fixture = Fixture::new(migrated, 4);
            fixture.prepare();
            let (old_change, captures) = prepared_import(&fixture, cut);
            let details = fixture
                .app()
                .changes_show(old_change.change_id.clone())
                .unwrap();
            let old_payloads: Vec<_> = details
                .manifest
                .operations
                .iter()
                .filter_map(|op| op.after_payload.as_ref())
                .map(|payload| {
                    (
                        payload.path.clone(),
                        fs::read(fixture.physical(&payload.path)).unwrap(),
                    )
                })
                .collect();
            advance_import_base(&fixture);
            fixture.remove_originals();
            let result = fixture.app().source_import_resume(KEY, 1).unwrap();
            assert!(result.completed);
            assert_eq!(result.groups_committed, 1);
            let group = result.last_group.unwrap();
            assert_ne!(group.change.change_id, old_change.change_id);
            assert_eq!(
                fixture
                    .app()
                    .changes_show(old_change.change_id)
                    .unwrap()
                    .status,
                ChangeStatus::Aborted
            );
            for (item, capture) in group.items.iter().zip(&captures) {
                assert_eq!(item.source_id, capture.allocation.source_id);
                assert_eq!(item.revision_id, capture.allocation.revision_id);
                fixture.assert_item(item.ordinal, &item.source_id, &item.revision_id, true);
                let revision = fixture
                    .app()
                    .read(ReadRequest {
                        selector: RecordSelector::Id(item.revision_id.clone()),
                        range: None,
                        max_bytes: 16_384,
                    })
                    .unwrap()
                    .record
                    .unwrap();
                assert_eq!(
                    revision.string("wiki_captured_at"),
                    Some(capture.allocation.captured_at.as_str())
                );
            }
            for (path, bytes) in old_payloads {
                assert_eq!(fs::read(fixture.physical(&path)).unwrap(), bytes);
            }
            assert_eq!(
                fixture
                    .app()
                    .source_import_resume(KEY, 1)
                    .unwrap()
                    .groups_committed,
                1
            );
        }
    }
}

#[test]
fn proof_less_aborted_attempt_replays_lost_close_ack_and_preserves_input_anchor() {
    for migrated in [false, true] {
        let fixture = Fixture::new(migrated, 4);
        fixture.prepare();
        let (old_change, captures) = prepared_import(&fixture, Cut::BeforeProof);
        advance_import_base(&fixture);
        fixture.remove_originals();
        let adapter = Arc::new(ImportIo::new(Some(Cut::BeforeResultAppend), vec![]));
        let app = OfflineApp::new(
            VaultFs::with_io(VaultRoot::explicit(&fixture.root).unwrap(), adapter.clone()),
            options(),
        )
        .unwrap();
        assert!(app.source_import_resume(KEY, 1).is_err());
        assert!(adapter.fired.load(Ordering::SeqCst));
        drop(app);
        assert_eq!(
            fixture
                .app()
                .source_import_status(KEY)
                .unwrap()
                .pending_change,
            Some(old_change.change_id.clone())
        );
        assert_eq!(
            fixture
                .app()
                .changes_show(old_change.change_id.clone())
                .unwrap()
                .status,
            ChangeStatus::Aborted
        );
        let resumed = fixture.app().source_import_resume(KEY, 1).unwrap();
        assert!(resumed.completed);
        let group = resumed.last_group.unwrap();
        let durable_snapshot = || {
            let mut entries = preservation_snapshot(&fixture.root);
            // Successful SQLite readers can update shared-memory read marks.
            // Canonical/state files, database/WAL bytes and publication stay guarded.
            entries.retain(|path, _| !path.to_string_lossy().ends_with(".sqlite-shm"));
            entries
        };
        let before = durable_snapshot();
        let publication =
            Catalog::new(fixture.app().fs().clone(), fixture.app().vault_id().clone())
                .query_snapshot(QueryReadLimits::default())
                .unwrap()
                .snapshot()
                .clone();
        for _ in 0..2 {
            let recovered = fixture.app().recover().unwrap().report.unwrap();
            assert!(recovered.changes.iter().any(
                |report| report.change == old_change && report.status == ChangeStatus::Aborted
            ));
            assert!(
                recovered
                    .changes
                    .iter()
                    .any(|report| report.change == group.change
                        && report.status == ChangeStatus::Committed)
            );
            assert!(recovered.staged.is_empty());
        }
        let historical = fixture
            .app()
            .changes_apply(old_change.change_id.clone())
            .unwrap();
        assert_eq!(historical.status, Some(ChangeStatus::Aborted));
        assert!(historical.reused);
        let after = durable_snapshot();
        let changed: Vec<_> = before
            .keys()
            .chain(after.keys())
            .filter(|path| before.get(*path) != after.get(*path))
            .collect();
        assert!(
            changed.is_empty(),
            "historical replay changed durable paths: {changed:?}"
        );
        assert_eq!(
            Catalog::new(fixture.app().fs().clone(), fixture.app().vault_id().clone())
                .query_snapshot(QueryReadLimits::default())
                .unwrap()
                .snapshot()
                .clone(),
            publication
        );
        for (item, capture) in group.items.iter().zip(captures) {
            assert_eq!(item.source_id, capture.allocation.source_id);
            assert_eq!(item.revision_id, capture.allocation.revision_id);
            fixture.assert_item(item.ordinal, &item.source_id, &item.revision_id, true);
        }
        assert_eq!(fixture.app().check().unwrap().error_count, 0);
    }
}

#[test]
fn completed_progress_refuses_missing_or_truncated_result_mappings() {
    for truncate in [false, true] {
        let fixture = Fixture::new(false, 1);
        fixture.prepare();
        let imported = fixture
            .app()
            .source_import_run(&fixture.manifest, KEY, 4, 1)
            .unwrap();
        assert!(imported.completed);
        let results = fixture.physical(&imported.results_path);
        if truncate {
            fs::write(results, b"short").unwrap();
        } else {
            fs::remove_file(results).unwrap();
        }
        let before = snapshot(&fixture.root);
        assert!(fixture.app().source_import_status(KEY).is_err());
        assert!(fixture.app().source_import_resume(KEY, 1).is_err());
        assert_eq!(snapshot(&fixture.root), before);
    }
}

#[test]
fn absent_input_anchor_preserves_original_v1_progress_checksum_encoding() {
    let fixture = Fixture::new(false, 1);
    fixture.prepare();
    prepared_import(&fixture, Cut::BeforeProof);
    let stem = Blake3Hash::digest(KEY.as_bytes()).hex().to_owned();
    let bytes = fs::read(
        fixture
            .root
            .join(format!(".wiki/state/source-imports/{stem}.json")),
    )
    .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(value["progress"]["pending"].get("input_intent").is_none());
    let envelope: super::source_import_types::ImportStateEnvelope =
        serde_json::from_slice(&bytes).unwrap();
    assert!(
        envelope
            .progress
            .pending
            .as_ref()
            .unwrap()
            .input_intent
            .is_none()
    );
    assert_eq!(serde_json::to_vec(&envelope).unwrap(), bytes);
    assert_eq!(
        Blake3Hash::digest(serde_json::to_vec(&envelope.progress).unwrap()),
        envelope.checksum
    );
}
struct ImportIo {
    cut: Option<Cut>,
    fired: AtomicBool,
    results_open: AtomicBool,
    header_replaced: AtomicBool,
    controls: Vec<PathBuf>,
    synced_controls: Mutex<BTreeSet<PathBuf>>,
}
impl ImportIo {
    fn new(cut: Option<Cut>, controls: Vec<PathBuf>) -> Self {
        Self {
            cut,
            fired: AtomicBool::new(false),
            results_open: AtomicBool::new(false),
            header_replaced: AtomicBool::new(false),
            controls,
            synced_controls: Mutex::new(BTreeSet::new()),
        }
    }
    fn fail(&self) -> std::io::Result<()> {
        self.fired.store(true, Ordering::SeqCst);
        Err(std::io::Error::other(
            "injected public import returned error",
        ))
    }
}
impl DurableIo for ImportIo {
    fn create_stage(&self, p: &Path) -> std::io::Result<File> {
        NativeIo.create_stage(p)
    }
    fn create_private_stage(&self, p: &Path) -> std::io::Result<File> {
        NativeIo.create_private_stage(p)
    }
    fn create_private_directory(&self, p: &Path) -> std::io::Result<()> {
        NativeIo.create_private_directory(p)
    }
    fn open_append(&self, p: &Path) -> std::io::Result<File> {
        if p.file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".results.jsonl"))
        {
            if matches!(self.cut, Some(Cut::BeforeResultAppend)) {
                self.fail()?;
            }
            self.results_open.store(true, Ordering::SeqCst);
        }
        NativeIo.open_append(p)
    }
    fn truncate_file(&self, f: &File, n: u64) -> std::io::Result<()> {
        NativeIo.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut File, b: &[u8]) -> std::io::Result<()> {
        NativeIo.write_stage(f, b)
    }
    fn sync_file(&self, f: &File) -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        NativeIo.sync_file(f)?;
        let file = f.metadata()?;
        for path in &self.controls {
            if let Ok(named) = fs::metadata(path)
                && named.dev() == file.dev()
                && named.ino() == file.ino()
            {
                self.synced_controls.lock().unwrap().insert(path.clone());
                if path.file_name().is_some_and(|name| name == "outcome.json")
                    && matches!(
                        self.cut,
                        Some(Cut::OutcomeReceiptSyncError | Cut::OutcomeReceiptChangedDuringSync)
                    )
                    && !self.fired.swap(true, Ordering::SeqCst)
                {
                    if matches!(self.cut, Some(Cut::OutcomeReceiptSyncError)) {
                        return self.fail();
                    }
                    let mut receipt: Value = serde_json::from_slice(&fs::read(path)?)?;
                    receipt["checksum"] = json!(Blake3Hash::digest(b"changed during receipt sync"));
                    fs::write(path, serde_json::to_vec(&receipt)?)?;
                }
            }
        }
        Ok(())
    }
    fn replace(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        let name = to.file_name().unwrap().to_string_lossy();
        if matches!(self.cut, Some(Cut::BeforeManifestCopy)) && name.ends_with(".manifest.jsonl") {
            return self.fail();
        }
        if self.controls.len() == 2
            && self.controls[1]
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".results.jsonl"))
            && to == self.controls[0]
        {
            let next: Value = serde_json::from_slice(&fs::read(from)?).unwrap();
            if next["progress"]["groups_committed"]
                .as_u64()
                .is_some_and(|count| count > 0)
            {
                assert!(
                    self.synced_controls
                        .lock()
                        .unwrap()
                        .contains(&self.controls[1]),
                    "cursor acknowledged before re-syncing the observed complete result frame"
                );
            }
        }
        if matches!(self.cut, Some(Cut::BeforeBegin)) && name == "operations.json" {
            let next: Value = serde_json::from_slice(&fs::read(from)?).unwrap();
            if next.get("active").is_some_and(|active| !active.is_null()) {
                return self.fail();
            }
        }
        if matches!(self.cut, Some(Cut::BeforeProof)) && name == "validation.json" {
            return self.fail();
        }
        if matches!(self.cut, Some(Cut::BeforeCursorAcknowledgement))
            && to.parent().is_some_and(|p| p.ends_with("source-imports"))
            && name.ends_with(".json")
        {
            let next: Value = serde_json::from_slice(&fs::read(from)?).unwrap();
            if next["progress"]["groups_committed"]
                .as_u64()
                .is_some_and(|count| count > 0)
            {
                return self.fail();
            }
        }
        if !self.controls.is_empty()
            && to
                .components()
                .any(|part| part.as_os_str() == "changes" || part.as_os_str() == "objects")
        {
            let synced = self.synced_controls.lock().unwrap();
            assert!(
                self.controls.iter().all(|path| synced.contains(path)),
                "proposal retained before observed header and intent were durably refreshed: {to:?}"
            );
        }
        NativeIo.replace(from, to)?;
        if matches!(self.cut, Some(Cut::AfterBegin)) && name == "operations.json" {
            let next: Value = serde_json::from_slice(&fs::read(to)?).unwrap();
            if next.get("active").is_some_and(|active| !active.is_null()) {
                return self.fail();
            }
        }
        if self.controls.first().is_some_and(|header| to == header) {
            self.header_replaced.store(true, Ordering::SeqCst);
        }
        if matches!(self.cut, Some(Cut::AfterIntentHeaderRename))
            && to.parent().is_some_and(|p| p.ends_with("source-imports"))
            && name.ends_with(".json")
        {
            let next: Value = serde_json::from_slice(&fs::read(to)?).unwrap();
            if next["progress"]["pending"]["intent"].is_object() {
                return self.fail();
            }
        }
        Ok(())
    }
    fn remove(&self, p: &Path) -> std::io::Result<()> {
        NativeIo.remove(p)
    }
    fn create_directory(&self, p: &Path) -> std::io::Result<()> {
        NativeIo.create_directory(p)
    }
    fn sync_directory(&self, p: &Path) -> std::io::Result<DirectorySync> {
        if matches!(self.cut, Some(Cut::AfterResultAppend))
            && self.results_open.swap(false, Ordering::SeqCst)
        {
            self.fail()?;
        }
        let result = NativeIo.sync_directory(p)?;
        if result == DirectorySync::Supported
            && self
                .controls
                .first()
                .is_some_and(|header| header.parent() == Some(p))
            && self.header_replaced.swap(false, Ordering::SeqCst)
        {
            self.synced_controls
                .lock()
                .unwrap()
                .insert(self.controls[0].clone());
        }
        Ok(result)
    }
}

#[test]
fn retained_proof_or_lost_committed_ack_reopens_without_originals_or_duplicate_publication() {
    for migrated in [false, true] {
        for cut in [
            Cut::BeforeBegin,
            Cut::AfterBegin,
            Cut::BeforeProof,
            Cut::BeforeResultAppend,
            Cut::AfterResultAppend,
            Cut::BeforeCursorAcknowledgement,
        ] {
            let fixture = Fixture::new(migrated, 4);
            fixture.prepare();
            let io = Arc::new(ImportIo::new(Some(cut), vec![]));
            let app = OfflineApp::new(
                VaultFs::with_io(VaultRoot::explicit(&fixture.root).unwrap(), io.clone()),
                options(),
            )
            .unwrap();
            assert!(
                app.source_import_run(&fixture.manifest, KEY, 4, 1).is_err(),
                "{migrated}/{cut:?}"
            );
            assert!(
                io.fired.load(Ordering::SeqCst),
                "cut never reached: {migrated}/{cut:?}"
            );
            drop(app);
            let status = fixture.app().source_import_status(KEY).unwrap();
            let pending = status
                .pending_change
                .expect("error lost known attempt identity");
            let before_change = fixture.app().changes_show(pending.clone()).unwrap();
            assert_eq!(
                before_change.status,
                if matches!(cut, Cut::BeforeBegin | Cut::AfterBegin | Cut::BeforeProof) {
                    ChangeStatus::Prepared
                } else {
                    ChangeStatus::Committed
                }
            );
            let prepared: PreparedChange = before_change.prepared;
            fixture.remove_originals();
            let resumed = if matches!(
                cut,
                Cut::AfterResultAppend | Cut::BeforeCursorAcknowledgement
            ) {
                let stem = format!(
                    ".wiki/state/source-imports/{}",
                    Blake3Hash::digest(KEY.as_bytes()).hex()
                );
                let controls = vec![
                    fixture.physical(&rel(format!("{stem}.json"))),
                    fixture.physical(&rel(format!("{stem}.results.jsonl"))),
                ];
                let observe = Arc::new(ImportIo::new(None, controls));
                let reopened = OfflineApp::new(
                    VaultFs::with_io(VaultRoot::explicit(&fixture.root).unwrap(), observe.clone()),
                    options(),
                )
                .unwrap();
                let result = reopened.source_import_resume(KEY, 1).unwrap();
                assert!(
                    observe
                        .synced_controls
                        .lock()
                        .unwrap()
                        .contains(&observe.controls[1])
                );
                result
            } else {
                fixture.app().source_import_resume(KEY, 1).unwrap()
            };
            assert!(resumed.completed);
            assert_eq!(resumed.imported_items, 4);
            assert_eq!(resumed.groups_committed, 1);
            let group = resumed.last_group.unwrap();
            assert_eq!(group.change, prepared);
            assert_eq!(
                fs::read_dir(fixture.root.join("sources")).unwrap().count(),
                4
            );
            for item in &group.items {
                fixture.assert_item(item.ordinal, &item.source_id, &item.revision_id, true);
            }
            let replay = fixture.app().source_import_resume(KEY, 1).unwrap();
            assert_eq!(replay.last_group.as_ref().unwrap().change, group.change);
            assert_eq!(replay.groups_committed, 1);
            assert_eq!(
                mapping(replay.last_group.as_ref().unwrap()),
                mapping(&group)
            );
            let checked = fixture.app().check().unwrap();
            assert!(checked.complete);
            assert_eq!(
                checked.error_count, 0,
                "{migrated}/{cut:?}: {:?}",
                checked.diagnostics
            );
        }
    }
}

#[test]
fn returned_ready_intent_header_rename_resyncs_exact_controls_before_retention() {
    for migrated in [false, true] {
        let fixture = Fixture::new(migrated, 4);
        fixture.prepare();
        let fault = Arc::new(ImportIo::new(Some(Cut::AfterIntentHeaderRename), vec![]));
        let app = OfflineApp::new(
            VaultFs::with_io(VaultRoot::explicit(&fixture.root).unwrap(), fault.clone()),
            options(),
        )
        .unwrap();
        assert!(app.source_import_run(&fixture.manifest, KEY, 4, 1).is_err());
        assert!(fault.fired.load(Ordering::SeqCst));
        drop(app);
        let pending = fixture
            .app()
            .source_import_status(KEY)
            .unwrap()
            .pending_change
            .unwrap();
        assert!(
            !fixture
                .physical(&rel(format!("changes/{pending}/change.md")))
                .exists()
        );
        let header = fixture.physical(&rel(format!(
            ".wiki/state/source-imports/{}.json",
            Blake3Hash::digest(KEY.as_bytes()).hex()
        )));
        // Direct known-key evidence only; no importer/history discovery scan.
        let retained: Value = serde_json::from_slice(&fs::read(&header).unwrap()).unwrap();
        let reference = retained["progress"]["pending"]["intent"]["path"]
            .as_str()
            .unwrap();
        let intent = fixture.physical(&rel(reference));
        let intent_before = fs::read(&intent).unwrap();
        let expected_pairs: Vec<(RecordId, RecordId)> = retained["progress"]["pending"]["captures"]
            .as_array()
            .unwrap()
            .iter()
            .map(|capture| {
                (
                    serde_json::from_value(capture["allocation"]["source_id"].clone()).unwrap(),
                    serde_json::from_value(capture["allocation"]["revision_id"].clone()).unwrap(),
                )
            })
            .collect();
        let observe = Arc::new(ImportIo::new(None, vec![header, intent.clone()]));
        let reopened = OfflineApp::new(
            VaultFs::with_io(VaultRoot::explicit(&fixture.root).unwrap(), observe.clone()),
            options(),
        )
        .unwrap();
        let resumed = reopened.source_import_resume(KEY, 1).unwrap();
        assert!(resumed.completed);
        let group = resumed.last_group.unwrap();
        assert_eq!(group.change.change_id, pending);
        assert_eq!(
            group
                .items
                .iter()
                .map(|item| (item.source_id.clone(), item.revision_id.clone()))
                .collect::<Vec<_>>(),
            expected_pairs
        );
        assert_eq!(fs::read(intent).unwrap(), intent_before);
        assert_eq!(observe.synced_controls.lock().unwrap().len(), 2);
    }
}

#[test]
fn unindexed_vault_preview_creates_no_cache_import_state_or_writer_lock() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("unindexed vault");
    init(&root, "Unindexed import preview", options()).unwrap();
    let input = temp.path().join("outside.txt");
    fs::write(&input, b"Unindexed preview evidence\n").unwrap();
    let list = temp.path().join("inputs.jsonl");
    fs::write(&list, format!("{}\n", json!({"path":input}))).unwrap();
    let manifest = temp.path().join("manifest.jsonl");
    super::prepare_source_import(&list, &manifest, false).unwrap();
    let before = snapshot(temp.path());
    let app = OfflineApp::new(
        VaultFs::new(VaultRoot::explicit(root).unwrap()),
        OperationOptions {
            dry_run: true,
            ..options()
        },
    )
    .unwrap();
    let result = app.source_import_run(&manifest, KEY, 4, 1).unwrap();
    assert!(result.preview);
    assert_eq!(result.total_items, 1);
    assert_eq!(result.imported_items, 0);
    assert_eq!(result.pending_change, None);
    assert_eq!(snapshot(temp.path()), before);
    let writer_app = OfflineApp::new(app.fs().clone(), options()).unwrap();
    let error = writer_app
        .source_import_run(&manifest, KEY, 4, 1)
        .unwrap_err();
    assert_eq!(error.code, crate::domain::ErrorCode::CapabilityUnavailable);
    assert!(error.message.contains("index rebuild --normalized"));
    assert!(
        !app.fs()
            .root()
            .path()
            .join(".wiki/state/source-imports")
            .exists()
    );
}

#[test]
fn public_import_preserves_external_reader_across_run_resume_and_replay() {
    for migrated in [false, true] {
        let fixture = Fixture::new(migrated, 8);
        fixture.prepare();
        let app = fixture.app();
        let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
        let old = catalog
            .query_snapshot(QueryReadLimits {
                max_elapsed_ms: 30_000,
                ..QueryReadLimits::default()
            })
            .unwrap();
        let base = old.snapshot().clone();
        let wiki = rel("WIKI.md");
        let before = old.document(&wiki).unwrap();
        let first = app.source_import_run(&fixture.manifest, KEY, 4, 1).unwrap();
        assert!(!first.completed);
        let second = app.source_import_resume(KEY, 1).unwrap();
        assert!(second.completed);
        assert_eq!(second.groups_committed, 2);
        let mut items = mapping(first.last_group.as_ref().unwrap());
        items.extend(mapping(second.last_group.as_ref().unwrap()));
        assert_eq!(
            items.iter().map(|item| item.0).collect::<Vec<_>>(),
            (0..8).collect::<Vec<_>>()
        );
        assert_eq!(old.snapshot(), &base);
        assert_eq!(old.document(&wiki).unwrap(), before);
        for (ordinal, source, revision) in &items {
            assert!(old.record(source).unwrap().is_none());
            assert!(old.record(revision).unwrap().is_none());
            fixture.assert_item(*ordinal, source, revision, true);
        }
        fixture.assert_query(items[0].0, &items[0].1, &items[0].2);
        fixture.assert_query(items[7].0, &items[7].1, &items[7].2);
        drop(old);
        fixture.remove_originals();
        let replay = app.source_import_resume(KEY, 1).unwrap();
        assert_eq!(replay.imported_items, 8);
        assert_eq!(replay.groups_committed, 2);
        assert_eq!(
            mapping(replay.last_group.as_ref().unwrap()),
            mapping(second.last_group.as_ref().unwrap())
        );
        let checked = app.check().unwrap();
        assert!(checked.complete);
        assert_eq!(checked.error_count, 0, "{:?}", checked.diagnostics);
    }
}
