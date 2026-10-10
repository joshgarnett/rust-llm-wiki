//! Ordinary normalized Page replacement rollback/apply; disposable vaults only.
//! Legacy exact-ID adoption uses the old public staging path; binary-pair
//! compatibility and crash qualification remain separate gates.
use super::{OfflineApp, OperationOptions, ReadRequest, RecordSelector, offline::init};
use crate::{
    catalog::{
        CatalogGraphValidator, query_types::QueryReadLimits,
        source_projection::RefreshProjectionLimits, source_refresh::IndexedRefreshSession,
        write_projection::project_page_inverse,
    },
    changes::{
        ChangeDraft, ChangeEngine, ChangeEvent, ChangeInspection, ChangeStatus, ExpectedWrite,
        PreparedChange,
    },
    domain::{Blake3Hash, ErrorCode, VaultRelativePath},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
    storage::{self, StorageOptions},
    vault::{DurableIo, VaultFs, VaultRoot, WriterPermit},
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

const PAGE: &str = "pages/rollback-guide.md";
const AUTHOR: &str = "Author sentinel café 東京: keep my explanation.\n";
const MARKER: &str = "GenuineRollbackMarker016: the author adds a new instruction.\n";
const UNFAMILIAR: &str = "UnfamiliarAuthor016: preserve this external edit.\n";

fn rel(value: impl Into<String>) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}

fn options() -> OperationOptions {
    OperationOptions {
        offline: true,
        lock_timeout_ms: 200,
        ..Default::default()
    }
}

fn page() -> Vec<u8> {
    format!(
        "---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: page_rollback_guide\ntitle: Rollback guide\nwiki_status: reviewed\n---\n# Guide\n{AUTHOR}"
    )
    .into_bytes()
}

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    directory: bool,
    bytes: Vec<u8>,
    modified: SystemTime,
}

