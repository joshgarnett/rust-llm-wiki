//! Actual ordinary schema2 batch/restart workflows; returned I/O errors are not
//! power-loss qualification. All inputs are disposable and provider-free.
use super::*;
use crate::{
    catalog::{CatalogGraphValidator, query_types::QueryReadLimits},
    changes::{ChangeEvent, ChangeInspection, ConflictResolutionMode, PreparedChange},
    retrieval::{QueryPlan, SearchFilters, lexical::search_indexed_sources},
    vault::{DirectorySync, DurableIo, NativeIo},
};
use std::{
    fs::File,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

fn inspect_v3(f: &Fixture, change: &PreparedChange) -> ChangeInspection {
    let inspection = f
        .app()
        .engine()
        .unwrap()
        .inspect(&change.change_id)
        .unwrap();
    assert_eq!(
        inspection.manifest.version, 3,
        "test must execute actual revision-tree route"
    );
    assert_eq!(inspection.manifest.revision_trees.len(), 2);
    for tree in &inspection.manifest.revision_trees {
        assert_eq!(tree.members.len(), 3);
        let mut names = tree
            .members
            .iter()
            .map(|i| {
                inspection.manifest.operations[*i]
                    .target
                    .as_str()
                    .rsplit('/')
                    .next()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["content.md", "original.bin", "revision.md"]);
    }
    assert!(!inspection.journal.frames.is_empty());
    assert!(
        inspection
            .journal
            .frames
            .iter()
            .all(|frame| frame.version == 2)
    );
    inspection
}
fn authored_page(f: &Fixture) -> (PathBuf, Vec<u8>, Vec<CitationRef>) {
    let citations = f
        .sources
        .iter()
        .map(|(source, revision, bytes)| {
            CitationRef::Source(SourceSpanRef {
                source_id: source.clone(),
                source_revision: revision.clone(),
                span: ByteSpan::new(0, bytes.len() as u64).unwrap(),
                quote_hash: Blake3Hash::digest(bytes),
            })
        })
        .collect::<Vec<_>>();
    let refs = PageSourceRefs::from_json_slice(
        &serde_json::to_vec(&json!({"schema_version":"1","citations":citations.clone()})).unwrap(),
    )
    .unwrap();
    f.app()
        .page_initialize_with_source_refs(
            Some(rel("pages/authored.md")),
            None,
            "Authored answer".into(),
            "# Answer\nKeep this author's exact words and historical citations.\n".into(),
            refs,
        )
        .unwrap();
    let path = f.physical("pages/authored.md");
    let bytes = fs::read(&path).unwrap();
    (path, bytes, citations)
}
fn old_trees(f: &Fixture) -> Vec<(PathBuf, BTreeMap<PathBuf, Vec<u8>>)> {
    f.sources
        .iter()
        .map(|(s, r, _)| {
            let path = f.physical(format!("sources/{s}/revisions/{r}"));
            let snapshot = canonical_snapshot(&path);
            (path, snapshot)
        })
        .collect()
}
fn assert_history(
    f: &Fixture,
    page: &(PathBuf, Vec<u8>, Vec<CitationRef>),
    old: &[(PathBuf, BTreeMap<PathBuf, Vec<u8>>)],
) {
    assert_eq!(fs::read(&page.0).unwrap(), page.1);
    for (path, bytes) in old {
        assert_eq!(&canonical_snapshot(path), bytes);
    }
    let app = f.app();
    let view = SourceView::from_fs_bounded(app.fs(), 4 * 1024 * 1024, 512).unwrap();
    for (i, citation) in page.2.iter().enumerate() {
        assert_eq!(
            view.verify(citation, CitationScope::Historical)
                .unwrap()
                .quote,
            f.sources[i].2
        );
    }
}
fn prepared_pair(f: &Fixture, changed: &[Vec<u8>; 2]) -> super::super::SourceRefreshBatchOutcome {
    let request = f.public_file(vec![f.item(1, &changed[1]), f.item(0, &changed[0])]);
    f.with_options(OperationOptions {
        stage_only: true,
        ..options()
    })
    .source_refresh_batch_file(&request)
    .unwrap()
}
fn assert_result_current(
    f: &Fixture,
    result: &super::super::SourceRefreshBatchOutcome,
    changed: &[Vec<u8>; 2],
) {
    for item in &result.items {
        let index = f
            .sources
            .iter()
            .position(|(s, _, _)| s == &item.source_id)
            .unwrap();
        f.assert_current(
            &item.source_id,
            item.revision_id.as_ref().unwrap(),
            &changed[index],
        );
    }
}
fn assert_search(f: &Fixture, result: &super::super::SourceRefreshBatchOutcome) {
    let app = f.app();
    let reader = app
        .catalog()
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    for item in &result.items {
        let plan = QueryPlan {
            filters: SearchFilters {
                source_ids: vec![item.source_id.clone()],
                ..Default::default()
            },
            ..Default::default()
        };
        let hits = search_indexed_sources(&reader, "Treeneedle090", &plan).unwrap();
        assert_eq!(
            hits.hits.len(),
            1,
            "orphans must not create duplicate searchable owners/evidence"
        );
        assert_eq!(hits.hits[0].source_id.as_ref(), Some(&item.source_id));
        assert_eq!(
            hits.hits[0].owner_revision.as_ref(),
            item.revision_id.as_ref()
        );
        assert!(hits.hits[0].excerpt.text.contains("Treeneedle090"));
    }
}
#[test]
fn two_source_prepared_v3_public_restart_apply_and_cache_loss_exclude_intact_and_tampered_orphans()
{
    let f = Fixture::new(true, 2);
    let page = authored_page(&f);
    let old = old_trees(&f);
    let changed = [
        b"# First\nTreeneedle090 blue capacity 41.\n".to_vec(),
        b"# Second\nTreeneedle090 green capacity 59.\n".to_vec(),
    ];
    let prepared = prepared_pair(&f, &changed);
    assert_eq!(prepared.status, Some(ChangeStatus::Prepared));
    let change = prepared.change.as_ref().unwrap();
    inspect_v3(&f, change);
    // Each app() is a fresh controller. Mutable caller bytes cannot alter retained allocations.
    for n in 0..2 {
        fs::write(
            f.inputs.join(format!("member {n}.md")),
            b"outside caller edited after preparation",
        )
        .unwrap();
    }
    assert_eq!(
        f.app()
            .changes_apply(change.change_id.clone())
            .unwrap()
            .status,
        Some(ChangeStatus::Committed)
    );
    let inspection = inspect_v3(&f, change);
    assert_eq!(
        inspection
            .journal
            .frames
            .iter()
            .filter(|frame| matches!(frame.event, ChangeEvent::TreeIntent { .. }))
            .count(),
        2
    );
    assert_eq!(
        inspection
            .journal
            .frames
            .iter()
            .filter(|frame| matches!(frame.event, ChangeEvent::TreeDone { .. }))
            .count(),
        2
    );
    assert_result_current(&f, &prepared, &changed);
    assert_history(&f, &page, &old);
    assert_search(&f, &prepared);
    let mut orphans = Vec::new();
    for item in &prepared.items {
        let parent = f.physical(format!("sources/{}/revisions", item.source_id));
        let container = parent.join(".wiki");
        for (name, tamper) in [
            ("unique-intact-orphan", false),
            ("unique-tampered-orphan", true),
        ] {
            let orphan = container.join(name);
            fs::create_dir(&orphan).unwrap();
            for member in ["revision.md", "original.bin", "content.md"] {
                fs::write(
                    orphan.join(member),
                    f.source_bytes(&item.source_id, item.revision_id.as_ref().unwrap(), member),
                )
                .unwrap();
            }
            if tamper {
                fs::write(
                    orphan.join("content.md"),
                    b"Treeneedle090 unfamiliar orphan tamper",
                )
                .unwrap();
                fs::write(
                    orphan.join("unfamiliar.md"),
                    b"# Hidden\nTreeneedle090 forbidden orphan evidence.\n",
                )
                .unwrap();
            }
            orphans.push((orphan.clone(), canonical_snapshot(&orphan)));
        }
    }
    let cache = f.physical(".wiki/cache");
    fs::remove_dir_all(cache).unwrap();
    f.app().index_rebuild_normalized().unwrap();
    let discovered = f.app().fs().root().scan_markdown().unwrap();
    assert!(
        discovered
            .iter()
            .all(|p| !p.as_str().contains("/revisions/.wiki/"))
    );
    assert_result_current(&f, &prepared, &changed);
    assert_history(&f, &page, &old);
    assert_search(&f, &prepared);
    for (orphan, bytes) in orphans {
        assert_eq!(canonical_snapshot(&orphan), bytes);
    }
    assert_eq!(
        f.app()
            .changes_apply(change.change_id.clone())
            .unwrap()
            .status,
        Some(ChangeStatus::Committed)
    );
}

struct RenameParentCut {
    native: NativeIo,
    source_parent: bool,
    armed: AtomicBool,
    directory_capability: AtomicBool,
    pending: Mutex<Option<PathBuf>>,
    renames: AtomicUsize,
    cuts: AtomicUsize,
}
impl RenameParentCut {
    fn new(source_parent: bool) -> Self {
        Self {
            native: NativeIo,
            source_parent,
            armed: AtomicBool::new(true),
            directory_capability: AtomicBool::new(true),
            pending: Mutex::new(None),
            renames: AtomicUsize::new(0),
            cuts: AtomicUsize::new(0),
        }
    }
}
impl DurableIo for RenameParentCut {
    fn create_stage(&self, p: &Path) -> std::io::Result<File> {
        self.native.create_stage(p)
    }
    fn create_private_stage(&self, p: &Path) -> std::io::Result<File> {
        self.native.create_private_stage(p)
    }
    fn create_private_directory(&self, p: &Path) -> std::io::Result<()> {
        self.native.create_private_directory(p)
    }
    fn open_append(&self, p: &Path) -> std::io::Result<File> {
        self.native.open_append(p)
    }
    fn truncate_file(&self, f: &File, n: u64) -> std::io::Result<()> {
        self.native.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut File, b: &[u8]) -> std::io::Result<()> {
        self.native.write_stage(f, b)
    }
    fn sync_file(&self, f: &File) -> std::io::Result<()> {
        self.native.sync_file(f)
    }
    fn replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        self.native.replace(a, b)
    }
    fn remove(&self, p: &Path) -> std::io::Result<()> {
        self.native.remove(p)
    }
    fn create_directory(&self, p: &Path) -> std::io::Result<()> {
        self.native.create_directory(p)
    }
    fn supports_directory_install(&self) -> bool {
        self.directory_capability.load(Ordering::SeqCst) && self.native.supports_directory_install()
    }
    fn install_directory_noreplace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        self.native.install_directory_noreplace(a, b)?;
        self.renames.fetch_add(1, Ordering::SeqCst);
        if self.armed.swap(false, Ordering::SeqCst) {
            *self.pending.lock().unwrap() = Some(
                if self.source_parent {
                    a.parent().unwrap()
                } else {
                    b.parent().unwrap()
                }
                .to_path_buf(),
            );
        }
        Ok(())
    }
    fn sync_directory(&self, p: &Path) -> std::io::Result<DirectorySync> {
        let mut pending = self.pending.lock().unwrap();
        if pending.as_deref() == Some(p) {
            pending.take();
            self.cuts.fetch_add(1, Ordering::SeqCst);
            return Err(std::io::ErrorKind::Other.into());
        }
        self.native.sync_directory(p)
    }
}
#[test]
fn public_v3_directory_rename_parent_sync_cuts_recover_original_allocations_history_and_page() {
    for source_parent in [false, true] {
        let f = Fixture::new(true, 2);
        let page = authored_page(&f);
        let old = old_trees(&f);
        let changed = [
            b"Treeneedle090 first recovery bytes.\n".to_vec(),
            b"Treeneedle090 second recovery bytes.\n".to_vec(),
        ];
        let prepared = prepared_pair(&f, &changed);
        let change = prepared.change.as_ref().unwrap();
        let before = inspect_v3(&f, change);
        let io = Arc::new(RenameParentCut::new(source_parent));
        let app = OfflineApp::new(
            VaultFs::with_io(VaultRoot::explicit(&f.root).unwrap(), io.clone()),
            options(),
        )
        .unwrap();
        assert!(app.changes_apply(change.change_id.clone()).is_err());
        assert_eq!(io.renames.load(Ordering::SeqCst), 1);
        assert_eq!(io.cuts.load(Ordering::SeqCst), 1);
        drop(app);
        let interrupted = inspect_v3(&f, change);
        assert!(
            interrupted
                .journal
                .frames
                .iter()
                .any(|frame| matches!(frame.event, ChangeEvent::TreeIntent { .. }))
        );
        assert!(
            !interrupted
                .journal
                .frames
                .iter()
                .any(|frame| matches!(frame.event, ChangeEvent::TreeDone { .. }))
        );
        let intent = interrupted
            .journal
            .frames
            .iter()
            .find_map(|frame| match frame.event {
                ChangeEvent::TreeIntent { group } => Some(group),
                _ => None,
            })
            .unwrap();
        let tree = &interrupted.manifest.revision_trees[intent];
        assert_eq!(
            fs::read_dir(f.physical(tree.root.as_str()))
                .unwrap()
                .count(),
            3,
            "rename cut retains complete canonical tree"
        );
        assert_history(&f, &page, &old);
        assert_eq!(
            f.app()
                .changes_apply(change.change_id.clone())
                .unwrap()
                .status,
            Some(ChangeStatus::Committed)
        );
        let after = inspect_v3(&f, change);
        assert_eq!(after.manifest.allocated_ids, before.manifest.allocated_ids);
        assert_eq!(after.prepared, before.prepared);
        assert_result_current(&f, &prepared, &changed);
        assert_history(&f, &page, &old);
        assert_search(&f, &prepared);
    }
}

