use lwiki::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::{ChangeDraft, ChangeEngine, ExpectedWrite, PreparedChange},
    domain::{Blake3Hash, ErrorCode, VaultRelativePath},
    storage::{self, StorageOptions},
    vault::{
        ExpectedState, VaultFs, VaultRoot, WriterPermit,
        fs::{DirectorySync, DurableIo, NativeIo},
    },
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
fn rel(p: &str) -> VaultRelativePath {
    VaultRelativePath::new(p).unwrap()
}
fn fixture() -> (tempfile::TempDir, VaultFs) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        include_bytes!("fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    let vault = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    (temp, vault)
}
fn write_change(
    vault: &VaultFs,
    writer: &WriterPermit,
    target: &str,
    bytes: Vec<u8>,
) -> PreparedChange {
    let engine = ChangeEngine::new(vault.clone()).unwrap();
    let expected = vault
        .read_before(&rel(target))
        .unwrap()
        .map_or(ExpectedState::Absent, |v| ExpectedState::Hash(v.hash));
    let prepared = engine
        .prepare(
            writer,
            ChangeDraft {
                title: "storage fixture".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: BTreeMap::new(),
                read_preconditions: vec![],
                operations: vec![ExpectedWrite {
                    target: rel(target),
                    expected,
                    proposed: Some(bytes),
                    apply_after: vec![],
                }],
            },
        )
        .unwrap()
        .prepared;
    engine
        .apply(
            writer,
            &prepared,
            &CatalogGraphValidator,
            &Catalog::new(vault.clone(), engine.vault_id().clone()),
        )
        .unwrap();
    prepared
}
fn opts(retain: usize) -> StorageOptions {
    StorageOptions {
        retain_undo_changes: retain,
        ..Default::default()
    }
}
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let path = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &path);
        } else {
            fs::copy(entry.path(), path).unwrap();
        }
    }
}
#[test]
fn inventory_is_read_only_and_global_dedup_is_not_reclaimability() {
    let (_temp, vault) = fixture();
    fs::write(vault.root().path().join("one.bin"), b"identical").unwrap();
    fs::write(vault.root().path().join("two.bin"), b"identical").unwrap();
    let before = storage::inventory(&vault, &opts(1)).unwrap();
    assert!(before.complete);
    assert_eq!(before.totals.duplicate_bytes, 9);
    let plan = storage::plan_cleanup(&vault, &opts(1)).unwrap();
    assert!(plan.candidates.is_empty());
    assert!(!vault.root().path().join(".wiki").exists());
    let bounded = storage::inventory(
        &vault,
        &StorageOptions {
            max_files: 1,
            ..opts(1)
        },
    )
    .unwrap();
    assert!(!bounded.complete);
}
#[test]
fn compaction_reduces_bytes_retains_undo_and_full_restore() {
    let (_temp, vault) = fixture();
    let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
    let mut changes = Vec::new();
    for i in 0..8 {
        changes.push(write_change(
            &vault,
            &writer,
            "page.md",
            vec![b'a' + i; 128 * 1024],
        ));
    }
    fs::create_dir_all(vault.root().path().join("sources/archive")).unwrap();
    fs::write(
        vault.root().path().join("sources/archive/evidence.bin"),
        b"immutable evidence",
    )
    .unwrap();
    let before = storage::inventory(&vault, &opts(1)).unwrap();
    let result = storage::cleanup(&vault, &writer, &opts(1)).unwrap();
    assert_eq!(result.layout_version, 2);
    let before_payloads: Vec<_> = before
        .files
        .iter()
        .filter(|f| {
            f.path.as_str().starts_with("changes/")
                && (f.path.as_str().contains("/before/") || f.path.as_str().contains("/proposed/"))
        })
        .collect();
    assert_eq!(before_payloads.len(), 15);
    let after_inventory = storage::inventory(&vault, &opts(1)).unwrap();
    let objects: Vec<_> = after_inventory
        .files
        .iter()
        .filter(|f| f.path.as_str().starts_with(".wiki/retained/objects/"))
        .collect();
    assert_eq!(objects.len(), 2);
    assert_eq!(objects.iter().map(|f| f.bytes).sum::<u64>(), 256 * 1024);
    // Thirteen 128KiB copies disappear, while catalog/cache, current page and
    // exact manifest/outcome history remain. Allow <128KiB receipt overhead.
    assert!(
        before.totals.logical_bytes - result.after.logical_bytes >= 1536 * 1024,
        "before={} after={}",
        before.totals.logical_bytes,
        result.after.logical_bytes
    );
    assert!(result.deleted_files >= 15);
    assert!(!vault.root().path().join("changes").exists());
    assert_eq!(
        fs::read(vault.root().path().join("sources/archive/evidence.bin")).unwrap(),
        b"immutable evidence"
    );
    let engine = ChangeEngine::new(vault.clone()).unwrap();
    for change in &changes {
        engine.inspect_history(&change.change_id).unwrap();
    }
    assert!(engine.inverse_plan(&changes[0]).is_err());
    engine.inverse_plan(changes.last().unwrap()).unwrap();
    let stable = storage::inventory(&vault, &opts(1)).unwrap();
    let again = storage::cleanup(&vault, &writer, &opts(1)).unwrap();
    assert_eq!(again.deleted_files, 0);
    assert_eq!(
        storage::inventory(&vault, &opts(1)).unwrap().files,
        stable.files
    );
    let backup = tempfile::tempdir().unwrap();
    copy_tree(vault.root().path(), backup.path());
    let restored = VaultFs::new(VaultRoot::explicit(backup.path()).unwrap());
    let restored_engine = ChangeEngine::new(restored.clone()).unwrap();
    restored_engine
        .inverse_plan(changes.last().unwrap())
        .unwrap();
    for change in &changes {
        restored_engine.inspect_history(&change.change_id).unwrap();
    }
    assert_eq!(
        storage::inventory(&restored, &opts(1)).unwrap().totals,
        stable.totals
    );
    let next = write_change(&vault, &writer, "page.md", vec![b'z'; 128 * 1024]);
    let newest = engine.inspect(&next.change_id).unwrap();
    assert_eq!(newest.manifest.version, 2);
    let object = newest.manifest.operations[0]
        .after_payload
        .as_ref()
        .unwrap();
    fs::write(
        vault.root().path().join("page.md"),
        b"independent editor bytes",
    )
    .unwrap();
    assert_eq!(
        Blake3Hash::digest(fs::read(vault.root().resolve(&object.path).unwrap()).unwrap()),
        object.hash
    );
    assert!(
        !engine
            .missing_retained_payloads(&changes[0].change_id)
            .unwrap()
            .is_empty()
    );
    assert!(
        engine
            .missing_retained_payloads(&next.change_id)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn internal_packets_and_details_keep_logical_paths_and_reports_visible() {
    let (_temp, vault) = fixture();
    for (path, bytes) in [
        ("knowledge/extractions/packets/task.md", "packet bytes"),
        ("runs/run_fixture/run.md", "provider run detail"),
        ("runs/run_fixture/research.md", "research head detail"),
        ("runs/run_fixture/outputs/receipt.md", "detail"),
        ("runs/run_fixture/outputs/report_human.md", "human report"),
    ] {
        fs::create_dir_all(vault.root().path().join(path).parent().unwrap()).unwrap();
        fs::write(vault.root().path().join(path), bytes).unwrap();
    }
    let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
    storage::cleanup(&vault, &writer, &opts(1)).unwrap();
    assert!(
        !vault
            .root()
            .path()
            .join("knowledge/extractions/packets/task.md")
            .exists()
    );
    assert!(
        !vault
            .root()
            .path()
            .join("runs/run_fixture/outputs/receipt.md")
            .exists()
    );
    assert!(
        vault
            .root()
            .path()
            .join("runs/run_fixture/outputs/report_human.md")
            .exists()
    );
    assert_eq!(
        vault
            .read_before(&rel("knowledge/extractions/packets/task.md"))
            .unwrap()
            .unwrap()
            .bytes,
        b"packet bytes"
    );
    let scan = vault.root().scan_markdown().unwrap();
    assert!(scan.contains(&rel("knowledge/extractions/packets/task.md")));
    assert!(scan.contains(&rel("runs/run_fixture/outputs/receipt.md")));
    assert!(scan.contains(&rel("runs/run_fixture/run.md")));
    assert!(scan.contains(&rel("runs/run_fixture/research.md")));
    assert!(!vault.root().path().join("runs/run_fixture/run.md").exists());
    assert!(
        !vault
            .root()
            .path()
            .join("runs/run_fixture/research.md")
            .exists()
    );
}
struct CutIo {
    cut: AtomicBool,
    at_remove: bool,
}
impl DurableIo for CutIo {
    fn create_stage(&self, p: &Path) -> io::Result<File> {
        NativeIo.create_stage(p)
    }
    fn create_private_stage(&self, p: &Path) -> io::Result<File> {
        NativeIo.create_private_stage(p)
    }
    fn create_private_directory(&self, p: &Path) -> io::Result<()> {
        NativeIo.create_private_directory(p)
    }
    fn open_append(&self, p: &Path) -> io::Result<File> {
        NativeIo.open_append(p)
    }
    fn truncate_file(&self, f: &File, n: u64) -> io::Result<()> {
        NativeIo.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut File, b: &[u8]) -> io::Result<()> {
        NativeIo.write_stage(f, b)
    }
    fn sync_file(&self, f: &File) -> io::Result<()> {
        NativeIo.sync_file(f)
    }
    fn replace(&self, s: &Path, t: &Path) -> io::Result<()> {
        if !self.at_remove && t.ends_with("layout.json") && self.cut.swap(false, Ordering::SeqCst) {
            return Err(io::Error::other("fixture activation cut"));
        }
        NativeIo.replace(s, t)
    }
    fn remove(&self, p: &Path) -> io::Result<()> {
        if self.at_remove
            && p.to_string_lossy().contains("/changes/")
            && !p.file_name().unwrap().to_string_lossy().starts_with('.')
            && self.cut.swap(false, Ordering::SeqCst)
        {
            return Err(io::Error::other("fixture unlink cut"));
        }
        NativeIo.remove(p)
    }
    fn create_directory(&self, p: &Path) -> io::Result<()> {
        NativeIo.create_directory(p)
    }
    fn sync_directory(&self, p: &Path) -> io::Result<DirectorySync> {
        NativeIo.sync_directory(p)
    }
}
#[test]
fn interrupted_activation_and_unlink_resume_and_block_ordinary_writes() {
    for at_remove in [false, true] {
        let (_temp, vault) = fixture();
        let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
        let old = write_change(&vault, &writer, "page.md", vec![b'a'; 65536]);
        write_change(&vault, &writer, "page.md", vec![b'b'; 65536]);
        let faulty = VaultFs::with_io(
            vault.root().clone(),
            Arc::new(CutIo {
                cut: AtomicBool::new(true),
                at_remove,
            }),
        );
        assert!(storage::cleanup(&faulty, &writer, &opts(1)).is_err());
        assert!(
            vault
                .root()
                .path()
                .join(".wiki/state/storage/cleanup.json")
                .exists()
        );
        let error = vault
            .stage(&rel("foreign.md"), b"blocked", &writer)
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::RecoveryRequired);
        let result = storage::cleanup(&vault, &writer, &opts(1)).unwrap();
        assert!(result.resumed);
        assert!(
            !vault
                .root()
                .path()
                .join(".wiki/state/storage/cleanup.json")
                .exists()
        );
        ChangeEngine::new(vault.clone())
            .unwrap()
            .inspect_history(&old.change_id)
            .unwrap();
        assert_eq!(
            vault.read_before(&rel("page.md")).unwrap().unwrap().hash,
            Blake3Hash::digest(vec![b'b'; 65536])
        );
        assert_eq!(
            storage::cleanup(&vault, &writer, &opts(1))
                .unwrap()
                .deleted_files,
            0
        );
    }
}

#[test]
fn edited_cleanup_epoch_cannot_delete_source_bytes_even_with_fresh_checksum() {
    let (_temp, vault) = fixture();
    let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
    write_change(&vault, &writer, "page.md", vec![b'a'; 65536]);
    write_change(&vault, &writer, "page.md", vec![b'b'; 65536]);
    fs::create_dir_all(vault.root().path().join("sources/retained")).unwrap();
    let evidence = "sources/retained/original.bin";
    fs::write(vault.root().path().join(evidence), b"citation evidence").unwrap();
    let source = storage::inventory(&vault, &opts(1))
        .unwrap()
        .files
        .into_iter()
        .find(|f| f.path.as_str() == evidence)
        .unwrap();
    let faulty = VaultFs::with_io(
        vault.root().clone(),
        Arc::new(CutIo {
            cut: AtomicBool::new(true),
            at_remove: true,
        }),
    );
    assert!(storage::cleanup(&faulty, &writer, &opts(1)).is_err());
    let pending = vault.root().path().join(".wiki/state/storage/cleanup.json");
    let original = fs::read(&pending).unwrap();
    let mut envelope: serde_json::Value = serde_json::from_slice(&original).unwrap();
    let id = envelope["payload"]["id"].as_str().unwrap().to_owned();
    envelope["payload"]["deletes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(source).unwrap());
    envelope["checksum"] = serde_json::to_value(Blake3Hash::digest(
        serde_json::to_vec(&envelope["payload"]).unwrap(),
    ))
    .unwrap();
    let changed = serde_json::to_vec(&envelope).unwrap();
    fs::write(&pending, &changed).unwrap();
    // Editing both mutable intent and immutable archive still cannot create
    // authority over an unrelated source; cleanup re-derives reachability.
    let archive = vault
        .root()
        .path()
        .join(format!(".wiki/state/storage/receipts/{id}.plan.json"));
    fs::write(&archive, &changed).unwrap();
    assert!(storage::cleanup(&vault, &writer, &opts(1)).is_err());
    assert_eq!(
        fs::read(vault.root().path().join(evidence)).unwrap(),
        b"citation evidence"
    );
    fs::write(pending, &original).unwrap();
    fs::write(archive, &original).unwrap();
    storage::cleanup(&vault, &writer, &opts(1)).unwrap();
}

#[test]
fn preview_plan_survives_writer_acquisition_but_not_substantive_edits() {
    let (_temp, vault) = fixture();
    let plan = storage::plan_cleanup(&vault, &opts(1)).unwrap();
    let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
    storage::cleanup(
        &vault,
        &writer,
        &StorageOptions {
            expected_plan: Some(plan.plan_hash),
            ..opts(1)
        },
    )
    .unwrap();
    drop(writer);
    let plan = storage::plan_cleanup(&vault, &opts(1)).unwrap();
    fs::write(vault.root().path().join("author.md"), b"new author content").unwrap();
    let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
    let error = storage::cleanup(
        &vault,
        &writer,
        &StorageOptions {
            expected_plan: Some(plan.plan_hash),
            ..opts(1)
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert_eq!(
        fs::read(vault.root().path().join("author.md")).unwrap(),
        b"new author content"
    );
}

#[test]
fn edited_cleanup_epoch_cannot_generate_canonical_bytes_or_rewrite_marker() {
    for marker in [false, true] {
        let (_temp, vault) = fixture();
        let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
        write_change(&vault, &writer, "page.md", vec![b'a'; 65536]);
        let faulty = VaultFs::with_io(
            vault.root().clone(),
            Arc::new(CutIo {
                cut: AtomicBool::new(true),
                at_remove: false,
            }),
        );
        assert!(storage::cleanup(&faulty, &writer, &opts(1)).is_err());
        let pending = vault.root().path().join(".wiki/state/storage/cleanup.json");
        let original = fs::read(&pending).unwrap();
        let original_marker = fs::read(vault.root().path().join("WIKI.md")).unwrap();
        let mut envelope: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let id = envelope["payload"]["id"].as_str().unwrap().to_owned();
        if marker {
            let mut bytes: Vec<u8> =
                serde_json::from_value(envelope["payload"]["marker_after"].clone()).unwrap();
            bytes.extend_from_slice(b"\nunauthorized marker body\n");
            envelope["payload"]["marker_after"] = serde_json::to_value(bytes).unwrap();
        } else {
            envelope["payload"]["generated"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({"path":"injected.md","bytes":b"unauthorized".to_vec()}));
        }
        envelope["checksum"] = serde_json::to_value(Blake3Hash::digest(
            serde_json::to_vec(&envelope["payload"]).unwrap(),
        ))
        .unwrap();
        let changed = serde_json::to_vec(&envelope).unwrap();
        fs::write(&pending, &changed).unwrap();
        let archive = vault
            .root()
            .path()
            .join(format!(".wiki/state/storage/receipts/{id}.plan.json"));
        fs::write(&archive, &changed).unwrap();
        assert!(storage::cleanup(&vault, &writer, &opts(1)).is_err());
        assert!(!vault.root().path().join("injected.md").exists());
        assert_eq!(
            fs::read(vault.root().path().join("WIKI.md")).unwrap(),
            original_marker
        );
        fs::write(pending, &original).unwrap();
        fs::write(archive, &original).unwrap();
        storage::cleanup(&vault, &writer, &opts(1)).unwrap();
    }
}

#[test]
fn increased_undo_window_uses_completed_expiry_proof_without_resurrecting_bytes() {
    let (_temp, vault) = fixture();
    let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
    let mut changes = Vec::new();
    for &value in b"abcd" {
        changes.push(write_change(&vault, &writer, "page.md", vec![value; 65536]));
    }
    storage::cleanup(&vault, &writer, &opts(1)).unwrap();
    let engine = ChangeEngine::new(vault.clone()).unwrap();
    assert!(
        !engine
            .missing_retained_payloads(&changes[0].change_id)
            .unwrap()
            .is_empty()
    );
    let preview = storage::plan_cleanup(&vault, &opts(20)).unwrap();
    assert!(
        preview
            .protected
            .iter()
            .any(|p| p.reason.contains("previously_expired"))
    );
    let before = storage::inventory(&vault, &opts(20)).unwrap();
    let result = storage::cleanup(&vault, &writer, &opts(20)).unwrap();
    assert_eq!(result.deleted_files, 0);
    assert!(!result.warnings.is_empty());
    assert_eq!(
        storage::inventory(&vault, &opts(20)).unwrap().files,
        before.files
    );
    assert!(engine.inverse_plan(&changes[0]).is_err());
    engine.inverse_plan(changes.last().unwrap()).unwrap();

    let latest = engine.inspect(&changes.last().unwrap().change_id).unwrap();
    let payload = latest.manifest.operations[0]
        .before_payload
        .as_ref()
        .unwrap();
    let object = vault.root().path().join(format!(
        ".wiki/retained/objects/blake3/{}",
        payload.hash.hex()
    ));
    let bytes = fs::read(&object).unwrap();
    fs::remove_file(&object).unwrap();
    assert!(
        storage::cleanup(&vault, &writer, &opts(20)).is_err(),
        "unknown missing protected bytes must not inherit an unrelated expiry"
    );
    fs::write(&object, b"corrupt existing object").unwrap();
    assert!(
        storage::cleanup(&vault, &writer, &opts(20)).is_err(),
        "expiry does not excuse present corrupt bytes"
    );
    fs::write(object, bytes).unwrap();
    storage::cleanup(&vault, &writer, &opts(20)).unwrap();
}

#[test]
fn pending_or_mismatched_completion_cannot_prove_expired_undo() {
    let (_temp, vault) = fixture();
    let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
    for &value in b"abc" {
        write_change(&vault, &writer, "page.md", vec![value; 65536]);
    }
    storage::cleanup(&vault, &writer, &opts(1)).unwrap();
    let plan = fs::read_dir(vault.root().path().join(".wiki/state/storage/receipts"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(".plan.json"))
        .unwrap();
    let receipt = plan.with_file_name(
        plan.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .replace(".plan.json", ".json"),
    );
    let original = fs::read(&receipt).unwrap();
    fs::remove_file(&receipt).unwrap();
    assert!(storage::cleanup(&vault, &writer, &opts(20)).is_err());
    fs::write(&receipt, &original).unwrap();
    let mut envelope: serde_json::Value = serde_json::from_slice(&original).unwrap();
    envelope["payload"]["plan_hash"] =
        serde_json::to_value(Blake3Hash::digest(b"different exact plan")).unwrap();
    envelope["checksum"] = serde_json::to_value(Blake3Hash::digest(
        serde_json::to_vec(&envelope["payload"]).unwrap(),
    ))
    .unwrap();
    fs::write(&receipt, serde_json::to_vec(&envelope).unwrap()).unwrap();
    assert!(storage::cleanup(&vault, &writer, &opts(20)).is_err());
    fs::write(receipt, original).unwrap();
    storage::cleanup(&vault, &writer, &opts(20)).unwrap();
}

#[test]
fn format_two_requires_full_backup_activation_and_exact_initial_archive() {
    let (_temp, vault) = fixture();
    let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
    storage::cleanup(&vault, &writer, &opts(1)).unwrap();
    let knowledge_only = tempfile::tempdir().unwrap();
    fs::copy(
        vault.root().path().join("WIKI.md"),
        knowledge_only.path().join("WIKI.md"),
    )
    .unwrap();
    let restored = VaultFs::new(VaultRoot::explicit(knowledge_only.path()).unwrap());
    assert_eq!(
        storage::inventory(&restored, &opts(1)).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    let active = vault.root().path().join(".wiki/state/storage/layout.json");
    let activation: serde_json::Value =
        serde_json::from_slice(&fs::read(&active).unwrap()).unwrap();
    let id = activation["payload"]["migration_id"].as_str().unwrap();
    let archive = vault
        .root()
        .path()
        .join(format!(".wiki/state/storage/receipts/{id}.plan.json"));
    let bytes = fs::read(&archive).unwrap();
    fs::remove_file(&archive).unwrap();
    assert!(storage::inventory(&vault, &opts(1)).is_err());
    fs::write(&archive, b"altered archive").unwrap();
    assert!(storage::inventory(&vault, &opts(1)).is_err());
    fs::write(archive, bytes).unwrap();
    storage::inventory(&vault, &opts(1)).unwrap();
}

#[test]
fn malformed_epoch_maximum_lengths_refuse_before_io_without_overflow() {
    for field in ["copies", "deletes"] {
        let (_temp, vault) = fixture();
        let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
        write_change(&vault, &writer, "page.md", vec![b'a'; 65536]);
        let faulty = VaultFs::with_io(
            vault.root().clone(),
            Arc::new(CutIo {
                cut: AtomicBool::new(true),
                at_remove: false,
            }),
        );
        assert!(storage::cleanup(&faulty, &writer, &opts(1)).is_err());
        let pending = vault.root().path().join(".wiki/state/storage/cleanup.json");
        let original = fs::read(&pending).unwrap();
        let marker = fs::read(vault.root().path().join("WIKI.md")).unwrap();
        let mut envelope: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let id = envelope["payload"]["id"].as_str().unwrap().to_owned();
        envelope["payload"][field][0]["bytes"] = serde_json::Value::from(u64::MAX);
        envelope["checksum"] = serde_json::to_value(Blake3Hash::digest(
            serde_json::to_vec(&envelope["payload"]).unwrap(),
        ))
        .unwrap();
        let changed = serde_json::to_vec(&envelope).unwrap();
        let archive = vault
            .root()
            .path()
            .join(format!(".wiki/state/storage/receipts/{id}.plan.json"));
        fs::write(&pending, &changed).unwrap();
        fs::write(&archive, &changed).unwrap();
        assert_eq!(
            storage::cleanup(&vault, &writer, &opts(1))
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(fs::read(&pending).unwrap(), changed);
        assert_eq!(
            fs::read(vault.root().path().join("WIKI.md")).unwrap(),
            marker
        );
        assert_eq!(
            fs::read(vault.root().path().join("page.md")).unwrap(),
            vec![b'a'; 65536]
        );
        fs::write(pending, &original).unwrap();
        fs::write(archive, &original).unwrap();
        storage::cleanup(&vault, &writer, &opts(1)).unwrap();
    }
}

#[test]
fn initialization_allows_empty_managed_directories_but_not_missing_marker_history() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("new-vault");
    lwiki::app::offline::init(
        &target,
        "Storage initialization",
        lwiki::app::OperationOptions::default(),
    )
    .unwrap();
    let vault = VaultFs::new(VaultRoot::explicit(&target).unwrap());
    assert_eq!(
        storage::inventory(&vault, &opts(1)).unwrap().layout_version,
        1
    );
    let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
    write_change(&vault, &writer, "page.md", b"retained history".to_vec());
    fs::remove_file(target.join("WIKI.md")).unwrap();
    assert_eq!(
        vault.root().resolve(&rel("changes")).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
}

#[test]
fn cleanup_preserves_portable_logical_siblings_without_normalizing_run_ids() {
    let (_temp, vault) = fixture();
    for path in [
        "knowledge/extractions/packets/task.md",
        "runs/run_Case/outputs/receipt.md",
    ] {
        let physical = vault.root().path().join(path);
        fs::create_dir_all(physical.parent().unwrap()).unwrap();
        fs::write(physical, b"retained fixture").unwrap();
    }
    let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
    storage::cleanup(&vault, &writer, &opts(1)).unwrap();
    assert!(
        !vault
            .root()
            .path()
            .join("knowledge/extractions/packets")
            .exists()
    );
    assert!(
        !vault
            .root()
            .path()
            .join("runs/run_Case/outputs/receipt.md")
            .exists()
    );
    let before = storage::inventory(&vault, &opts(1)).unwrap().files;
    let engine = ChangeEngine::new(vault.clone()).unwrap();
    let draft = |path| ChangeDraft {
        title: "logical portability".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: vec![ExpectedWrite {
            target: rel(path),
            expected: ExpectedState::Absent,
            proposed: Some(b"new bytes".to_vec()),
            apply_after: vec![],
        }],
    };
    for path in [
        "knowledge/extractions/Packets/other.md",
        "knowledge/extractions/packets/Task.md",
        "runs/run_Case/Outputs/other.md",
        "runs/run_Case/outputs/Receipt.md",
        "runs/run_case/outputs/other.md",
    ] {
        let error = engine.plan(&draft(path)).unwrap_err();
        assert!(error.message.contains("case-folded"), "{path}: {error:?}");
    }
    // A differently spelled, noncolliding ID remains an exact valid identity.
    engine.plan(&draft("runs/run_Other/notes.md")).unwrap();
    assert_eq!(storage::inventory(&vault, &opts(1)).unwrap().files, before);
}