// This tiny-fixture observation includes directories, SQL sidecars and all
// operational state. Only a real writer's documented lock diagnostic is omitted
// for terminal retries; dry-run observations omit nothing.
fn tree(root: &Path, omit_writer_lock: bool) -> BTreeMap<PathBuf, Entry> {
    fn visit(
        root: &Path,
        current: &Path,
        omit_writer_lock: bool,
        entries: &mut BTreeMap<PathBuf, Entry>,
    ) {
        let relative = current.strip_prefix(root).unwrap();
        if omit_writer_lock && relative == Path::new(".wiki/state/writer.lock") {
            return;
        }
        let metadata = fs::symlink_metadata(current).unwrap();
        assert!(!metadata.file_type().is_symlink());
        assert!(metadata.is_file() || metadata.is_dir());
        entries.insert(
            relative.to_owned(),
            Entry {
                directory: metadata.is_dir(),
                bytes: if metadata.is_file() {
                    fs::read(current).unwrap()
                } else {
                    vec![]
                },
                modified: metadata.modified().unwrap(),
            },
        );
        if metadata.is_dir() {
            for child in fs::read_dir(current).unwrap() {
                visit(root, &child.unwrap().path(), omit_writer_lock, entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, omit_writer_lock, &mut entries);
    entries
}

// Keep the exact inventory/bytes/mtime oracle, but report only changed paths
// and bounded content summaries rather than dumping every SQLite byte.
fn assert_tree_unchanged(root: &Path, omit_writer_lock: bool, before: &BTreeMap<PathBuf, Entry>) {
    let after = tree(root, omit_writer_lock);
    if &after == before {
        return;
    }
    let paths = before
        .keys()
        .chain(after.keys())
        .collect::<std::collections::BTreeSet<_>>();
    let changed = paths
        .into_iter()
        .filter(|path| before.get(*path) != after.get(*path))
        .collect::<Vec<_>>();
    let summary = |entry: Option<&Entry>| {
        entry.map(|entry| {
            (
                entry.directory,
                entry.bytes.len(),
                Blake3Hash::digest(&entry.bytes),
                entry.modified,
            )
        })
    };
    let details = changed
        .iter()
        .take(16)
        .map(|path| (path, summary(before.get(*path)), summary(after.get(*path))))
        .collect::<Vec<_>>();
    panic!(
        "complete tree changed at {} paths (first 16; path, before, after): {details:?}",
        changed.len()
    );
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    original: Vec<u8>,
    source_history: BTreeMap<VaultRelativePath, Vec<u8>>,
}

impl Fixture {
    fn new(retained: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("normalized rollback vault with spaces");
        init(&root, "Normalized rollback", options()).unwrap();
        let vault = VaultFs::new(VaultRoot::explicit(&root).unwrap());
        if retained {
            let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
            storage::cleanup(&vault, &writer, &StorageOptions::default()).unwrap();
        }
        let app = OfflineApp::new(vault, options()).unwrap();
        app.index_rebuild_normalized().unwrap();
        assert_eq!(storage::layout::active(app.fs().root()).unwrap(), retained);
        let captured = app
            .source_add(CaptureRequest {
                title: "Unrelated immutable history".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "disposable-history.txt".into(),
                original: b"UnrelatedHistory016: exact original capture.\n".to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: Some("text/plain".into()),
            })
            .unwrap();
        let source = &captured.allocated_ids["source"];
        let revision = &captured.allocated_ids["revision"];
        let history_paths = [
            rel(format!("sources/{source}/source.md")),
            rel(format!("sources/{source}/revisions/{revision}/revision.md")),
            rel(format!(
                "sources/{source}/revisions/{revision}/original.bin"
            )),
            rel(format!("sources/{source}/revisions/{revision}/content.md")),
        ];
        let source_history = history_paths
            .into_iter()
            .map(|path| {
                let physical = app.fs().root().resolve(&path).unwrap();
                (path, fs::read(physical).unwrap())
            })
            .collect();
        let original = page();
        app.page_put(rel(PAGE), original.clone(), None).unwrap();
        Self {
            _temp: temp,
            root,
            original,
            source_history,
        }
    }

    fn app(&self) -> OfflineApp {
        self.with_options(options())
    }

    fn with_options(&self, options: OperationOptions) -> OfflineApp {
        OfflineApp::new(
            VaultFs::new(VaultRoot::explicit(&self.root).unwrap()),
            options,
        )
        .unwrap()
    }

    fn physical(&self, path: &VaultRelativePath) -> PathBuf {
        self.app().fs().root().resolve(path).unwrap()
    }

    fn bytes(&self) -> Vec<u8> {
        fs::read(self.physical(&rel(PAGE))).unwrap()
    }

    fn publication(&self) -> (String, u64) {
        let authority = self.app().catalog().operation_state().unwrap().unwrap();
        (
            authority.publication().file_id.clone(),
            authority.publication().epoch,
        )
    }

    fn assert_source_history(&self) {
        for (path, bytes) in &self.source_history {
            assert_eq!(fs::read(self.physical(path)).unwrap(), *bytes, "{path}");
        }
    }

    fn assert_page(&self, expected: &[u8]) {
        assert_eq!(self.bytes(), expected);
        let read = self
            .app()
            .read(ReadRequest {
                selector: RecordSelector::Path(rel(PAGE)),
                range: None,
                max_bytes: 8192,
            })
            .unwrap();
        assert_eq!(read.path, rel(PAGE));
        assert_eq!(read.hash, Blake3Hash::digest(expected));
        assert_eq!(read.record.unwrap().id().as_str(), "page_rollback_guide");
        assert!(read.body.contains(AUTHOR));
        assert!(!read.truncated);
        assert!(read.continuation.is_none());
        self.assert_source_history();
    }

    fn marker_change(&self) -> (PreparedChange, Vec<u8>) {
        let mut marked = self.original.clone();
        marked.extend_from_slice(MARKER.as_bytes());
        let before = self.publication();
        let changed = self
            .app()
            .page_put(
                rel(PAGE),
                marked.clone(),
                Some(Blake3Hash::digest(&self.original)),
            )
            .unwrap();
        assert_eq!(changed.status, Some(ChangeStatus::Committed));
        assert_eq!(changed.snapshot.unwrap().generation, before.1 + 1);
        self.assert_page(&marked);
        (changed.change.unwrap(), marked)
    }

    fn rollback(&self, parent: &PreparedChange, marked: &[u8]) -> PreparedChange {
        let before = self.publication();
        let staged = self
            .app()
            .changes_rollback(parent.change_id.clone())
            .unwrap();
        assert_eq!(staged.status, Some(ChangeStatus::Prepared));
        assert!(staged.snapshot.is_none());
        let inverse = staged.change.unwrap();
        assert_ne!(inverse.change_id, parent.change_id);
        let details = self.app().changes_show(inverse.change_id.clone()).unwrap();
        assert_eq!(details.prepared, inverse);
        assert_eq!(
            details.manifest.inverse_of.as_ref(),
            Some(&parent.change_id)
        );
        assert_eq!(details.manifest.operations.len(), 1);
        assert_eq!(details.manifest.operations[0].target, rel(PAGE));
        assert!(!details.manifest.read_preconditions.is_empty());
        assert_eq!(details.payloads.len(), 1);
        assert_eq!(details.payloads[0].before.as_deref(), Some(marked));
        assert_eq!(
            details.payloads[0].proposed.as_deref(),
            Some(self.original.as_slice())
        );
        assert!(details.omitted_payloads.is_empty());
        assert!(details.unavailable_payloads.is_empty());
        assert_eq!(details.frames.len(), 1);
        assert_eq!(details.frames[0].event, ChangeEvent::Prepared);
        assert_eq!(self.publication(), before);
        self.assert_page(marked);
        inverse
    }
}

fn indexed_and_committed(details: &super::ChangeDetails) -> (usize, usize) {
    (
        details
            .frames
            .iter()
            .filter(|frame| matches!(frame.event, ChangeEvent::Indexed { .. }))
            .count(),
        details
            .frames
            .iter()
            .filter(|frame| frame.event == ChangeEvent::Committed)
            .count(),
    )
}

fn old_public_path_inverse(f: &Fixture, parent: &PreparedChange) -> ChangeInspection {
    let app = f.app();
    let engine = ChangeEngine::new(app.fs().clone()).unwrap();
    // Exactly the old public changes_rollback path: authenticated inverse
    // plan, legacy graph validation, then ordinary retained preparation.
    // No edited manifest, invented proof or inconsistent payload fixture.
    let draft = engine.inverse_plan(parent).unwrap().draft;
    engine
        .validate_draft_graph(&draft, &CatalogGraphValidator)
        .unwrap();
    let writer = WriterPermit::acquire(app.fs().root(), Duration::ZERO).unwrap();
    engine.prepare(&writer, draft).unwrap()
}

fn retain_legacy_admission(f: &Fixture, change: &PreparedChange) {
    let app = f.app();
    let catalog = app.catalog();
    let writer = WriterPermit::acquire(app.fs().root(), Duration::ZERO).unwrap();
    let session = IndexedRefreshSession::adopt_legacy_page(&catalog, &writer, change).unwrap();
    assert_eq!(session.proof().version, 4);
    assert_eq!(&session.proof().change, change);
    drop(session);
    drop(writer);
}

#[test]
fn legacy_page_admission_authenticated_envelope_refuses_parent_prefix_rows_base_and_unsafe_guard_union()
 {
    use crate::catalog::source_refresh::{RetainedDelta, rebind_legacy_delta_for_test};
    for retained in [false, true] {
        for case in [
            "parent",
            "prefix",
            "rows",
            "base",
            "reserved-guard",
            "case-guard",
            "ancestor-guard",
            "separate-delta",
            "corrupt-envelope",
        ] {
            let f = Fixture::new(retained);
            let (parent, marked) = f.marker_change();
            let legacy = old_public_path_inverse(&f, &parent);
            retain_legacy_admission(&f, &legacy.prepared);
            let app = f.app();
            let engine = ChangeEngine::new(app.fs().clone()).unwrap();
            let (mut proof, embedded) = engine
                .load_indexed_refresh_retention(&legacy.prepared)
                .unwrap()
                .unwrap();
            let mut value = serde_json::to_value(embedded.unwrap()).unwrap();
            let wrong_hash =
                serde_json::to_value(Blake3Hash::digest(b"adversarial wrong retained binding017"))
                    .unwrap();
            match case {
                "parent" => value["legacy_page_admission"]["parent"]["manifest_hash"] = wrong_hash,
                "prefix" => value["legacy_page_admission"]["prepared_journal_hash"] = wrong_hash,
                "rows" => {
                    let document = value["rows"]["documents"]
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .find(|row| row["action"] == "put" && row["row"]["path"] == PAGE)
                        .expect("genuine Page projection retains its document row");
                    document["row"]["path"] = serde_json::json!("pages/unauthorized-row017.md");
                }
                "base" => {
                    proof.base.generation += 1;
                    value["base"] = serde_json::to_value(&proof.base).unwrap();
                }
                "reserved-guard" | "case-guard" | "ancestor-guard" => {
                    let guard = rel(match case {
                        "reserved-guard" => ".wiki/state/forbidden.md",
                        "case-guard" => "wiki.md",
                        _ => "pages/rollback-guide.md/child.md",
                    });
                    let dependency = crate::changes::ReadDependency {
                        path: guard,
                        expected: crate::vault::ExpectedState::Absent,
                    };
                    proof.before.push(dependency.clone());
                    proof.after.push(dependency);
                    proof.before.sort_by(|a, b| a.path.cmp(&b.path));
                    proof.after.sort_by(|a, b| a.path.cmp(&b.path));
                    value["before"] = serde_json::to_value(&proof.before).unwrap();
                    value["after"] = serde_json::to_value(&proof.after).unwrap();
                }
                _ => {}
            }
            let delta: RetainedDelta = serde_json::from_value(value).unwrap();
            // Use the production canonical delta/intended binding. The fixture
            // writes authenticated malformed authority directly; it never calls
            // this a valid producer or passes it through a production constructor.
            rebind_legacy_delta_for_test(&mut proof, &delta).unwrap();
            let embedded_delta = Some(&delta);
            let checksum =
                Blake3Hash::digest(serde_json::to_vec(&(&proof, &embedded_delta)).unwrap());
            #[derive(serde::Serialize)]
            struct AdversarialEnvelope<'a> {
                proof: &'a crate::changes::indexed_refresh::IndexedRefreshProof,
                embedded_delta: Option<&'a RetainedDelta>,
                checksum: Blake3Hash,
            }
            let envelope = AdversarialEnvelope {
                proof: &proof,
                embedded_delta,
                checksum,
            };
            let path = f.physical(
                &crate::changes::indexed_refresh::baseline_path(&legacy.prepared).unwrap(),
            );
            let mut bytes = serde_json::to_vec(&envelope).unwrap();
            if case == "corrupt-envelope" {
                bytes.pop();
            }
            fs::write(path, bytes).unwrap();
            if case == "separate-delta" {
                fs::write(
                    f.physical(
                        &crate::catalog::source_refresh::delta_path(&legacy.prepared).unwrap(),
                    ),
                    b"{}",
                )
                .unwrap();
            }
            let before = tree(&f.root, false);
            assert!(
                f.with_options(OperationOptions {
                    dry_run: true,
                    ..options()
                })
                .changes_apply(legacy.prepared.change_id.clone())
                .is_err(),
                "{case}"
            );
            assert_tree_unchanged(&f.root, false, &before);
            let before = tree(&f.root, true);
            assert!(
                app.changes_apply(legacy.prepared.change_id.clone())
                    .is_err(),
                "{case}"
            );
            if case != "base" {
                assert_tree_unchanged(&f.root, true, &before);
            }
            assert_eq!(f.bytes(), marked);
            assert_eq!(
                f.publication().1,
                proof.base.generation - u64::from(case == "base")
            );
            assert_eq!(
                engine
                    .load_manifest_structure(&legacy.prepared.change_id)
                    .unwrap()
                    .1,
                legacy.prepared.manifest_hash
            );
            assert_eq!(
                crate::changes::journal::load_journal(
                    app.fs(),
                    &legacy.manifest,
                    &legacy.prepared.manifest_hash
                )
                .unwrap()
                .frames,
                legacy.journal.frames
            );
            f.assert_source_history();
        }
    }
}

#[test]
fn normalized_page_inverse_preview_preserves_missing_and_torn_journals_and_checks_supporting_guards()
 {
    for retained in [false, true] {
        for journal_case in ["missing", "torn", "supporting-guard"] {
            let f = Fixture::new(retained);
            let (parent, marked) = f.marker_change();
            let inverse = f.rollback(&parent, &marked);
            let journal =
                f.physical(&crate::changes::journal::journal_path(&inverse.change_id).unwrap());
            match journal_case {
                "missing" => fs::remove_file(journal).unwrap(),
                "torn" => {
                    let mut bytes = fs::read(&journal).unwrap();
                    bytes.extend_from_slice(b"LWJ");
                    fs::write(journal, bytes).unwrap();
                }
                "supporting-guard" => {
                    let wiki = f.physical(&rel("WIKI.md"));
                    let mut bytes = fs::read(&wiki).unwrap();
                    bytes.extend_from_slice(b"\nChanged supporting author fact017.\n");
                    fs::write(wiki, bytes).unwrap();
                }
                _ => unreachable!(),
            }
            let before = tree(&f.root, false);
            let result = f
                .with_options(OperationOptions {
                    dry_run: true,
                    ..options()
                })
                .changes_apply(inverse.change_id.clone());
            if journal_case == "supporting-guard" {
                assert_eq!(result.unwrap_err().code, ErrorCode::FreshnessConflict);
            } else {
                let planned = result.unwrap();
                assert_eq!(planned.status, Some(ChangeStatus::Prepared));
                assert!(planned.snapshot.is_none());
            }
            assert_tree_unchanged(&f.root, false, &before);
            assert_eq!(f.bytes(), marked);
            f.assert_source_history();
        }
    }
}

#[test]
fn normalized_page_inverse_terminal_preview_and_retry_preserve_later_author_bytes() {
    for retained in [false, true] {
        let f = Fixture::new(retained);
        let (parent, marked) = f.marker_change();
        let inverse = f.rollback(&parent, &marked);
        let committed = f.app().changes_apply(inverse.change_id.clone()).unwrap();
        let mut later = f.original.clone();
        later.extend_from_slice(UNFAMILIAR.as_bytes());
        fs::write(f.physical(&rel(PAGE)), &later).unwrap();
        for dry_run in [true, false] {
            let before = tree(&f.root, !dry_run);
            let retry = f
                .with_options(OperationOptions {
                    dry_run,
                    ..options()
                })
                .changes_apply(inverse.change_id.clone())
                .unwrap();
            assert!(retry.reused);
            assert_eq!(retry.status, Some(ChangeStatus::Committed));
            assert_eq!(retry.snapshot, committed.snapshot);
            assert_tree_unchanged(&f.root, !dry_run, &before);
        }
        assert_eq!(f.bytes(), later);
        assert_eq!(
            indexed_and_committed(&f.app().changes_show(inverse.change_id).unwrap()),
            (1, 1)
        );
        f.assert_source_history();
    }
}

#[test]
fn legacy_page_admission_refuses_original_guards_started_history_and_existing_receipts_without_preview_writes()
 {
    for retained in [false, true] {
        for case in [
            "original-guard",
            "started",
            "torn-prefix",
            "existing-proof",
            "existing-delta",
            "unfamiliar-author",
        ] {
            let f = Fixture::new(retained);
            let (parent, marked) = f.marker_change();
            let app = f.app();
            let engine = ChangeEngine::new(app.fs().clone()).unwrap();
            let legacy = if case == "original-guard" {
                let mut draft = engine.inverse_plan(&parent).unwrap().draft;
                let wiki = rel("WIKI.md");
                draft
                    .read_preconditions
                    .push(crate::changes::ReadDependency {
                        path: wiki.clone(),
                        expected: crate::vault::ExpectedState::Hash(Blake3Hash::digest(
                            fs::read(f.physical(&wiki)).unwrap(),
                        )),
                    });
                // An original read guard is not legal public inverse ancestry.
                // Assert that invariant first, then retain an explicitly
                // adversarial internal plan to reach the separate admission
                // refusal. This is not an old-producer compatibility fixture.
                let before_validation = tree(&f.root, false);
                let graph_error = engine
                    .validate_draft_graph(&draft, &CatalogGraphValidator)
                    .unwrap_err();
                assert_eq!(graph_error.code, ErrorCode::RecordInvalid);
                assert_eq!(
                    graph_error.message,
                    "inverse contains non-reversal authority"
                );
                assert_tree_unchanged(&f.root, false, &before_validation);
                let writer = WriterPermit::acquire(app.fs().root(), Duration::ZERO).unwrap();
                engine.prepare(&writer, draft).unwrap()
            } else {
                old_public_path_inverse(&f, &parent)
            };
            match case {
                "started" => {
                    let writer = WriterPermit::acquire(app.fs().root(), Duration::ZERO).unwrap();
                    crate::changes::journal::append_event(
                        app.fs(),
                        &writer,
                        &legacy.manifest,
                        &legacy.prepared.manifest_hash,
                        ChangeEvent::Applying,
                    )
                    .unwrap();
                }
                "torn-prefix" => {
                    let path = f.physical(
                        &crate::changes::journal::journal_path(&legacy.prepared.change_id).unwrap(),
                    );
                    let mut bytes = fs::read(&path).unwrap();
                    bytes.extend_from_slice(b"LWJ");
                    fs::write(path, bytes).unwrap();
                }
                "existing-proof" | "existing-delta" => {
                    let path = if case == "existing-proof" {
                        crate::changes::indexed_refresh::baseline_path(&legacy.prepared).unwrap()
                    } else {
                        crate::catalog::source_refresh::delta_path(&legacy.prepared).unwrap()
                    };
                    fs::write(f.physical(&path), b"{}").unwrap();
                }
                "unfamiliar-author" => {
                    let mut bytes = marked.clone();
                    bytes.extend_from_slice(UNFAMILIAR.as_bytes());
                    fs::write(f.physical(&rel(PAGE)), bytes).unwrap();
                }
                _ => {}
            }
            let before = tree(&f.root, false);
            let error = f
                .with_options(OperationOptions {
                    dry_run: true,
                    ..options()
                })
                .changes_apply(legacy.prepared.change_id.clone())
                .unwrap_err();
            if case == "unfamiliar-author" {
                assert_eq!(error.code, ErrorCode::ContentConflict);
            } else if case == "original-guard" {
                assert_eq!(error.code, ErrorCode::RecoveryRequired);
                assert_eq!(
                    error.message,
                    "legacy Page admission requires exact identity and no original read guards"
                );
            }
            assert_tree_unchanged(&f.root, false, &before);
            let before = tree(&f.root, true);
            let error = app
                .changes_apply(legacy.prepared.change_id.clone())
                .unwrap_err();
            if case == "original-guard" {
                assert_eq!(error.code, ErrorCode::RecoveryRequired);
                assert_eq!(
                    error.message,
                    "legacy Page admission requires exact identity and no original read guards"
                );
            }
            assert_tree_unchanged(&f.root, true, &before);
            assert_eq!(
                engine
                    .load_manifest_structure(&legacy.prepared.change_id)
                    .unwrap()
                    .1,
                legacy.prepared.manifest_hash
            );
            f.assert_source_history();
        }
    }
}

#[test]
fn legacy_page_admission_retained_guards_reject_author_supporting_and_obsolete_base_changes() {
    for retained in [false, true] {
        for case in ["author", "supporting", "base"] {
            let f = Fixture::new(retained);
            let (parent, marked) = f.marker_change();
            let legacy = old_public_path_inverse(&f, &parent);
            retain_legacy_admission(&f, &legacy.prepared);
            match case {
                "author" => {
                    let mut bytes = marked.clone();
                    bytes.extend_from_slice(UNFAMILIAR.as_bytes());
                    fs::write(f.physical(&rel(PAGE)), bytes).unwrap();
                }
                "supporting" => {
                    let wiki = f.physical(&rel("WIKI.md"));
                    let mut bytes = fs::read(&wiki).unwrap();
                    bytes.extend_from_slice(b"\nSupporting edit017.\n");
                    fs::write(wiki, bytes).unwrap();
                }
                "base" => {
                    let bytes = b"---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: page_later_publication\ntitle: Later publication\nwiki_status: reviewed\n---\nUnrelated author text.\n".to_vec();
                    f.app()
                        .page_put(rel("pages/later-publication.md"), bytes, None)
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let current = f.bytes();
            let publication = f.publication();
            let before = tree(&f.root, false);
            let error = f
                .with_options(OperationOptions {
                    dry_run: true,
                    ..options()
                })
                .changes_apply(legacy.prepared.change_id.clone())
                .unwrap_err();
            assert_eq!(
                error.code,
                if case == "base" {
                    ErrorCode::RecoveryRequired
                } else {
                    ErrorCode::FreshnessConflict
                }
            );
            assert_tree_unchanged(&f.root, false, &before);
            assert!(
                f.app()
                    .changes_apply(legacy.prepared.change_id.clone())
                    .is_err()
            );
            assert_eq!(f.bytes(), current);
            assert_eq!(f.publication(), publication);
            let details = f
                .app()
                .changes_show(legacy.prepared.change_id.clone())
                .unwrap();
            assert_eq!(details.manifest, legacy.manifest);
            assert_eq!(details.frames, legacy.journal.frames);
            if case == "base" {
                let aborted = f
                    .app()
                    .changes_abort(legacy.prepared.change_id.clone())
                    .unwrap();
                assert_eq!(aborted.status, Some(ChangeStatus::Aborted));
                assert_eq!(f.bytes(), current);
                assert_eq!(f.publication(), publication);
            }
            f.assert_source_history();
        }
    }
}

// Narrow returned-error adapter follows existing NativeIo fault helpers. Each
// cut records that its real production boundary was reached; no call-count guess.
struct LegacyAdmissionIo {
    target: PathBuf,
    after: bool,
    reached: std::sync::atomic::AtomicBool,
    rendezvous: Option<std::sync::Arc<LegacyAdmissionRendezvous>>,
}

struct LegacyAdmissionRendezvous {
    path: PathBuf,
    bytes: Vec<u8>,
}
impl LegacyAdmissionRendezvous {
    fn pause(&self) -> ! {
        // The parent waits for the complete exact marker, not mere existence.
        // This callback owns the real reached cut and never rewrites the marker.
        fs::write(&self.path, &self.bytes).unwrap();
        loop {
            std::thread::park_timeout(Duration::from_secs(1));
        }
    }
}

#[cfg(unix)]
fn legacy_admission_marker(cut: &str, retained: bool, change: &PreparedChange) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "cut":cut,"retained":retained,"change":change,"reached":true
    }))
    .unwrap()
}
impl crate::vault::DurableIo for LegacyAdmissionIo {
    fn create_stage(&self, p: &Path) -> std::io::Result<fs::File> {
        crate::vault::NativeIo.create_stage(p)
    }
    fn create_private_stage(&self, p: &Path) -> std::io::Result<fs::File> {
        crate::vault::NativeIo.create_private_stage(p)
    }
    fn create_private_directory(&self, p: &Path) -> std::io::Result<()> {
        crate::vault::NativeIo.create_private_directory(p)
    }
    fn open_append(&self, p: &Path) -> std::io::Result<fs::File> {
        crate::vault::NativeIo.open_append(p)
    }
    fn truncate_file(&self, f: &fs::File, n: u64) -> std::io::Result<()> {
        crate::vault::NativeIo.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut fs::File, b: &[u8]) -> std::io::Result<()> {
        crate::vault::NativeIo.write_stage(f, b)
    }
    fn sync_file(&self, f: &fs::File) -> std::io::Result<()> {
        crate::vault::NativeIo.sync_file(f)
    }
    fn replace(&self, staged: &Path, target: &Path) -> std::io::Result<()> {
        if target == self.target && !self.reached.swap(true, std::sync::atomic::Ordering::SeqCst) {
            if self.after {
                crate::vault::NativeIo.replace(staged, target)?;
            }
            if let Some(rendezvous) = &self.rendezvous {
                rendezvous.pause();
            }
            return Err(std::io::Error::other(
                "reached legacy Page admission boundary017",
            ));
        }
        crate::vault::NativeIo.replace(staged, target)
    }
    fn remove(&self, p: &Path) -> std::io::Result<()> {
        crate::vault::NativeIo.remove(p)
    }
    fn create_directory(&self, p: &Path) -> std::io::Result<()> {
        crate::vault::NativeIo.create_directory(p)
    }
    fn sync_directory(&self, p: &Path) -> std::io::Result<crate::vault::DirectorySync> {
        crate::vault::NativeIo.sync_directory(p)
    }
}
struct LegacyAdmissionPublication {
    reached: std::sync::atomic::AtomicBool,
    rendezvous: Option<std::sync::Arc<LegacyAdmissionRendezvous>>,
}
impl crate::catalog::PublicationFault for LegacyAdmissionPublication {
    fn check(&self, point: crate::catalog::PublicationCheckpoint) -> crate::domain::Result<()> {
        if point == crate::catalog::PublicationCheckpoint::AfterCommit
            && !self.reached.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            if let Some(rendezvous) = &self.rendezvous {
                rendezvous.pause();
            }
            return Err(crate::domain::WikiError::new(
                ErrorCode::RecoveryRequired,
                "reached legacy Page admission SQL publication017",
            ));
        }
        Ok(())
    }
}