#[test]
fn public_partial_tree_conflict_resume_and_abandon_preserve_external_bytes() {
    for occupied in 0..4 {
        let f = Fixture::new(true, 2);
        let page = authored_page(&f);
        let old = old_trees(&f);
        let changed = [
            b"Treeneedle090 first conflict proposal.\n".to_vec(),
            b"Treeneedle090 second conflict proposal.\n".to_vec(),
        ];
        let prepared = prepared_pair(&f, &changed);
        let change = prepared.change.as_ref().unwrap();
        let inspection = inspect_v3(&f, change);
        let root = f.physical(inspection.manifest.revision_trees[0].root.as_str());
        fs::create_dir(&root).unwrap();
        if occupied == 1 {
            fs::write(
                root.join("content.md"),
                b"unfamiliar external evidence preserved",
            )
            .unwrap();
        } else if occupied >= 2 {
            for &member in &inspection.manifest.revision_trees[0].members {
                let payload = f
                    .app()
                    .changes_payload(change.change_id.clone(), member)
                    .unwrap();
                if occupied == 3 || payload.target.as_str().ends_with("/content.md") {
                    fs::write(
                        f.physical(payload.target.as_str()),
                        payload.proposed.unwrap(),
                    )
                    .unwrap();
                }
            }
            // Partial exact-after/absent-before mixtures and complete exact trees
            // without their own intent must both be refused by the public route.
            assert!(
                f.app()
                    .engine()
                    .unwrap()
                    .inspect(&change.change_id)
                    .unwrap()
                    .observations
                    .iter()
                    .all(|o| o.observed == o.before || o.observed == o.after)
            );
        }
        let external = canonical_snapshot(&root);
        let apply_error = f.app().changes_apply(change.change_id.clone()).unwrap_err();
        assert_eq!(
            apply_error.code,
            crate::domain::ErrorCode::ContentConflict,
            "occupied mode {occupied}: public apply error message={} details={:?}",
            apply_error.message,
            apply_error.details
        );
        let app = f.app();
        let engine = app.engine().unwrap();
        assert_eq!(
            engine.inspect(&change.change_id).unwrap().status,
            ChangeStatus::Conflict,
            "occupied mode {occupied}: public apply refused without durable Conflict; error code={:?} message={} details={:?}",
            apply_error.code,
            apply_error.message,
            apply_error.details
        );
        let writer = WriterPermit::acquire(app.fs().root(), Duration::from_millis(200)).unwrap();
        for mode in [
            ConflictResolutionMode::Resume,
            ConflictResolutionMode::Abandon,
        ] {
            let request = engine.resolution_plan(change, mode).unwrap_or_else(|error| {
                panic!("occupied mode {occupied}, resolution {mode:?}: preview failed code={:?} message={} details={:?}",
                    error.code, error.message, error.details)
            });
            let resolution_error = engine
                .resolve(&writer, &request, &CatalogGraphValidator, &app.catalog())
                .unwrap_err();
            assert_eq!(
                resolution_error.code,
                crate::domain::ErrorCode::ContentConflict,
                "occupied mode {occupied}, resolution {mode:?}: expected preserved-tree conflict; error message={} details={:?}",
                resolution_error.message,
                resolution_error.details
            );
            assert_eq!(
                canonical_snapshot(&root),
                external,
                "occupied mode {occupied}, resolution {mode:?}: external root bytes changed"
            );
        }
        drop(writer);
        assert_history(&f, &page, &old);
        for (source, revision, bytes) in &f.sources {
            f.assert_current(source, revision, bytes);
        }
    }
}

