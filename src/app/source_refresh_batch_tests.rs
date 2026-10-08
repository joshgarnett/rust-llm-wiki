//! Guarded public batch workflows in disposable normalized vaults; no providers.
use super::{
    OfflineApp, OperationOptions, PageSourceRefs, ReadRequest, RecordSelector,
    SourceRefreshBatchItem, SourceRefreshBatchRequest, offline::init,
};
use crate::{
    changes::ChangeStatus,
    domain::{Blake3Hash, ByteSpan, CitationRef, RecordId, SourceSpanRef, VaultRelativePath},
    sources::{CaptureRequest, CitationScope, ExtractionInput, SourceOrigin, SourceView},
    storage::{self, StorageOptions},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

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
fn capture(bytes: &[u8]) -> CaptureRequest {
    CaptureRequest {
        title: "Original Source title".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "original-input.md".into(),
        original: bytes.to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: Some("text/markdown".into()),
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    inputs: PathBuf,
    sources: Vec<(RecordId, RecordId, Vec<u8>)>,
}
impl Fixture {
    fn new(migrated: bool, count: usize) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("batch vault with spaces");
        init(&root, "Guarded batch tests", options()).unwrap();
        let vault = VaultFs::new(VaultRoot::explicit(&root).unwrap());
        if migrated {
            let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
            storage::cleanup(&vault, &writer, &StorageOptions::default()).unwrap();
        }
        let app = OfflineApp::new(vault, options()).unwrap();
        app.index_rebuild_normalized().unwrap();
        assert_eq!(storage::layout::active(app.fs().root()).unwrap(), migrated);
        let inputs = temp.path().join("caller input files");
        fs::create_dir(&inputs).unwrap();
        let sources = (0..count)
            .map(|i| {
                let bytes = format!(
                    "# Source {i}\nOriginal fact {i}: amber capacity {}.\n",
                    i + 17
                )
                .into_bytes();
                let result = app.source_add(capture(&bytes)).unwrap();
                (
                    result.allocated_ids["source"].clone(),
                    result.allocated_ids["revision"].clone(),
                    bytes,
                )
            })
            .collect();
        Self {
            _temp: temp,
            root,
            inputs,
            sources,
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
    fn physical(&self, path: impl Into<String>) -> PathBuf {
        self.app().fs().root().resolve(&rel(path)).unwrap()
    }
    fn head(&self, source: &RecordId) -> super::ReadOutcome {
        self.app()
            .read(ReadRequest {
                selector: RecordSelector::Id(source.clone()),
                range: None,
                max_bytes: 65536,
            })
            .unwrap()
    }
    fn item(&self, index: usize, bytes: &[u8]) -> SourceRefreshBatchItem {
        let source = &self.sources[index].0;
        let head = self.head(source);
        let file = PathBuf::from(format!("member {index}.md"));
        fs::write(self.inputs.join(&file), bytes).unwrap();
        SourceRefreshBatchItem {
            source_id: source.clone(),
            file,
            if_match: head.hash,
            expected_revision: RecordId::new(
                head.record
                    .unwrap()
                    .string("wiki_current_revision")
                    .unwrap(),
            )
            .unwrap(),
            input_hash: Blake3Hash::digest(bytes),
            title: None,
            media_type: Some("text/markdown".into()),
        }
    }
    fn public_file(&self, items: Vec<SourceRefreshBatchItem>) -> PathBuf {
        let path = self.inputs.join("request.json");
        fs::write(
            &path,
            serde_json::to_vec(&SourceRefreshBatchRequest { items }).unwrap(),
        )
        .unwrap();
        path
    }
    fn source_bytes(&self, source: &RecordId, revision: &RecordId, name: &str) -> Vec<u8> {
        fs::read(self.physical(format!("sources/{source}/revisions/{revision}/{name}"))).unwrap()
    }
    fn assert_current(&self, source: &RecordId, revision: &RecordId, bytes: &[u8]) {
        let head = self.head(source);
        assert_eq!(
            head.record.unwrap().string("wiki_current_revision"),
            Some(revision.as_str())
        );
        for name in ["original.bin", "content.md"] {
            assert_eq!(self.source_bytes(source, revision, name), bytes);
        }
        let read = self
            .app()
            .read(ReadRequest {
                selector: RecordSelector::Path(rel(format!(
                    "sources/{source}/revisions/{revision}/content.md"
                ))),
                range: None,
                max_bytes: 65536,
            })
            .unwrap();
        assert_eq!(read.body.as_bytes(), bytes);
        // This canonical OfflineApp read deliberately exposes no selected-verification
        // citation. Authenticate an exact public Source span through SourceView instead.
        assert!(read.source_citation.is_none());
        let citation = CitationRef::Source(SourceSpanRef {
            source_id: source.clone(),
            source_revision: revision.clone(),
            span: ByteSpan::new(0, bytes.len() as u64).unwrap(),
            quote_hash: Blake3Hash::digest(bytes),
        });
        let app = self.app();
        let view = SourceView::from_fs_bounded(app.fs(), 4 * 1024 * 1024, 512).unwrap();
        assert_eq!(
            view.verify(&citation, CitationScope::Current)
                .unwrap()
                .quote,
            bytes
        );
    }
}
// Canonical bytes plus publication authority, excluding rebuildable coordination.
fn canonical_snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap();
            if relative.starts_with(".wiki/cache")
                || relative == Path::new(".wiki/state/writer.lock")
            {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(relative.to_owned(), fs::read(&path).unwrap());
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}
fn whole_snapshot(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    fn walk(
        root: &Path,
        dir: &Path,
        out: &mut BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)>,
    ) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    (
                        fs::read(&path).unwrap(),
                        fs::metadata(&path).unwrap().modified().unwrap(),
                    ),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}

#[test]
fn one_and_sixteen_public_file_members_preserve_identity_history_and_current_reads_in_both_layouts()
{
    for migrated in [false, true] {
        for count in [1, 16] {
            let f = Fixture::new(migrated, count);
            let changed: Vec<_> = (0..count)
                .map(|i| format!("# New fact {i}\nBlue capacity {}.\n", 31 + i).into_bytes())
                .collect();
            // Reverse caller order exercises sorted internal planning versus ordinal results.
            let items = (0..count).rev().map(|i| f.item(i, &changed[i])).collect();
            let old_trees: Vec<_> = f
                .sources
                .iter()
                .map(|(source, revision, _)| {
                    let path = f.physical(format!("sources/{source}/revisions/{revision}"));
                    (path.clone(), canonical_snapshot(&path))
                })
                .collect();
            let request = f.public_file(items);
            let caller_before = whole_snapshot(&f.inputs);
            let outcome = f.app().source_refresh_batch_file(&request).unwrap();
            assert_eq!(outcome.status, Some(ChangeStatus::Committed));
            assert!(outcome.change.is_some());
            assert_eq!(outcome.items.len(), count);
            for (ordinal, item) in outcome.items.iter().enumerate() {
                let index = count - 1 - ordinal;
                let (source, old, original) = &f.sources[index];
                assert_eq!(item.ordinal, ordinal);
                assert_eq!(&item.source_id, source);
                assert_eq!(item.previous_revision_id.as_ref(), Some(old));
                assert_eq!(item.reused, Some(false));
                assert_eq!(item.no_op, Some(false));
                assert!(item.capture_state.is_some());
                let new = item.revision_id.as_ref().unwrap();
                assert_ne!(new, old);
                f.assert_current(source, new, &changed[index]);
                for name in ["original.bin", "content.md"] {
                    assert_eq!(f.source_bytes(source, old, name), *original);
                }
            }
            for (path, bytes) in old_trees {
                assert_eq!(
                    canonical_snapshot(&path),
                    bytes,
                    "old immutable tree changed"
                );
            }
            assert_eq!(whole_snapshot(&f.inputs), caller_before);
        }
    }
}

#[test]
fn all_noop_is_read_only_and_mixed_noop_preserves_unchanged_head() {
    for migrated in [false, true] {
        let f = Fixture::new(migrated, 2);
        let noop: Vec<_> = (0..2).map(|i| f.item(i, &f.sources[i].2)).collect();
        let request = f.public_file(noop);
        let before = canonical_snapshot(&f.root);
        let outcome = f.app().source_refresh_batch_file(&request).unwrap();
        assert!(outcome.change.is_none());
        assert!(outcome.status.is_none());
        assert!(
            outcome
                .items
                .iter()
                .all(|i| i.no_op == Some(true) && i.reused == Some(true))
        );
        assert_eq!(canonical_snapshot(&f.root), before);
        let changed = b"# Changed\nOnly first member changes.\n";
        let request = f.public_file(vec![f.item(0, changed), f.item(1, &f.sources[1].2)]);
        let result = f.app().source_refresh_batch_file(&request).unwrap();
        assert_eq!(result.items[0].no_op, Some(false));
        assert_eq!(result.items[1].no_op, Some(true));
        assert_eq!(result.items[1].revision_id.as_ref(), Some(&f.sources[1].1));
        f.assert_current(&f.sources[1].0, &f.sources[1].1, &f.sources[1].2);
    }
}

#[test]
fn explicit_title_and_historical_reuse_keep_exact_old_citation_and_authored_page() {
    for migrated in [false, true] {
        let f = Fixture::new(migrated, 1);
        let (source, old, original) = &f.sources[0];
        let citation = CitationRef::Source(SourceSpanRef {
            source_id: source.clone(),
            source_revision: old.clone(),
            span: ByteSpan::new(0, original.len() as u64).unwrap(),
            quote_hash: Blake3Hash::digest(original),
        });
        let refs = PageSourceRefs::from_json_slice(
            &serde_json::to_vec(&json!({"schema_version":"1","citations":[citation.clone()]}))
                .unwrap(),
        )
        .unwrap();
        f.app()
            .page_initialize_with_source_refs(
                Some(rel("pages/authored.md")),
                None,
                "Authored answer".into(),
                "# Answer\nThe author keeps this wording and exact old citation.\n".into(),
                refs,
            )
            .unwrap();
        let page = f.physical("pages/authored.md");
        let before = fs::read(&page).unwrap();
        let mut item = f.item(0, b"# Replacement\nNew green capacity.\n");
        item.title = Some("Explicit current Source title".into());
        let first = f
            .app()
            .source_refresh_batch_file(&f.public_file(vec![item]))
            .unwrap();
        assert_eq!(
            f.head(source).record.unwrap().string("title"),
            Some("Explicit current Source title")
        );
        assert_ne!(first.items[0].revision_id.as_ref(), Some(old));
        let app = f.app();
        let view = SourceView::from_fs_bounded(app.fs(), 1024 * 1024, 128).unwrap();
        assert_eq!(
            view.verify(&citation, CitationScope::Historical)
                .unwrap()
                .quote,
            *original
        );
        assert_eq!(fs::read(&page).unwrap(), before);
        let result = f
            .app()
            .source_refresh_batch_file(&f.public_file(vec![f.item(0, original)]))
            .unwrap();
        assert_eq!(result.items[0].reused, Some(true));
        assert_eq!(result.items[0].no_op, Some(false));
        assert_eq!(result.items[0].revision_id.as_ref(), Some(old));
        f.assert_current(source, old, original);
        assert_eq!(fs::read(&page).unwrap(), before);
    }
}

#[test]
fn mandatory_hash_head_input_and_withdrawal_guards_refuse_before_canonical_mutation() {
    let f = Fixture::new(false, 1);
    let valid = f.item(0, b"New guarded payload.\n");
    for which in 0..3 {
        let mut bad = valid.clone();
        match which {
            0 => bad.if_match = Blake3Hash::digest(b"wrong Source envelope"),
            1 => bad.expected_revision = RecordId::new("revision_wrong_head").unwrap(),
            _ => bad.input_hash = Blake3Hash::digest(b"wrong caller input"),
        }
        let file = f.public_file(vec![bad]);
        let before = canonical_snapshot(&f.root);
        assert!(f.app().source_refresh_batch_file(&file).is_err());
        assert_eq!(canonical_snapshot(&f.root), before);
    }
    let drift = f.item(0, b"Sealed bytes before external edit.\n");
    let input = f.inputs.join(&drift.file);
    let file = f.public_file(vec![drift]);
    fs::write(input, b"Unreviewed caller drift.\n").unwrap();
    let before = canonical_snapshot(&f.root);
    assert!(f.app().source_refresh_batch_file(&file).is_err());
    assert_eq!(canonical_snapshot(&f.root), before);
    f.app()
        .source_withdraw(f.sources[0].0.clone(), "Explicit withdrawal")
        .unwrap();
    let item = f.item(0, b"Must not reactivate.\n");
    let file = f.public_file(vec![item]);
    let before = canonical_snapshot(&f.root);
    assert!(f.app().source_refresh_batch_file(&file).is_err());
    assert_eq!(canonical_snapshot(&f.root), before);
}

#[test]
fn malformed_requests_duplicates_aliases_and_bounds_refuse_without_changes() {
    let f = Fixture::new(false, 2);
    let a = f.item(0, b"First changed.\n");
    let b = f.item(1, b"Second changed.\n");
    let mut alias = b.clone();
    alias.file = PathBuf::from(format!("./{}", a.file.display()));
    alias.input_hash = a.input_hash.clone();
    for items in [
        vec![],
        vec![a.clone(), a.clone()],
        vec![a.clone(); 17],
        vec![a.clone(), alias],
    ] {
        let file = f.public_file(items);
        let before = canonical_snapshot(&f.root);
        assert!(f.app().source_refresh_batch_file(&file).is_err());
        assert_eq!(canonical_snapshot(&f.root), before);
    }
    for field in ["if_match", "expected_revision", "input_hash"] {
        let mut value = serde_json::to_value(&SourceRefreshBatchRequest {
            items: vec![a.clone()],
        })
        .unwrap();
        value["items"][0].as_object_mut().unwrap().remove(field);
        let file = f.inputs.join("missing.json");
        fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
        let before = canonical_snapshot(&f.root);
        assert!(f.app().source_refresh_batch_file(&file).is_err());
        assert_eq!(canonical_snapshot(&f.root), before);
    }
    let file = f.inputs.join("oversized.json");
    fs::write(&file, vec![b' '; 1024 * 1024 + 1]).unwrap();
    let before = canonical_snapshot(&f.root);
    assert!(f.app().source_refresh_batch_file(&file).is_err());
    assert_eq!(canonical_snapshot(&f.root), before);
    let left = vec![b'a'; 2 * 1024 * 1024];
    let right = vec![b'b'; 2 * 1024 * 1024 + 1];
    let file = f.public_file(vec![f.item(0, &left), f.item(1, &right)]);
    let before = canonical_snapshot(&f.root);
    assert!(f.app().source_refresh_batch_file(&file).is_err());
    assert_eq!(canonical_snapshot(&f.root), before);
}

#[test]
fn stdin_directory_and_symlink_inputs_are_not_accepted_as_member_files() {
    let f = Fixture::new(false, 1);
    let valid = f.item(0, b"Input changed.\n");
    // A real caller file named '-' must not accidentally enable the stdin alias.
    fs::write(f.inputs.join("-"), b"Input changed.\n").unwrap();
    for file in [PathBuf::from("-"), PathBuf::from(".")] {
        let mut item = valid.clone();
        item.file = file;
        let request = f.public_file(vec![item]);
        let before = canonical_snapshot(&f.root);
        assert!(f.app().source_refresh_batch_file(&request).is_err());
        assert_eq!(canonical_snapshot(&f.root), before);
    }
    #[cfg(unix)]
    {
        let link = f.inputs.join("linked.md");
        std::os::unix::fs::symlink(f.inputs.join(&valid.file), &link).unwrap();
        let mut item = valid;
        item.file = PathBuf::from("linked.md");
        let request = f.public_file(vec![item]);
        let before = canonical_snapshot(&f.root);
        assert!(f.app().source_refresh_batch_file(&request).is_err());
        assert_eq!(canonical_snapshot(&f.root), before);
    }
}

#[test]
fn dry_run_checks_public_bytes_with_missing_cache_and_creates_no_locks_or_cache() {
    for migrated in [false, true] {
        let f = Fixture::new(migrated, 1);
        let file = f.public_file(vec![f.item(0, b"Preview input only.\n")]);
        let cache = f.physical(".wiki/cache");
        if cache.exists() {
            fs::remove_dir_all(cache).unwrap();
        }
        let preview = f.with_options(OperationOptions {
            dry_run: true,
            ..options()
        });
        let before = whole_snapshot(&f.root);
        let result = preview.source_refresh_batch_file(&file).unwrap();
        assert!(result.dry_run);
        assert!(result.change.is_none());
        let item = &result.items[0];
        assert!(
            item.previous_revision_id.is_none()
                && item.revision_id.is_none()
                && item.no_op.is_none()
                && item.reused.is_none()
        );
        assert_eq!(whole_snapshot(&f.root), before);
    }
}

#[test]
fn staged_change_uses_frozen_input_and_refuses_later_source_author_edit() {
    for migrated in [false, true] {
        let f = Fixture::new(migrated, 1);
        let original_new = b"Frozen staged evidence.\n";
        let item = f.item(0, original_new);
        let input = f.inputs.join(&item.file);
        let file = f.public_file(vec![item]);
        let stage = f.with_options(OperationOptions {
            stage_only: true,
            ..options()
        });
        let prepared = stage.source_refresh_batch_file(&file).unwrap();
        assert_eq!(prepared.status, Some(ChangeStatus::Prepared));
        fs::write(&input, b"Caller changed external bytes after staging.\n").unwrap();
        let result = f
            .app()
            .changes_apply(prepared.change.unwrap().change_id)
            .unwrap();
        assert_eq!(result.status, Some(ChangeStatus::Committed));
        f.assert_current(
            &f.sources[0].0,
            prepared.items[0].revision_id.as_ref().unwrap(),
            original_new,
        );
        let file = f.public_file(vec![f.item(0, b"Second staged proposal.\n")]);
        let staged = stage.source_refresh_batch_file(&file).unwrap();
        let path = f.physical(format!("sources/{}/source.md", f.sources[0].0));
        let mut author = fs::read(&path).unwrap();
        author.extend_from_slice(b"\nAuthor note edited outside the application.\n");
        fs::write(&path, &author).unwrap();
        assert!(
            f.app()
                .changes_apply(staged.change.unwrap().change_id)
                .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), author);
    }
}

#[test]
fn staged_mixed_batch_retains_the_noop_members_source_guard() {
    for migrated in [false, true] {
        let f = Fixture::new(migrated, 2);
        let file = f.public_file(vec![
            f.item(0, b"First member candidate.\n"),
            f.item(1, &f.sources[1].2),
        ]);
        let staged = f
            .with_options(OperationOptions {
                stage_only: true,
                ..options()
            })
            .source_refresh_batch_file(&file)
            .unwrap();
        assert_eq!(staged.items[1].no_op, Some(true));
        let changed = f
            .app()
            .source_refresh(
                f.sources[1].0.clone(),
                capture(b"Noop member changed after staging.\n"),
            )
            .unwrap();
        let before = f.source_bytes(
            &f.sources[1].0,
            &changed.allocated_ids["revision"],
            "content.md",
        );
        assert!(
            f.app()
                .changes_apply(staged.change.unwrap().change_id)
                .is_err()
        );
        f.assert_current(&f.sources[0].0, &f.sources[0].1, &f.sources[0].2);
        f.assert_current(&f.sources[1].0, &changed.allocated_ids["revision"], &before);
    }
}

#[test]
fn typed_request_keeps_distinct_source_identities_for_identical_new_payloads() {
    let f = Fixture::new(false, 2);
    let bytes = b"Identical caller bytes belong to two independently captured Sources.\n";
    let request = SourceRefreshBatchRequest {
        items: vec![f.item(0, bytes), f.item(1, bytes)],
    };
    let outcome = f.app().source_refresh_batch(request, &f.inputs).unwrap();
    assert_eq!(outcome.status, Some(ChangeStatus::Committed));
    assert_ne!(outcome.items[0].source_id, outcome.items[1].source_id);
    assert_ne!(outcome.items[0].revision_id, outcome.items[1].revision_id);
    for (index, item) in outcome.items.iter().enumerate() {
        assert_eq!(item.source_id, f.sources[index].0);
        assert_eq!(item.reused, Some(false));
        f.assert_current(&item.source_id, item.revision_id.as_ref().unwrap(), bytes);
    }
}

#[test]
fn escaped_maximum_title_and_media_roundtrip_and_preview_enforce_decoded_field_bounds() {
    let f = Fixture::new(false, 1);
    // JSON escaping expands these caller fields while the decoded UTF-8 bounds stay exact.
    let title = format!("{}x", "\u{0001}".repeat(4095));
    let media = format!("{}x", "\u{0002}".repeat(511));
    assert_eq!(title.len(), 4096);
    assert_eq!(media.len(), 512);
    let mut item = f.item(0, b"Escaped metadata remains explicit caller input.\n");
    item.title = Some(title.clone());
    item.media_type = Some(media.clone());
    let file = f.public_file(vec![item.clone()]);
    let preview = f.with_options(OperationOptions {
        dry_run: true,
        ..options()
    });
    let before = whole_snapshot(&f.root);
    let result = preview.source_refresh_batch_file(&file).unwrap();
    assert!(result.items[0].capture_state.is_none());
    assert_eq!(whole_snapshot(&f.root), before);
    for field in ["title", "media_type"] {
        let mut over = item.clone();
        if field == "title" {
            over.title.as_mut().unwrap().push('x');
        } else {
            over.media_type.as_mut().unwrap().push('x');
        }
        let request = f.public_file(vec![over]);
        assert!(preview.source_refresh_batch_file(&request).is_err());
        assert_eq!(whole_snapshot(&f.root), before);
    }
    // Restore the admitted request and exercise real canonical serialization, not just parsing.
    let result = f
        .app()
        .source_refresh_batch_file(&f.public_file(vec![item]))
        .unwrap();
    assert_eq!(result.status, Some(ChangeStatus::Committed));
    assert_eq!(f.head(&f.sources[0].0).record.unwrap().title(), title);
    let revision = result.items[0].revision_id.as_ref().unwrap();
    let note = f
        .app()
        .read(ReadRequest {
            selector: RecordSelector::Id(revision.clone()),
            range: None,
            max_bytes: 65536,
        })
        .unwrap()
        .record
        .unwrap();
    assert_eq!(note.string("wiki_media_type"), Some(media.as_str()));
}

#[test]
fn source_authored_body_and_long_retained_inventory_survive_batch_head_change() {
    for migrated in [false, true] {
        let f = Fixture::new(migrated, 1);
        let source = &f.sources[0].0;
        for n in 0..12 {
            f.app()
                .source_refresh(
                    source.clone(),
                    capture(format!("Retained observation {n}: old exact evidence.\n").as_bytes()),
                )
                .unwrap();
        }
        let source_path = f.physical(format!("sources/{source}/source.md"));
        let author = "Author note: keep every character and old provenance. café 東京.\n"
            .repeat(2048)
            .into_bytes();
        let mut source_bytes = fs::read(&source_path).unwrap();
        source_bytes.extend_from_slice(&author);
        fs::write(&source_path, source_bytes).unwrap();
        f.app().index_sync(false).unwrap();
        let history_root = f.physical(format!("sources/{source}/revisions"));
        let old_history = canonical_snapshot(&history_root);
        assert_eq!(fs::read_dir(&history_root).unwrap().count(), 13);
        let result = f
            .app()
            .source_refresh_batch_file(
                &f.public_file(vec![f.item(0, b"New batch head after long history.\n")]),
            )
            .unwrap();
        assert_eq!(result.status, Some(ChangeStatus::Committed));
        assert_eq!(result.items[0].source_id, *source);
        assert_eq!(result.items[0].reused, Some(false));
        assert!(fs::read(&source_path).unwrap().ends_with(&author));
        for (path, bytes) in old_history {
            assert_eq!(fs::read(history_root.join(path)).unwrap(), bytes);
        }
        assert_eq!(fs::read_dir(&history_root).unwrap().count(), 14);
        let head = f.head(source).record.unwrap();
        assert_eq!(
            head.field("wiki_revisions")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            14
        );
        f.assert_current(
            source,
            result.items[0].revision_id.as_ref().unwrap(),
            b"New batch head after long history.\n",
        );
    }
}

#[test]
fn cumulative_large_author_notes_refuse_before_change_while_one_member_is_supported() {
    let f = Fixture::new(false, 10);
    // Source body+raw_text each share the existing 8MiB serialized row allowance.
    // Individually admissible 1.5MiB author notes and 2MiB captured heads exercise
    // cumulative selected reads/proposals, not an oversized single document row.
    let prior = vec![b'b'; 2 * 1024 * 1024];
    for (source, _, _) in &f.sources {
        f.app()
            .source_refresh(source.clone(), capture(&prior))
            .unwrap();
    }
    let author = vec![b'a'; 1536 * 1024];
    for (source, _, _) in &f.sources {
        let path = f.physical(format!("sources/{source}/source.md"));
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(b"\nCaller-authored large note follows.\n");
        bytes.extend_from_slice(&author);
        bytes.push(b'\n');
        fs::write(path, bytes).unwrap();
    }
    f.app().index_sync(false).unwrap();
    let single = f.public_file(vec![
        f.item(0, b"One large-note Source refresh remains supported.\n"),
    ]);
    assert_eq!(
        f.app().source_refresh_batch_file(&single).unwrap().status,
        Some(ChangeStatus::Committed)
    );
    let items = (0..10)
        .map(|n| f.item(n, format!("Combined new observation {n}.\n").as_bytes()))
        .collect();
    let request = f.public_file(items);
    let before = canonical_snapshot(&f.root);
    let error = f.app().source_refresh_batch_file(&request).unwrap_err();
    assert_eq!(error.code, crate::domain::ErrorCode::BudgetExceeded);
    assert_eq!(
        canonical_snapshot(&f.root),
        before,
        "budget refusal wrote a Change or canonical content"
    );
}

#[test]
fn bom_and_crlf_noop_preserves_exact_capture_bytes_and_revision_metadata() {
    for migrated in [false, true] {
        let mut f = Fixture::new(migrated, 0);
        let bytes =
            b"\xef\xbb\xbf# Exact capture\r\nAuthor chose CRLF and a UTF-8 BOM.\r\n".to_vec();
        let added = f.app().source_add(capture(&bytes)).unwrap();
        f.sources.push((
            added.allocated_ids["source"].clone(),
            added.allocated_ids["revision"].clone(),
            bytes.clone(),
        ));
        let file = f.public_file(vec![f.item(0, &bytes)]);
        let before = canonical_snapshot(&f.root);
        let outcome = f.app().source_refresh_batch_file(&file).unwrap();
        assert!(outcome.change.is_none());
        assert_eq!(outcome.items[0].no_op, Some(true));
        assert_eq!(outcome.items[0].reused, Some(true));
        assert_eq!(canonical_snapshot(&f.root), before);
        f.assert_current(&f.sources[0].0, &f.sources[0].1, &bytes);
    }
}

#[test]
fn writer_wait_exhausts_shared_deadline_before_preparation_in_both_layouts() {
    use crate::{catalog::source_projection::RefreshProjectionLimits, domain::ErrorCode};
    use std::sync::mpsc;

    for migrated in [false, true] {
        let f = Fixture::new(migrated, 1);
        let request = SourceRefreshBatchRequest {
            items: vec![f.item(0, b"Changed capture waiting for writer authority.\n")],
        };
        let mut noop = f.item(0, &f.sources[0].2);
        noop.file = PathBuf::from("unchanged control.md");
        fs::write(f.inputs.join(&noop.file), &f.sources[0].2).unwrap();
        // Restore the changed caller file after constructing the separate no-op control.
        fs::write(
            f.inputs.join(&request.items[0].file),
            b"Changed capture waiting for writer authority.\n",
        )
        .unwrap();
        let before = canonical_snapshot(&f.root);
        let vault = VaultFs::new(VaultRoot::explicit(&f.root).unwrap());
        let held = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
        let app = f.with_options(OperationOptions {
            lock_timeout_ms: 5_000,
            stage_only: true,
            ..options()
        });
        let (tx, rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let input_dir = &f.inputs;
            scope.spawn(move || {
                let result = app.source_refresh_batch_bounded(
                    request,
                    input_dir,
                    RefreshProjectionLimits {
                        max_elapsed: Duration::from_secs(1),
                        ..Default::default()
                    },
                );
                tx.send(result).unwrap();
            });
            assert!(
                matches!(
                    rx.recv_timeout(Duration::from_millis(1_200)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ),
                "the tiny plan must reach the held writer rather than fail before acquisition"
            );
            let unchanged = f
                .app()
                .source_refresh_batch_bounded(
                    SourceRefreshBatchRequest { items: vec![noop] },
                    &f.inputs,
                    RefreshProjectionLimits::default(),
                )
                .unwrap();
            assert!(unchanged.change.is_none());
            assert_eq!(canonical_snapshot(&f.root), before);
            drop(held);
            let error = rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::BudgetExceeded);
        });
        assert_eq!(
            canonical_snapshot(&f.root),
            before,
            "deadline exhaustion must precede retention and publication"
        );
    }
}