#[test]
fn legacy_page_admission_reached_envelope_canonical_and_sql_errors_resume_exact_id_once() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    for retained in [false, true] {
        for cut in [
            "envelope-before",
            "envelope-after",
            "canonical-after",
            "sql-after-commit",
        ] {
            let f = Fixture::new(retained);
            let (parent, marked) = f.marker_change();
            let legacy = old_public_path_inverse(&f, &parent);
            let before_publication = f.publication();
            let root = VaultRoot::explicit(&f.root).unwrap();
            let target = f.physical(&if cut == "canonical-after" {
                rel(PAGE)
            } else {
                crate::changes::indexed_refresh::baseline_path(&legacy.prepared).unwrap()
            });
            let io = Arc::new(LegacyAdmissionIo {
                target,
                after: cut != "envelope-before",
                reached: AtomicBool::new(false),
                rendezvous: None,
            });
            let fs = if cut == "sql-after-commit" {
                VaultFs::new(root)
            } else {
                VaultFs::with_io(root, io.clone())
            };
            let publication_fault = Arc::new(LegacyAdmissionPublication {
                reached: AtomicBool::new(false),
                rendezvous: None,
            });
            let catalog = crate::catalog::Catalog::with_options(
                fs.clone(),
                f.app().vault_id().clone(),
                crate::catalog::CatalogOptions {
                    busy_timeout_ms: 200,
                    fault: (cut == "sql-after-commit").then(|| {
                        publication_fault.clone() as Arc<dyn crate::catalog::PublicationFault>
                    }),
                },
            );
            let engine = ChangeEngine::new(fs.clone()).unwrap();
            let writer = WriterPermit::acquire(fs.root(), Duration::ZERO).unwrap();
            let interrupted =
                match IndexedRefreshSession::adopt_legacy_page(&catalog, &writer, &legacy.prepared)
                {
                    Ok(mut session) => engine.apply_indexed_refresh(&writer, &mut session),
                    Err(error) => Err(error),
                };
            assert!(interrupted.is_err(), "{cut}");
            assert!(
                if cut == "sql-after-commit" {
                    publication_fault.reached.load(Ordering::SeqCst)
                } else {
                    io.reached.load(Ordering::SeqCst)
                },
                "real cut not reached: {cut}"
            );
            drop(writer);
            if cut.starts_with("envelope-") {
                assert_eq!(f.bytes(), marked);
            }
            // Restart through public recovery: a Prepared envelope remains a
            // proposal; applying/published cuts resume through existing authority.
            f.app().recover().unwrap();
            let applied = f
                .app()
                .changes_apply(legacy.prepared.change_id.clone())
                .unwrap();
            assert_eq!(applied.change.as_ref(), Some(&legacy.prepared));
            assert_eq!(applied.status, Some(ChangeStatus::Committed));
            assert_eq!(
                applied.snapshot.as_ref().unwrap().generation,
                before_publication.1 + 1
            );
            assert_eq!(
                f.publication(),
                (before_publication.0, before_publication.1 + 1)
            );
            let details = f
                .app()
                .changes_show(legacy.prepared.change_id.clone())
                .unwrap();
            assert_eq!(details.manifest, legacy.manifest);
            assert_eq!(&details.frames[..1], &legacy.journal.frames);
            assert_eq!(indexed_and_committed(&details), (1, 1));
            let stable = tree(&f.root, true);
            assert!(
                f.app()
                    .changes_apply(legacy.prepared.change_id)
                    .unwrap()
                    .reused
            );
            assert_tree_unchanged(&f.root, true, &stable);
            f.assert_page(&f.original);
            f.assert_source_history();
        }
    }
}