#[test]
fn committed_v3_public_apply_refuses_missing_runtime_capability_without_writes() {
    let f = Fixture::new(true, 2);
    let page = authored_page(&f);
    let old = old_trees(&f);
    let changed = [
        b"Treeneedle090 committed capability first.\n".to_vec(),
        b"Treeneedle090 committed capability second.\n".to_vec(),
    ];
    let prepared = prepared_pair(&f, &changed);
    let change = prepared.change.as_ref().unwrap();
    inspect_v3(&f, change);
    assert_eq!(
        f.app()
            .changes_apply(change.change_id.clone())
            .unwrap()
            .status,
        Some(ChangeStatus::Committed)
    );
    let committed = inspect_v3(&f, change);
    assert_eq!(committed.status, ChangeStatus::Committed);
    let io = Arc::new(RenameParentCut::new(false));
    io.directory_capability.store(false, Ordering::SeqCst);
    let app = OfflineApp::new(
        VaultFs::with_io(VaultRoot::explicit(&f.root).unwrap(), io.clone()),
        options(),
    )
    .unwrap();
    let before = canonical_snapshot(&f.root);
    let error = app.changes_apply(change.change_id.clone()).unwrap_err();
    assert_eq!(error.code, crate::domain::ErrorCode::CapabilityUnavailable);
    assert_eq!(
        canonical_snapshot(&f.root),
        before,
        "terminal replay capability refusal must preserve canonical and retained bytes"
    );
    assert_eq!(io.renames.load(Ordering::SeqCst), 0);
    assert_eq!(io.cuts.load(Ordering::SeqCst), 0);
    let preview = OfflineApp::new(
        VaultFs::with_io(VaultRoot::explicit(&f.root).unwrap(), io),
        OperationOptions {
            dry_run: true,
            ..options()
        },
    )
    .unwrap();
    assert_eq!(
        preview
            .changes_apply(change.change_id.clone())
            .unwrap()
            .status,
        Some(ChangeStatus::Committed)
    );
    assert_eq!(
        preview
            .engine()
            .unwrap()
            .inspect(&change.change_id)
            .unwrap()
            .manifest
            .version,
        3
    );
    assert_eq!(
        canonical_snapshot(&f.root),
        before,
        "read-only preview must preserve canonical and retained bytes"
    );
    assert_result_current(&f, &prepared, &changed);
    assert_history(&f, &page, &old);
}
