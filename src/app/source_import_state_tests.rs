#![cfg(unix)]

use super::*;
use crate::{
    changes::{PreparedChange, types::NamedChangeIdentity},
    sources::{
        CaptureRequest, ExtractionInput, SourceOrigin, SourceStore,
        import_manifest_types::{ImportExtraction, ImportManifestItem},
        types::CaptureAllocation,
    },
    vault::{NativeIo, VaultRoot},
};
use std::{io::Write, time::Duration};
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
struct Fixture {
    _temp: tempfile::TempDir,
    fs: VaultFs,
    writer: WriterPermit,
    store: ImportStore,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"),"---\nwiki_schema: '1'\nwiki_id: vault_import_state\nwiki_kind: vault\ntitle: Import state\n---\n").unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs.root(), Duration::ZERO).unwrap();
        let store =
            ImportStore::new(fs.clone(), id("vault_import_state"), "owned import key").unwrap();
        store.initialize_directory(&writer).unwrap();
        Self {
            _temp: temp,
            fs,
            writer,
            store,
        }
    }
    fn initial(&self) -> ImportProgress {
        let manifest = StoredImportManifest {
            hash: Blake3Hash::digest(b"manifest bytes"),
            items: 1,
            bytes: 100,
            first_item_offset: 20,
        };
        ImportProgress {
            version: IMPORT_STATE_VERSION,
            vault_id: id("vault_import_state"),
            key: "owned import key".into(),
            results_hash: self.store.initial_results_hash(&manifest).unwrap(),
            manifest,
            group_size: 4,
            next_ordinal: 0,
            manifest_offset: 20,
            groups_committed: 0,
            results_offset: 0,
            last_group: None,
            pending: None,
            completed: false,
        }
    }
    fn pending(&self) -> ImportProgress {
        let mut p = self.initial();
        p.pending = Some(ImportPendingGroup {
            input_intent: None,
            group: 0,
            first_ordinal: 0,
            next_manifest_offset: 80,
            captures: vec![ImportPendingCapture {
                item: ImportManifestItem {
                    ordinal: 0,
                    path: self
                        ._temp
                        .path()
                        .join("outside.txt")
                        .to_str()
                        .unwrap()
                        .into(),
                    title: "Imported exact source".into(),
                    media_type: Some("text/plain".into()),
                    original_hash: Blake3Hash::digest(b"original"),
                    byte_len: 8,
                    extraction: ImportExtraction::Utf8Preserve,
                },
                allocation: CaptureAllocation {
                    source_id: id("source_import_state"),
                    revision_id: id("revision_import_state"),
                    captured_at: "2026-10-04T00:00:00Z".into(),
                },
            }],
            change: NamedChangeIdentity {
                change_id: id("change_import_state"),
                created_at: "2026-10-04T00:00:00Z".into(),
            },
            intent: None,
        });
        p
    }
    fn event(&self, p: &ImportProgress) -> ImportResultEvent {
        let pending = p.pending.as_ref().unwrap();
        ImportResultEvent::GroupCommitted {
            result: ImportGroupResult {
                group: pending.group,
                change: PreparedChange {
                    change_id: pending.change.change_id.clone(),
                    manifest_hash: Blake3Hash::digest(
                        b"test result comparison only; not application authority",
                    ),
                },
                items: pending
                    .captures
                    .iter()
                    .map(|capture| ImportedItem {
                        ordinal: capture.item.ordinal,
                        source_id: capture.allocation.source_id.clone(),
                        revision_id: capture.allocation.revision_id.clone(),
                    })
                    .collect(),
            },
        }
    }
    fn write_progress(&self, p: &ImportProgress) -> Blake3Hash {
        self.store
            .save(&self.writer, p, ExpectedState::Absent)
            .unwrap()
    }
    fn results(&self) -> std::path::PathBuf {
        self.fs
            .root()
            .path()
            .join(self.store.results_path().as_str())
    }
    fn intent(&self) -> NamedIndexedIntent {
        let catalog = crate::catalog::Catalog::new(self.fs.clone(), id("vault_import_state"));
        catalog.rebuild_normalized(&self.writer).unwrap();
        let pending = self.pending().pending.unwrap();
        let capture = &pending.captures[0];
        let plan = SourceStore::new(self.fs.clone())
            .plan_capture_named(
                CaptureRequest {
                    title: capture.item.title.clone(),
                    origin_kind: SourceOrigin::LocalFile,
                    origin: capture.item.path.clone(),
                    original: b"original".to_vec(),
                    extraction: ExtractionInput::Utf8Preserve,
                    media_type: capture.item.media_type.clone(),
                },
                &capture.allocation,
            )
            .unwrap();
        let reader = catalog
            .query_snapshot(crate::catalog::query_types::QueryReadLimits::default())
            .unwrap();
        let projected = crate::catalog::capture_projection::project_capture_batch(
            &self.fs,
            &reader,
            vec![plan],
            &crate::catalog::source_projection::RefreshProjectionLimits::default(),
        )
        .unwrap();
        drop(reader);
        crate::catalog::source_refresh::IndexedRefreshSession::seal_named_write(
            &catalog,
            &self.writer,
            projected,
            pending.change,
        )
        .unwrap()
        .intent()
        .clone()
    }
}
#[test]
fn import_store_constructor_is_pure_and_key_paths_are_exact_flat_names() {
    let fixture = Fixture::new();
    let root = fixture.fs.root().path().to_path_buf();
    drop(fixture.writer);
    fs::remove_dir_all(&root).unwrap();
    let store = ImportStore::new(fixture.fs, id("vault_import_state"), "Café 東京 import").unwrap();
    assert!(!root.exists());
    assert_eq!(
        store.state.as_str(),
        format!(
            "{ANCHOR}/{}.json",
            Blake3Hash::digest("Café 東京 import".as_bytes()).hex()
        )
    );
    for key in ["", "line\nkey", "nul\0key", "\u{0085}"] {
        assert!(ImportStore::new(store.fs.clone(), id("vault_import_state"), key).is_err());
    }
    assert!(
        ImportStore::new(store.fs.clone(), id("vault_import_state"), &"x".repeat(513)).is_err()
    );
    assert!(ImportStore::new(store.fs.clone(), id("vault_import_state"), &"x".repeat(512)).is_ok());
}
#[test]
fn import_state_checksum_raw_hash_compare_replace_and_immutable_header() {
    let fixture = Fixture::new();
    let p = fixture.initial();
    let hash = fixture.write_progress(&p);
    assert_eq!(
        fixture.store.load().unwrap(),
        Some((p.clone(), hash.clone()))
    );
    assert!(
        fixture
            .store
            .save(&fixture.writer, &p, ExpectedState::Absent)
            .is_err()
    );
    assert!(
        fixture
            .store
            .save(
                &fixture.writer,
                &p,
                ExpectedState::Hash(Blake3Hash::digest(b"wrong"))
            )
            .is_err()
    );
    let mut changed = p.clone();
    changed.group_size = 8;
    assert!(
        fixture
            .store
            .save(&fixture.writer, &changed, ExpectedState::Hash(hash.clone()))
            .is_err()
    );
    let envelope = ImportStateEnvelope {
        progress: p.clone(),
        checksum: Blake3Hash::digest(serde_json::to_vec(&p).unwrap()),
    };
    let pretty = serde_json::to_vec_pretty(&envelope).unwrap();
    fs::write(
        fixture.fs.root().path().join(fixture.store.state.as_str()),
        &pretty,
    )
    .unwrap();
    let raw_hash = fixture.store.load().unwrap().unwrap().1;
    assert_eq!(raw_hash, Blake3Hash::digest(&pretty));
    assert_ne!(raw_hash, hash);
    assert_eq!(
        fixture
            .store
            .save(&fixture.writer, &p, ExpectedState::Hash(raw_hash))
            .unwrap(),
        hash
    );
}
#[test]
fn import_state_rejects_invalid_progress_shapes_and_unknown_corrupt_headers() {
    let fixture = Fixture::new();
    let good = fixture.pending();
    for variant in 0..12 {
        let mut p = good.clone();
        match variant {
            0 => p.version = 99,
            1 => p.vault_id = id("vault_foreign"),
            2 => p.key = "other".into(),
            3 => p.group_size = 9,
            4 => p.next_ordinal = 2,
            5 => p.manifest_offset = 101,
            6 => p.results_offset = MAX_IMPORT_RESULTS_BYTES + 1,
            7 => p.completed = true,
            8 => p.pending.as_mut().unwrap().group = 1,
            9 => p.pending.as_mut().unwrap().captures[0].item.ordinal = 1,
            10 => {
                p.pending.as_mut().unwrap().captures[0]
                    .allocation
                    .revision_id = p.pending.as_ref().unwrap().captures[0]
                    .allocation
                    .source_id
                    .clone()
            }
            _ => {
                p.pending.as_mut().unwrap().captures[0]
                    .allocation
                    .captured_at = "bad".into()
            }
        }
        assert!(
            fixture
                .store
                .save(&fixture.writer, &p, ExpectedState::Absent)
                .is_err(),
            "variant {variant}"
        );
    }
    let target = fixture.fs.root().path().join(fixture.store.state.as_str());
    fs::write(&target, b"foreign unknown bytes").unwrap();
    assert!(fixture.store.load().is_err());
    assert!(
        fixture
            .store
            .save(&fixture.writer, &good, ExpectedState::Absent)
            .is_err()
    );
    assert_eq!(fs::read(&target).unwrap(), b"foreign unknown bytes");
    let mut envelope = ImportStateEnvelope {
        progress: good,
        checksum: Blake3Hash::digest(b"wrong"),
    };
    fs::write(&target, serde_json::to_vec(&envelope).unwrap()).unwrap();
    assert!(fixture.store.load().is_err());
    envelope.checksum = Blake3Hash::digest(serde_json::to_vec(&envelope.progress).unwrap());
    let mut value = serde_json::to_string(&envelope).unwrap();
    value.insert_str(
        1,
        "\"checksum\":\"blake3:0000000000000000000000000000000000000000000000000000000000000000\",",
    );
    fs::write(&target, value).unwrap();
    assert!(fixture.store.load().is_err());
}
#[test]
fn import_initial_header_does_not_adopt_foreign_future_flat_payload() {
    for manifest in [false, true] {
        let fixture = Fixture::new();
        let target = if manifest {
            fixture.store.manifest_path()
        } else {
            fixture.store.results_path()
        };
        fs::write(fixture.fs.root().path().join(target.as_str()), b"").unwrap();
        assert!(
            fixture
                .store
                .save(&fixture.writer, &fixture.initial(), ExpectedState::Absent)
                .is_err()
        );
        assert!(fixture.store.load().unwrap().is_none());
    }
}
#[test]
fn import_manifest_requires_owned_header_and_installs_only_exact_frozen_bytes() {
    let fixture = Fixture::new();
    let source = fixture._temp.path().join("external manifest.jsonl");
    let bytes = b"frozen exact manifest blob\n";
    fs::write(&source, bytes).unwrap();
    let summary = ManifestPreparation {
        path: source.to_str().unwrap().into(),
        manifest_hash: Blake3Hash::digest(bytes),
        items: 1,
        input_bytes: 8,
        manifest_bytes: bytes.len() as u64,
        first_item_offset: 5,
        preview: false,
    };
    assert!(
        fixture
            .store
            .install_manifest(&fixture.writer, &source, &summary)
            .is_err()
    );
    let mut p = fixture.initial();
    p.manifest = StoredImportManifest {
        hash: summary.manifest_hash.clone(),
        items: 1,
        bytes: summary.manifest_bytes,
        first_item_offset: 5,
    };
    p.manifest_offset = 5;
    p.results_hash = fixture.store.initial_results_hash(&p.manifest).unwrap();
    fixture.write_progress(&p);
    fs::write(&source, b"drifted bytes").unwrap();
    assert!(
        fixture
            .store
            .install_manifest(&fixture.writer, &source, &summary)
            .is_err()
    );
    fs::write(&source, bytes).unwrap();
    fixture
        .store
        .install_manifest(&fixture.writer, &source, &summary)
        .unwrap();
    let target = fixture
        .fs
        .root()
        .path()
        .join(fixture.store.manifest_path().as_str());
    let modified = fs::metadata(&target).unwrap().modified().unwrap();
    fixture
        .store
        .install_manifest(&fixture.writer, &source, &summary)
        .unwrap();
    assert_eq!(fs::metadata(&target).unwrap().modified().unwrap(), modified);
    fs::remove_file(&source).unwrap();
    fixture
        .store
        .install_manifest(&fixture.writer, &source, &summary)
        .unwrap();
    assert_eq!(fs::read(&target).unwrap(), bytes);
    fs::write(&target, b"foreign target bytes").unwrap();
    assert!(
        fixture
            .store
            .install_manifest(&fixture.writer, &source, &summary)
            .is_err()
    );
    assert_eq!(fs::read(target).unwrap(), b"foreign target bytes");
}
#[test]
fn import_intent_content_address_vault_descriptor_and_corruption_checks() {
    let fixture = Fixture::new();
    fixture.write_progress(&fixture.pending());
    let intent = fixture.intent();
    let reference = fixture
        .store
        .retain_intent(&fixture.writer, &intent)
        .unwrap();
    assert_eq!(fixture.store.read_intent(&reference).unwrap(), intent);
    assert_eq!(
        fixture
            .store
            .retain_intent(&fixture.writer, &intent)
            .unwrap(),
        reference
    );
    let mut wrong = reference.clone();
    wrong.path = fixture.store.manifest_path();
    assert!(fixture.store.read_intent(&wrong).is_err());
    let mut foreign = intent.clone();
    foreign.proof.vault_id = id("vault_foreign");
    assert!(
        fixture
            .store
            .retain_intent(&fixture.writer, &foreign)
            .is_err()
    );
    foreign = intent.clone();
    foreign.proof.operation = Some(
        crate::changes::indexed_refresh::IndexedWriteOperation::SourceWithdraw {
            source_id: id("source_import_state"),
        },
    );
    assert!(
        fixture
            .store
            .retain_intent(&fixture.writer, &foreign)
            .is_err()
    );
    let target = fixture.fs.root().path().join(reference.path.as_str());
    fs::write(&target, b"foreign bytes").unwrap();
    assert!(fixture.store.read_intent(&reference).is_err());
    assert!(
        fixture
            .store
            .retain_intent(&fixture.writer, &intent)
            .is_err()
    );
    assert_eq!(fs::read(target).unwrap(), b"foreign bytes");
}
#[test]
fn import_result_chain_replays_one_unacknowledged_frame_and_rejects_duplicate_append() {
    let fixture = Fixture::new();
    let mut p = fixture.pending();
    let state_hash = fixture.write_progress(&p);
    let event = fixture.event(&p);
    let (offset, hash) = fixture
        .store
        .append_result(&fixture.writer, 0, &p.results_hash, event.clone())
        .unwrap();
    let tail = fixture.store.read_tail(0, &p.results_hash).unwrap();
    assert!(!tail.torn_tail);
    assert_eq!(tail.next_offset, offset);
    assert_eq!(tail.frame.unwrap().checksum, hash);
    assert!(
        fixture
            .store
            .append_result(&fixture.writer, 0, &p.results_hash, event.clone())
            .is_err()
    );
    let ImportResultEvent::GroupCommitted { result } = event else {
        unreachable!()
    };
    p.next_ordinal = 1;
    p.manifest_offset = 80;
    p.groups_committed = 1;
    p.last_group = Some(result);
    p.pending = None;
    p.completed = true;
    p.results_offset = offset;
    p.results_hash = hash;
    fixture
        .store
        .save(&fixture.writer, &p, ExpectedState::Hash(state_hash))
        .unwrap();
    assert!(
        fixture
            .store
            .read_tail(offset, &p.results_hash)
            .unwrap()
            .frame
            .is_none()
    );
}
#[test]
fn import_result_torn_tail_is_classified_and_only_checked_incomplete_bytes_truncate() {
    let fixture = Fixture::new();
    let mut p = fixture.pending();
    let state_hash = fixture.write_progress(&p);
    let event = fixture.event(&p);
    let (offset, hash) = fixture
        .store
        .append_result(&fixture.writer, 0, &p.results_hash, event.clone())
        .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(fixture.results())
        .unwrap()
        .write_all(b"{\"partial\"")
        .unwrap();
    let length = fs::metadata(fixture.results()).unwrap().len();
    let tail = fixture.store.read_tail(0, &p.results_hash).unwrap();
    assert!(tail.torn_tail && tail.frame.is_some());
    assert!(
        fixture
            .store
            .truncate_tail(&fixture.writer, 0, length)
            .is_err(),
        "complete unacknowledged frame is preserved"
    );
    let ImportResultEvent::GroupCommitted { result } = event else {
        unreachable!()
    };
    p.next_ordinal = 1;
    p.manifest_offset = 80;
    p.groups_committed = 1;
    p.last_group = Some(result);
    p.pending = None;
    p.completed = true;
    p.results_offset = offset;
    p.results_hash = hash;
    fixture
        .store
        .save(&fixture.writer, &p, ExpectedState::Hash(state_hash))
        .unwrap();
    assert!(
        fixture
            .store
            .truncate_tail(&fixture.writer, offset, length + 1)
            .is_err()
    );
    fixture
        .store
        .truncate_tail(&fixture.writer, offset, length)
        .unwrap();
    assert_eq!(fs::metadata(fixture.results()).unwrap().len(), offset);
}
#[test]
fn import_result_unknown_complete_frames_chain_or_oversize_tails_remain_untouched() {
    for variant in 0..3 {
        let fixture = Fixture::new();
        let p = fixture.pending();
        fixture.write_progress(&p);
        fixture
            .store
            .append_result(&fixture.writer, 0, &p.results_hash, fixture.event(&p))
            .unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(fixture.results())
            .unwrap();
        if variant == 0 {
            file.write_all(b"{}\n").unwrap();
        }
        if variant == 1 {
            file.write_all(&vec![b'x'; MAX_IMPORT_RESULT_LINE_BYTES + 1])
                .unwrap();
        }
        if variant == 2 {
            drop(file);
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(fixture.results()).unwrap()).unwrap();
            value["previous"] = serde_json::json!(Blake3Hash::digest(b"wrong previous"));
            let mut bytes = serde_json::to_vec(&value).unwrap();
            bytes.push(b'\n');
            fs::write(fixture.results(), bytes).unwrap();
        }
        let before = fs::read(fixture.results()).unwrap();
        assert!(fixture.store.read_tail(0, &p.results_hash).is_err());
        assert!(
            fixture
                .store
                .truncate_tail(&fixture.writer, 0, before.len() as u64)
                .is_err()
        );
        assert_eq!(fs::read(fixture.results()).unwrap(), before);
    }
}
#[cfg(unix)]
#[test]
fn import_private_file_hardlink_symlink_alias_directory_and_nested_vault_refuse() {
    for variant in 0..5 {
        let fixture = Fixture::new();
        let target = fixture.fs.root().path().join(fixture.store.state.as_str());
        let foreign = fixture._temp.path().join("foreign.bin");
        fs::write(&foreign, b"foreign exact bytes").unwrap();
        match variant {
            0 => fs::hard_link(&foreign, &target).unwrap(),
            1 => std::os::unix::fs::symlink(&foreign, &target).unwrap(),
            2 => fs::write(
                target.with_file_name(target.file_name().unwrap().to_str().unwrap().to_uppercase()),
                b"case alias",
            )
            .unwrap(),
            3 => fs::create_dir(&target).unwrap(),
            _ => {
                fs::write(
                    fixture.fs.root().path().join(format!("{ANCHOR}/WIKI.md")),
                    b"nested marker",
                )
                .unwrap();
            }
        }
        assert!(
            fixture
                .store
                .save(&fixture.writer, &fixture.initial(), ExpectedState::Absent)
                .is_err()
        );
        assert_eq!(fs::read(foreign).unwrap(), b"foreign exact bytes");
    }
}
#[cfg(unix)]
#[test]
fn import_private_creation_modes_are_restrictive_and_foreign_vault_is_rejected() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let p = fixture.pending();
    fixture.write_progress(&p);
    fixture
        .store
        .append_result(&fixture.writer, 0, &p.results_hash, fixture.event(&p))
        .unwrap();
    for target in [&fixture.store.state, &fixture.store.results] {
        assert_eq!(
            fs::metadata(fixture.fs.root().path().join(target.as_str()))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
    let foreign =
        ImportStore::new(fixture.fs.clone(), id("vault_foreign"), "owned import key").unwrap();
    assert!(foreign.load().is_err());
}

struct RecordingIo {
    calls: std::sync::Mutex<Vec<&'static str>>,
}
impl RecordingIo {
    fn record(&self, name: &'static str) {
        self.calls.lock().unwrap().push(name);
    }
}
impl DurableIo for RecordingIo {
    fn create_stage(&self, p: &Path) -> std::io::Result<File> {
        self.record("ordinary_stage");
        NativeIo.create_stage(p)
    }
    fn create_private_stage(&self, p: &Path) -> std::io::Result<File> {
        self.record("private_stage");
        NativeIo.create_private_stage(p)
    }
    fn create_private_directory(&self, p: &Path) -> std::io::Result<()> {
        self.record("private_directory");
        NativeIo.create_private_directory(p)
    }
    fn create_directory(&self, p: &Path) -> std::io::Result<()> {
        self.record("ordinary_directory");
        NativeIo.create_directory(p)
    }
    fn open_append(&self, p: &Path) -> std::io::Result<File> {
        self.record("append");
        NativeIo.open_append(p)
    }
    fn truncate_file(&self, f: &File, n: u64) -> std::io::Result<()> {
        self.record("truncate");
        NativeIo.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut File, b: &[u8]) -> std::io::Result<()> {
        self.record("write");
        NativeIo.write_stage(f, b)
    }
    fn sync_file(&self, f: &File) -> std::io::Result<()> {
        self.record("sync_file");
        NativeIo.sync_file(f)
    }
    fn replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        self.record("replace");
        NativeIo.replace(a, b)
    }
    fn remove(&self, p: &Path) -> std::io::Result<()> {
        self.record("remove");
        NativeIo.remove(p)
    }
    fn remove_directory(&self, p: &Path) -> std::io::Result<()> {
        self.record("remove_directory");
        NativeIo.remove_directory(p)
    }
    fn sync_directory(&self, p: &Path) -> std::io::Result<DirectorySync> {
        self.record("sync_directory");
        NativeIo.sync_directory(p)
    }
}
#[cfg(unix)]
#[test]
fn import_private_creation_proxy_forwards_each_operation_once_to_same_inner_adapter() {
    let temp = tempfile::tempdir().unwrap();
    let recorder = Arc::new(RecordingIo {
        calls: std::sync::Mutex::new(vec![]),
    });
    let inner: Arc<dyn DurableIo> = recorder.clone();
    let proxy = PrivateCreationIo {
        inner: inner.clone(),
    };
    assert!(Arc::ptr_eq(&proxy.inner, &inner));
    let directory = temp.path().join("private");
    proxy.create_directory(&directory).unwrap();
    let stage = directory.join("stage");
    let target = directory.join("target");
    let mut file = proxy.create_stage(&stage).unwrap();
    proxy.write_stage(&mut file, b"exact").unwrap();
    proxy.sync_file(&file).unwrap();
    proxy.sync_directory(&directory).unwrap();
    proxy.replace(&stage, &target).unwrap();
    drop(file);
    let mut file = proxy.open_append(&target).unwrap();
    proxy.write_stage(&mut file, b" suffix").unwrap();
    proxy.truncate_file(&file, 5).unwrap();
    proxy.sync_file(&file).unwrap();
    drop(file);
    assert_eq!(fs::read(&target).unwrap(), b"exact");
    proxy.remove(&target).unwrap();
    proxy.remove_directory(&directory).unwrap();
    assert_eq!(
        *recorder.calls.lock().unwrap(),
        vec![
            "private_directory",
            "private_stage",
            "write",
            "sync_file",
            "sync_directory",
            "replace",
            "append",
            "write",
            "truncate",
            "sync_file",
            "remove",
            "remove_directory"
        ]
    );
    assert_eq!(
        proxy.remove(&target).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(recorder.calls.lock().unwrap().last(), Some(&"remove"));
}

#[test]
fn import_closed_attempt_acknowledgment_can_keep_same_pending_identity() {
    let fixture = Fixture::new();
    let mut p = fixture.pending();
    let header = fixture.write_progress(&p);
    let pending = p.pending.clone().unwrap();
    let event = ImportResultEvent::AttemptClosed {
        group: pending.group,
        change: pending.change.clone(),
        prepared: None,
        reason: ImportAttemptCloseReason::StaleBase,
    };
    let (offset, hash) = fixture
        .store
        .append_result(&fixture.writer, 0, &p.results_hash, event)
        .unwrap();
    p.results_offset = offset;
    p.results_hash = hash;
    fixture
        .store
        .save(&fixture.writer, &p, ExpectedState::Hash(header))
        .unwrap();
    assert_eq!(
        fixture.store.load().unwrap().unwrap().0.pending,
        Some(pending)
    );
    assert!(
        fixture
            .store
            .read_tail(offset, &p.results_hash)
            .unwrap()
            .frame
            .is_none()
    );
}
#[test]
fn import_first_frame_torn_fragment_and_long_acknowledged_prefix_are_bounded() {
    let fixture = Fixture::new();
    let mut p = fixture.pending();
    let hash = fixture.write_progress(&p);
    fixture
        .store
        .install(&fixture.writer, &fixture.store.results, b"", 0)
        .unwrap();
    fs::write(fixture.results(), b"{\"partial").unwrap();
    let tail = fixture.store.read_tail(0, &p.results_hash).unwrap();
    assert!(tail.frame.is_none() && tail.torn_tail && tail.next_offset == 0);
    fixture.store.truncate_tail(&fixture.writer, 0, 9).unwrap();
    // The header acknowledges this prefix. Tail inspection seeks to its known
    // offset; it deliberately does not reconstruct or rehash acknowledged data.
    let length = MAX_IMPORT_RESULTS_BYTES - 1;
    std::fs::OpenOptions::new()
        .write(true)
        .open(fixture.results())
        .unwrap()
        .set_len(length)
        .unwrap();
    p.results_offset = length;
    p.results_hash = Blake3Hash::digest(b"acknowledged chain fixture");
    fixture
        .store
        .save(&fixture.writer, &p, ExpectedState::Hash(hash))
        .unwrap();
    let tail = fixture.store.read_tail(length, &p.results_hash).unwrap();
    assert!(tail.frame.is_none() && !tail.torn_tail && tail.next_offset == length);
}