#[cfg(unix)]
#[test]
#[ignore = "owned by enabled legacy Page admission SIGKILL wrapper"]
fn legacy_page_admission_native_crash_child() {
    use std::sync::{Arc, atomic::AtomicBool};
    let root =
        VaultRoot::explicit(std::env::var_os("LWIKI_LEGACY_PAGE_CRASH_ROOT").unwrap()).unwrap();
    let retained = std::env::var("LWIKI_LEGACY_PAGE_CRASH_LAYOUT").unwrap() == "retained";
    assert_eq!(crate::storage::layout::active(&root).unwrap(), retained);
    let cut = std::env::var("LWIKI_LEGACY_PAGE_CRASH_CUT").unwrap();
    assert!(matches!(
        cut.as_str(),
        "envelope-before" | "envelope-after" | "canonical-after" | "sql-after-commit"
    ));
    let change: PreparedChange =
        serde_json::from_str(&std::env::var("LWIKI_LEGACY_PAGE_CRASH_CHANGE").unwrap()).unwrap();
    let rendezvous = Arc::new(LegacyAdmissionRendezvous {
        path: PathBuf::from(std::env::var_os("LWIKI_LEGACY_PAGE_CRASH_MARKER").unwrap()),
        bytes: legacy_admission_marker(&cut, retained, &change),
    });
    let plain = VaultFs::new(root.clone());
    let target_path = if cut == "canonical-after" {
        rel(PAGE)
    } else {
        crate::changes::indexed_refresh::baseline_path(&change).unwrap()
    };
    let target = plain.root().resolve(&target_path).unwrap();
    let fs = if cut == "sql-after-commit" {
        plain
    } else {
        VaultFs::with_io(
            root,
            Arc::new(LegacyAdmissionIo {
                target,
                after: cut != "envelope-before",
                reached: AtomicBool::new(false),
                rendezvous: Some(rendezvous.clone()),
            }),
        )
    };
    let engine = ChangeEngine::new(fs.clone()).unwrap();
    let catalog = crate::catalog::Catalog::with_options(
        fs.clone(),
        engine.vault_id().clone(),
        crate::catalog::CatalogOptions {
            busy_timeout_ms: 1000,
            fault: (cut == "sql-after-commit").then(|| {
                Arc::new(LegacyAdmissionPublication {
                    reached: AtomicBool::new(false),
                    rendezvous: Some(rendezvous),
                }) as Arc<dyn crate::catalog::PublicationFault>
            }),
        },
    );
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    crate::maintenance_parallel::command_scope(|| {
        let mut session = IndexedRefreshSession::adopt_legacy_page(&catalog, &writer, &change)?;
        engine.apply_indexed_refresh(&writer, &mut session)?;
        Ok(())
    })
    .unwrap();
    panic!("legacy Page native child did not reach {cut}");
}

#[cfg(unix)]
#[test]
fn legacy_page_admission_native_sigkill_reaps_and_recovers_same_id_at_four_cuts_both_layouts() {
    use std::{
        os::unix::process::ExitStatusExt,
        process::{Command, Stdio},
        time::Instant,
    };
    // Reap even if any rendezvous assertion or cut oracle panics. The parent
    // retains its original fixture and every recovery uses that same vault/ID.
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
    for retained in [false, true] {
        for cut in [
            "envelope-before",
            "envelope-after",
            "canonical-after",
            "sql-after-commit",
        ] {
            let f = Fixture::new(retained);
            let (parent, marked) = f.marker_change();
            let parent_before = f.app().changes_show(parent.change_id.clone()).unwrap();
            let legacy = old_public_path_inverse(&f, &parent);
            let details_before = f
                .app()
                .changes_show(legacy.prepared.change_id.clone())
                .unwrap();
            let publication = f.publication();
            let prepared_prefix =
                crate::changes::journal::encode_frame(&legacy.journal.frames[0]).unwrap();
            let marker = f._temp.path().join("legacy-page-cut-ready019.json");
            let expected_marker = legacy_admission_marker(cut, retained, &legacy.prepared);
            let child_name = format!(
                "{}::legacy_page_admission_native_crash_child",
                module_path!().split_once("::").unwrap().1
            );
            let mut child = Child(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", &child_name, "--ignored", "--nocapture"])
                    .env("LWIKI_LEGACY_PAGE_CRASH_ROOT", &f.root)
                    .env(
                        "LWIKI_LEGACY_PAGE_CRASH_LAYOUT",
                        if retained { "retained" } else { "original" },
                    )
                    .env("LWIKI_LEGACY_PAGE_CRASH_CUT", cut)
                    .env(
                        "LWIKI_LEGACY_PAGE_CRASH_CHANGE",
                        serde_json::to_string(&legacy.prepared).unwrap(),
                    )
                    .env("LWIKI_LEGACY_PAGE_CRASH_MARKER", &marker)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if fs::read(&marker).is_ok_and(|bytes| bytes == expected_marker) {
                    break;
                }
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "child exited before reached {cut}, retained={retained}"
                );
                assert!(
                    Instant::now() < deadline,
                    "exact reached marker timed out: {cut}, retained={retained}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(fs::read(&marker).unwrap(), expected_marker);
            child.0.kill().unwrap();
            assert_eq!(
                child.0.wait().unwrap().signal(),
                Some(9),
                "actual native SIGKILL at {cut}"
            );
            let app = f.app();
            let engine = ChangeEngine::new(app.fs().clone()).unwrap();
            let (manifest, hash) = engine.load_manifest(&legacy.prepared.change_id).unwrap();
            assert_eq!(manifest, legacy.manifest);
            assert_eq!(hash, legacy.prepared.manifest_hash);
            let journal_path =
                crate::changes::journal::journal_path(&legacy.prepared.change_id).unwrap();
            let journal_bytes = fs::read(f.physical(&journal_path)).unwrap();
            assert!(journal_bytes.starts_with(&prepared_prefix));
            let state =
                crate::changes::journal::decode_journal(&journal_bytes, &manifest, &hash).unwrap();
            assert!(!state.torn_tail);
            let proof = engine.load_indexed_refresh_proof(&legacy.prepared).unwrap();
            let envelope_path =
                crate::changes::indexed_refresh::baseline_path(&legacy.prepared).unwrap();
            let retained_envelope = proof
                .as_ref()
                .map(|_| fs::read(f.physical(&envelope_path)).unwrap());
            let authority = app.catalog().operation_state().unwrap().unwrap();
            if cut.starts_with("envelope-") {
                assert_eq!(state.frames, legacy.journal.frames);
                assert_eq!(state.status, Some(ChangeStatus::Prepared));
                assert!(authority.active().is_none());
                assert_eq!(f.bytes(), marked);
                assert_eq!(proof.is_some(), cut == "envelope-after");
            } else {
                assert_eq!(f.bytes(), f.original);
                assert_eq!(&authority.active().unwrap().change, &legacy.prepared);
                assert_eq!(
                    state.status,
                    Some(if cut == "canonical-after" {
                        ChangeStatus::Applying
                    } else {
                        ChangeStatus::FilesApplied
                    })
                );
            }
            if let Some(proof) = &proof {
                assert_eq!(proof.version, 4);
                assert_eq!(proof.change, legacy.prepared);
            }
            assert_eq!(f.publication(), publication);
            // Acquiring/releasing the native writer after reaping demonstrates
            // the crashed process no longer owns its lock before restart.
            drop(WriterPermit::acquire(app.fs().root(), Duration::from_secs(1)).unwrap());
            let recovery = app.recover().unwrap();
            if cut.starts_with("envelope-") {
                assert!(recovery.report.unwrap().staged.contains(&legacy.prepared));
                assert_eq!(f.bytes(), marked);
                assert_eq!(f.publication(), publication);
            }
            let applied = app
                .changes_apply(legacy.prepared.change_id.clone())
                .unwrap();
            assert_eq!(applied.change.as_ref(), Some(&legacy.prepared));
            assert_eq!(applied.status, Some(ChangeStatus::Committed));
            let intended = applied.snapshot.unwrap();
            assert_eq!(intended.generation, publication.1 + 1);
            if let Some(proof) = proof {
                assert_eq!(intended, proof.intended);
            }
            assert_eq!(f.publication(), (publication.0, publication.1 + 1));
            let after = app.changes_show(legacy.prepared.change_id.clone()).unwrap();
            assert_eq!(after.prepared, legacy.prepared);
            assert_eq!(after.manifest, legacy.manifest);
            assert_eq!(&after.frames[..1], &legacy.journal.frames);
            assert_eq!(after.payloads[0].before, details_before.payloads[0].before);
            assert_eq!(
                after.payloads[0].proposed,
                details_before.payloads[0].proposed
            );
            assert_eq!(indexed_and_committed(&after), (1, 1));
            if let Some(bytes) = retained_envelope {
                assert_eq!(
                    fs::read(f.physical(&envelope_path)).unwrap(),
                    bytes,
                    "recovery must not reproject or replace retained v4 at {cut}"
                );
            }
            let parent_after = app.changes_show(parent.change_id.clone()).unwrap();
            assert_eq!(parent_after.manifest, parent_before.manifest);
            assert_eq!(parent_after.frames, parent_before.frames);
            let stable = tree(&f.root, true);
            let retry = app
                .changes_apply(legacy.prepared.change_id.clone())
                .unwrap();
            assert!(retry.reused);
            assert_eq!(retry.snapshot, Some(intended));
            assert_tree_unchanged(&f.root, true, &stable);
            assert_eq!(
                indexed_and_committed(
                    &app.changes_show(legacy.prepared.change_id.clone()).unwrap()
                ),
                (1, 1)
            );
            f.assert_page(&f.original);
            f.assert_source_history();
            eprintln!(
                "legacy Page SIGKILL cut={cut} retained={retained} change={} reached=true signal=9 reaped=true recovered=true",
                legacy.prepared.change_id
            );
        }
    }
}

#[test]
fn normalized_page_replacement_rollback_apply_preserves_author_history_and_one_publication() {
    for retained in [false, true] {
        let f = Fixture::new(retained);
        let (parent, marked) = f.marker_change();
        let parent_before = f.app().changes_show(parent.change_id.clone()).unwrap();
        let inverse = f.rollback(&parent, &marked);
        let staged_before = f.app().changes_show(inverse.change_id.clone()).unwrap();
        let before = f.publication();
        let applied = f.app().changes_apply(inverse.change_id.clone()).unwrap();
        assert_eq!(applied.change.as_ref(), Some(&inverse));
        assert_eq!(applied.status, Some(ChangeStatus::Committed));
        let snapshot = applied.snapshot.unwrap();
        assert_eq!(snapshot.generation, before.1 + 1);
        assert_eq!(f.publication(), (before.0, before.1 + 1));
        f.assert_page(&f.original);
        let terminal = f.app().changes_show(inverse.change_id.clone()).unwrap();
        assert_eq!(terminal.prepared, inverse);
        assert_eq!(terminal.manifest, staged_before.manifest);
        assert_eq!(indexed_and_committed(&terminal), (1, 1));
        let indexed: Vec<_> = terminal
            .frames
            .iter()
            .filter_map(|frame| match &frame.event {
                ChangeEvent::Indexed { snapshot } => Some(snapshot.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(indexed, vec![snapshot.clone()]);
        assert_eq!(
            terminal.payloads[0].before.as_deref(),
            Some(marked.as_slice())
        );
        assert_eq!(
            terminal.payloads[0].proposed.as_deref(),
            Some(f.original.as_slice())
        );
        let parent_after = f.app().changes_show(parent.change_id.clone()).unwrap();
        assert_eq!(parent_after.manifest, parent_before.manifest);
        assert_eq!(parent_after.frames, parent_before.frames);
        assert_eq!(
            parent_after.payloads[0].before,
            parent_before.payloads[0].before
        );
        assert_eq!(
            parent_after.payloads[0].proposed,
            parent_before.payloads[0].proposed
        );
        let stable = tree(&f.root, true);
        let retry = f.app().changes_apply(inverse.change_id.clone()).unwrap();
        assert!(retry.reused);
        assert_eq!(retry.change.as_ref(), Some(&inverse));
        assert_eq!(retry.status, Some(ChangeStatus::Committed));
        assert_eq!(retry.snapshot, Some(snapshot));
        assert_tree_unchanged(&f.root, true, &stable);
        assert_eq!(
            indexed_and_committed(&f.app().changes_show(inverse.change_id).unwrap()),
            (1, 1)
        );
        f.assert_page(&f.original);
    }
}

#[test]
fn normalized_page_inverse_refuses_unfamiliar_author_before_rollback_without_allocation() {
    for retained in [false, true] {
        let f = Fixture::new(retained);
        let (parent, mut marked) = f.marker_change();
        marked.extend_from_slice(UNFAMILIAR.as_bytes());
        fs::write(f.physical(&rel(PAGE)), &marked).unwrap();
        let before = tree(&f.root, false);
        let error = f.app().changes_rollback(parent.change_id).unwrap_err();
        assert_eq!(error.code, ErrorCode::ContentConflict);
        assert_tree_unchanged(&f.root, false, &before);
        assert_eq!(f.bytes(), marked);
        f.assert_source_history();
    }
}

#[test]
fn normalized_page_inverse_refuses_unfamiliar_author_between_staging_and_apply() {
    for retained in [false, true] {
        let f = Fixture::new(retained);
        let (parent, marked) = f.marker_change();
        let inverse = f.rollback(&parent, &marked);
        let prepared = f.app().changes_show(inverse.change_id.clone()).unwrap();
        let publication = f.publication();
        let mut unfamiliar = marked;
        unfamiliar.extend_from_slice(UNFAMILIAR.as_bytes());
        fs::write(f.physical(&rel(PAGE)), &unfamiliar).unwrap();
        let error = f
            .app()
            .changes_apply(inverse.change_id.clone())
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::FreshnessConflict);
        assert_eq!(f.bytes(), unfamiliar);
        assert_eq!(f.publication(), publication);
        let refused = f.app().changes_show(inverse.change_id).unwrap();
        assert_eq!(refused.manifest, prepared.manifest);
        assert_eq!(indexed_and_committed(&refused), (0, 0));
        assert_eq!(refused.payloads[0].before, prepared.payloads[0].before);
        assert_eq!(refused.payloads[0].proposed, prepared.payloads[0].proposed);
        f.assert_source_history();
    }
}

#[test]
fn normalized_page_inverse_dry_run_rollback_and_apply_preserve_complete_tree() {
    for retained in [false, true] {
        let f = Fixture::new(retained);
        let (parent, marked) = f.marker_change();
        let preview = f.with_options(OperationOptions {
            dry_run: true,
            ..options()
        });
        let before = tree(&f.root, false);
        let planned = preview.changes_rollback(parent.change_id.clone()).unwrap();
        assert!(planned.change.is_none());
        assert!(planned.snapshot.is_none());
        assert_tree_unchanged(&f.root, false, &before);
        let inverse = f.rollback(&parent, &marked);
        let before = tree(&f.root, false);
        let planned = preview.changes_apply(inverse.change_id.clone()).unwrap();
        assert_eq!(planned.change.as_ref(), Some(&inverse));
        assert_eq!(planned.status, Some(ChangeStatus::Prepared));
        assert!(planned.snapshot.is_none());
        assert_tree_unchanged(&f.root, false, &before);
        f.assert_page(&marked);
    }
}

#[test]
fn normalized_inverse_of_inverse_restores_exact_author_marker_without_rewriting_history() {
    for retained in [false, true] {
        let f = Fixture::new(retained);
        let (parent, marked) = f.marker_change();
        let inverse = f.rollback(&parent, &marked);
        f.app().changes_apply(inverse.change_id.clone()).unwrap();
        f.assert_page(&f.original);
        let first_history = f.app().changes_show(inverse.change_id.clone()).unwrap();
        let before = f.publication();
        let second = f.app().changes_rollback(inverse.change_id.clone()).unwrap();
        assert_eq!(second.status, Some(ChangeStatus::Prepared));
        assert_eq!(f.publication(), before);
        let second = second.change.unwrap();
        let staged = f.app().changes_show(second.change_id.clone()).unwrap();
        assert_eq!(
            staged.manifest.inverse_of.as_ref(),
            Some(&inverse.change_id)
        );
        assert_eq!(
            staged.payloads[0].before.as_deref(),
            Some(f.original.as_slice())
        );
        assert_eq!(
            staged.payloads[0].proposed.as_deref(),
            Some(marked.as_slice())
        );
        let applied = f.app().changes_apply(second.change_id.clone()).unwrap();
        assert_eq!(applied.status, Some(ChangeStatus::Committed));
        assert_eq!(applied.snapshot.unwrap().generation, before.1 + 1);
        f.assert_page(&marked);
        assert_eq!(
            indexed_and_committed(&f.app().changes_show(second.change_id).unwrap()),
            (1, 1)
        );
        let first_after = f.app().changes_show(inverse.change_id).unwrap();
        assert_eq!(first_after.manifest, first_history.manifest);
        assert_eq!(first_after.frames, first_history.frames);
        f.assert_source_history();
    }
}

#[test]
fn old_public_path_legacy_prepared_page_inverse_applies_same_id_once() {
    for retained in [false, true] {
        let f = Fixture::new(retained);
        let (parent, marked) = f.marker_change();
        let app = f.app();
        let legacy = old_public_path_inverse(&f, &parent);
        assert_eq!(legacy.status, ChangeStatus::Prepared);
        assert_eq!(legacy.manifest.inverse_of.as_ref(), Some(&parent.change_id));
        assert!(legacy.manifest.read_preconditions.is_empty());
        assert_eq!(legacy.manifest.operations.len(), 1);
        assert_eq!(legacy.manifest.operations[0].target, rel(PAGE));
        assert_eq!(legacy.journal.frames.len(), 1);
        assert_eq!(legacy.journal.frames[0].event, ChangeEvent::Prepared);
        let details = app.changes_show(legacy.prepared.change_id.clone()).unwrap();
        assert_eq!(
            details.payloads[0].before.as_deref(),
            Some(marked.as_slice())
        );
        assert_eq!(
            details.payloads[0].proposed.as_deref(),
            Some(f.original.as_slice())
        );
        let publication = f.publication();
        let before = tree(&f.root, false);
        let preview = f
            .with_options(OperationOptions {
                dry_run: true,
                ..options()
            })
            .changes_apply(legacy.prepared.change_id.clone())
            .unwrap();
        assert_eq!(preview.change.as_ref(), Some(&legacy.prepared));
        assert_eq!(preview.status, Some(ChangeStatus::Prepared));
        assert!(preview.snapshot.is_none());
        assert_tree_unchanged(&f.root, false, &before);
        assert_eq!(f.publication(), publication);
        let applied = app
            .changes_apply(legacy.prepared.change_id.clone())
            .unwrap();
        assert_eq!(applied.change.as_ref(), Some(&legacy.prepared));
        assert_eq!(applied.status, Some(ChangeStatus::Committed));
        let snapshot = applied.snapshot.unwrap();
        assert_eq!(snapshot.generation, publication.1 + 1);
        let after = app.changes_show(legacy.prepared.change_id.clone()).unwrap();
        assert_eq!(after.prepared, legacy.prepared);
        assert_eq!(after.manifest, legacy.manifest);
        assert_eq!(&after.frames[..1], &legacy.journal.frames);
        assert_eq!(indexed_and_committed(&after), (1, 1));
        assert_eq!(after.payloads[0].before, details.payloads[0].before);
        assert_eq!(after.payloads[0].proposed, details.payloads[0].proposed);
        f.assert_page(&f.original);
        // Historical terminal retry must ignore a later unfamiliar author edit.
        let mut later = f.original.clone();
        later.extend_from_slice(UNFAMILIAR.as_bytes());
        fs::write(f.physical(&rel(PAGE)), &later).unwrap();
        let stable = tree(&f.root, true);
        let retry = app
            .changes_apply(legacy.prepared.change_id.clone())
            .unwrap();
        assert!(retry.reused);
        assert_eq!(retry.snapshot, Some(snapshot));
        assert_eq!(retry.change.as_ref(), Some(&legacy.prepared));
        assert_tree_unchanged(&f.root, true, &stable);
        assert_eq!(
            indexed_and_committed(&app.changes_show(legacy.prepared.change_id).unwrap()),
            (1, 1)
        );
        assert_eq!(f.bytes(), later);
        f.assert_source_history();
    }
}

#[test]
fn normalized_page_creation_inverse_refuses_before_retained_allocation() {
    for retained in [false, true] {
        let f = Fixture::new(retained);
        let target = rel("pages/new-creation.md");
        let bytes = String::from_utf8(page())
            .unwrap()
            .replace("page_rollback_guide", "page_new_for_inverse")
            .into_bytes();
        let created = f
            .app()
            .page_put(target.clone(), bytes.clone(), None)
            .unwrap();
        assert_eq!(created.status, Some(ChangeStatus::Committed));
        let before = tree(&f.root, false);
        let error = f
            .app()
            .changes_rollback(created.change.unwrap().change_id)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::RecordInvalid);
        assert_eq!(
            error.message,
            "Page inverse excludes assets, creation, deletion, rename and ordered operations"
        );
        assert_tree_unchanged(&f.root, false, &before);
        assert_eq!(fs::read(f.physical(&target)).unwrap(), bytes);
        f.assert_page(&f.original);
    }
}

#[test]
fn legacy_page_inverse_projection_probe_exposes_missing_manifest_guards_without_applying() {
    for retained in [false, true] {
        let f = Fixture::new(retained);
        let (parent, marked) = f.marker_change();
        let legacy = old_public_path_inverse(&f, &parent);
        assert!(legacy.manifest.read_preconditions.is_empty());
        let app = f.app();
        let engine = ChangeEngine::new(app.fs().clone()).unwrap();
        let mut legacy_paths = vec![
            crate::changes::prepare::manifest_path(&legacy.prepared.change_id).unwrap(),
            crate::changes::journal::journal_path(&legacy.prepared.change_id).unwrap(),
        ];
        for operation in &legacy.manifest.operations {
            for payload in [&operation.before_payload, &operation.after_payload]
                .into_iter()
                .flatten()
            {
                legacy_paths.push(payload.path.clone());
            }
        }
        let before_legacy: BTreeMap<_, _> = legacy_paths
            .iter()
            .map(|path| {
                let physical = f.physical(path);
                (
                    path.clone(),
                    (
                        fs::read(&physical).unwrap(),
                        fs::metadata(physical).unwrap().modified().unwrap(),
                    ),
                )
            })
            .collect();
        let publication = f.publication();
        let catalog = app.catalog();
        let sealed = engine.page_inverse_plan(&parent).unwrap();
        let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
        let projected = project_page_inverse(
            app.fs(),
            &reader,
            sealed,
            &RefreshProjectionLimits::default(),
        )
        .unwrap()
        .unwrap();
        let projected_draft = projected.draft();
        assert_eq!(projected_draft.inverse_of, legacy.manifest.inverse_of);
        assert_eq!(projected_draft.origin, legacy.manifest.origin);
        assert_eq!(projected_draft.allocated_ids, legacy.manifest.allocated_ids);
        assert_eq!(
            projected_draft.operations.len(),
            legacy.manifest.operations.len()
        );
        for (write, operation) in projected_draft
            .operations
            .iter()
            .zip(&legacy.manifest.operations)
        {
            assert_eq!(write.target, operation.target);
            assert_eq!(write.expected, operation.before);
            let after = write
                .proposed
                .as_ref()
                .map_or(crate::vault::ExpectedState::Absent, |bytes| {
                    crate::vault::ExpectedState::Hash(Blake3Hash::digest(bytes))
                });
            assert_eq!(after, operation.after);
            let details = app.changes_show(legacy.prepared.change_id.clone()).unwrap();
            let payload = details
                .payloads
                .iter()
                .find(|payload| payload.target == write.target)
                .unwrap();
            assert_eq!(write.proposed, payload.proposed);
            let expected_order: Vec<_> = operation
                .apply_after
                .iter()
                .map(|index| legacy.manifest.operations[*index].target.clone())
                .collect();
            assert_eq!(write.apply_after, expected_order);
        }
        let extra_guards = projected_draft.read_preconditions.clone();
        assert!(!extra_guards.is_empty());
        let guards_encoded_bytes = serde_json::to_vec(&extra_guards).unwrap().len();
        drop(reader);
        let writer = WriterPermit::acquire(app.fs().root(), Duration::ZERO).unwrap();
        let session = IndexedRefreshSession::prepare_write(&catalog, &writer, projected).unwrap();
        let fresh_proof = session.proof().clone();
        assert_ne!(fresh_proof.change, legacy.prepared);
        let (fresh_manifest, _) = engine.load_manifest(&fresh_proof.change.change_id).unwrap();
        fresh_proof.validate_manifest(&fresh_manifest).unwrap();
        let proof_payload_encoded_bytes = serde_json::to_vec(&fresh_proof).unwrap().len();
        let proof_path =
            crate::changes::indexed_refresh::baseline_path(&fresh_proof.change).unwrap();
        let proof_receipt_encoded_bytes = fs::metadata(f.physical(&proof_path)).unwrap().len();
        let delta_path = crate::catalog::source_refresh::delta_path(&fresh_proof.change).unwrap();
        let delta_encoded_bytes = fs::metadata(f.physical(&delta_path)).unwrap().len();
        let mut probe = fresh_proof.clone();
        // Memory-only representational probe. It is never retained or applied;
        // changing the ID/hash does not grant the missing immutable read guards.
        probe.change = legacy.prepared.clone();
        let error = probe.validate_manifest(&legacy.manifest).unwrap_err();
        assert_eq!(error.code, ErrorCode::RecoveryRequired);
        assert_eq!(
            error.message,
            "selected refresh boundary is absent from retained read preconditions"
        );
        drop(session);
        drop(writer);
        for (path, (bytes, modified)) in &before_legacy {
            let physical = f.physical(path);
            assert_eq!(fs::read(&physical).unwrap(), *bytes, "{path}");
            assert_eq!(
                fs::metadata(physical).unwrap().modified().unwrap(),
                *modified
            );
        }
        let after = app.changes_show(legacy.prepared.change_id.clone()).unwrap();
        assert_eq!(after.prepared, legacy.prepared);
        assert_eq!(after.manifest, legacy.manifest);
        assert_eq!(after.frames, legacy.journal.frames);
        assert_eq!(after.status, ChangeStatus::Prepared);
        assert_eq!(f.publication(), publication);
        f.assert_page(&marked);
        println!(
            "NORMALIZED-CHANGE016-PROBE {}",
            serde_json::json!({
                "retained_layout": retained,
                "legacy_change": legacy.prepared,
                "fresh_change": fresh_proof.change,
                "extra_read_guards": extra_guards,
                "guards_encoded_bytes": guards_encoded_bytes,
                "proof_payload_encoded_bytes": proof_payload_encoded_bytes,
                "proof_receipt_encoded_bytes": proof_receipt_encoded_bytes,
                "delta_encoded_bytes": delta_encoded_bytes,
                "writes_equal": true,
                "legacy_guard_validation_refused": true,
                "legacy_bytes_and_journal_preserved": true,
                "applied": false,
                "compatibility_pass": false
            })
        );
    }
}

#[test]
fn normalized_page_inverse_rejects_prepared_and_aborted_parent_without_allocation() {
    for retained in [false, true] {
        let f = Fixture::new(retained);
        let mut marked = f.original.clone();
        marked.extend_from_slice(MARKER.as_bytes());
        let staged = f
            .with_options(OperationOptions {
                stage_only: true,
                ..options()
            })
            .page_put(rel(PAGE), marked, Some(Blake3Hash::digest(&f.original)))
            .unwrap();
        assert_eq!(staged.status, Some(ChangeStatus::Prepared));
        let staged = staged.change.unwrap();
        for status in [ChangeStatus::Prepared, ChangeStatus::Aborted] {
            if status == ChangeStatus::Aborted {
                let aborted = f.app().changes_abort(staged.change_id.clone()).unwrap();
                assert_eq!(aborted.status, Some(ChangeStatus::Aborted));
            }
            assert_eq!(
                f.app()
                    .changes_show(staged.change_id.clone())
                    .unwrap()
                    .status,
                status
            );
            let before = tree(&f.root, false);
            let error = f
                .app()
                .changes_rollback(staged.change_id.clone())
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::RecoveryRequired);
            assert_eq!(
                error.message,
                "Page inverse requires authenticated committed history"
            );
            assert_tree_unchanged(&f.root, false, &before);
            f.assert_page(&f.original);
        }
    }
}

#[test]
fn adversarial_same_identity_pages_with_nonreversal_inverse_ancestry_refuse_without_allocation() {
    for retained in [false, true] {
        let f = Fixture::new(retained);
        let (parent, marked) = f.marker_change();
        let app = f.app();
        let parent_history = app.changes_show(parent.change_id.clone()).unwrap();
        let snapshot = parent_history
            .frames
            .iter()
            .find_map(|frame| match &frame.event {
                ChangeEvent::Indexed { snapshot } => Some(snapshot.clone()),
                _ => None,
            })
            .unwrap();
        let engine = ChangeEngine::new(app.fs().clone()).unwrap();
        let writer = WriterPermit::acquire(app.fs().root(), Duration::ZERO).unwrap();
        let mut wrong_reversal = marked.clone();
        wrong_reversal.extend_from_slice(b"Adversarial016: valid Page, wrong reversal.\n");
        assert!(wrong_reversal.len() < 4096);
        let corrupt = engine
            .prepare(
                &writer,
                ChangeDraft {
                    title: "Explicit adversarial non-reversal ancestry".into(),
                    origin: None,
                    inverse_of: Some(parent.change_id.clone()),
                    allocated_ids: BTreeMap::new(),
                    read_preconditions: vec![],
                    operations: vec![ExpectedWrite {
                        target: rel(PAGE),
                        expected: crate::vault::ExpectedState::Hash(Blake3Hash::digest(&marked)),
                        proposed: Some(wrong_reversal.clone()),
                        apply_after: vec![],
                    }],
                },
            )
            .unwrap();
        // Deliberate internal corruption fixture, never producer compatibility
        // evidence: bypass semantic application to install same-ID valid Page
        // bytes and a checksum-valid, transition-valid retained transcript.
        // Public rollback must still prove exact ancestral reversal, rather
        // than trusting valid Page envelopes or a Committed journal alone.
        fs::write(f.physical(&rel(PAGE)), &wrong_reversal).unwrap();
        for event in [
            ChangeEvent::Applying,
            ChangeEvent::Intent { op: 0 },
            ChangeEvent::Done { op: 0 },
            ChangeEvent::FilesApplied,
            ChangeEvent::Indexed { snapshot },
            ChangeEvent::Committed,
        ] {
            crate::changes::journal::append_event(
                app.fs(),
                &writer,
                &corrupt.manifest,
                &corrupt.prepared.manifest_hash,
                event,
            )
            .unwrap();
        }
        drop(writer);
        let observed = app
            .changes_show(corrupt.prepared.change_id.clone())
            .unwrap();
        assert_eq!(observed.status, ChangeStatus::Committed);
        assert_eq!(
            observed.manifest.inverse_of.as_ref(),
            Some(&parent.change_id)
        );
        assert_eq!(
            observed.payloads[0].before.as_deref(),
            Some(marked.as_slice())
        );
        assert_eq!(
            observed.payloads[0].proposed.as_deref(),
            Some(wrong_reversal.as_slice())
        );
        let before = tree(&f.root, false);
        let error = app
            .changes_rollback(corrupt.prepared.change_id)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::RecordInvalid);
        assert_eq!(
            error.message,
            "inverse is not the exact guarded mutable reversal"
        );
        assert_tree_unchanged(&f.root, false, &before);
        assert_eq!(f.bytes(), wrong_reversal);
        f.assert_source_history();
    }
}
