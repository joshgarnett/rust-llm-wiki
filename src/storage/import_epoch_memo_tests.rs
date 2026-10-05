//! Native disposable fixtures; counters observe predicate work, not latency.
use super::*;
use std::{cell::Cell, fs, panic::AssertUnwindSafe};

std::thread_local! {
    static DECODED: Cell<usize> = const { Cell::new(0) };
    static REUSED: Cell<usize> = const { Cell::new(0) };
}
pub(super) fn decoded_epoch() {
    DECODED.with(|count| count.set(count.get() + 1));
}
pub(super) fn reused_epoch() {
    REUSED.with(|count| count.set(count.get() + 1));
}
fn counts() -> (usize, usize) {
    (DECODED.with(Cell::get), REUSED.with(Cell::get))
}
fn reset() {
    DECODED.with(|count| count.set(0));
    REUSED.with(|count| count.set(0));
}
fn no_scope() {
    assert!(IMPORT_EPOCH.with(|state| state.borrow().is_none()));
}
fn fixture() -> (tempfile::TempDir, VaultFs) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    let vault = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    {
        let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
        cleanup(&vault, &writer, &StorageOptions::default()).unwrap();
    }
    reset();
    (temp, vault)
}
fn activation(root: &VaultRoot) -> (layout::Layout, std::path::PathBuf, Vec<u8>) {
    let activation: layout::Layout =
        layout::decode(&fs::read(root.path().join(layout::ACTIVE)).unwrap()).unwrap();
    let path = root.path().join(format!(
        ".wiki/state/storage/receipts/{}.plan.json",
        activation.migration_id,
    ));
    let bytes = fs::read(&path).unwrap();
    (activation, path, bytes)
}
fn replace(root: &VaultRoot, activation: &layout::Layout, path: &std::path::Path, bytes: &[u8]) {
    let mut activation = activation.clone();
    activation.migration_hash = Blake3Hash::digest(bytes);
    fs::write(path, bytes).unwrap();
    fs::write(
        root.path().join(layout::ACTIVE),
        layout::encode(&activation).unwrap(),
    )
    .unwrap();
}

#[test]
fn import_epoch_memo_reuses_only_after_fresh_read_hash_and_marker_checks() {
    let (_temp, vault) = fixture();
    let root = vault.root();
    let (_activation, path, original) = activation(root);
    let marker = fs::read(root.path().join("WIKI.md")).unwrap();
    let scope = ImportEpochScope::begin(root, true);
    assert!(layout::active(root).unwrap());
    assert!(layout::active(root).unwrap());
    assert_eq!(counts(), (1, 1));

    let mut changed = original.clone();
    changed[0] ^= 1;
    fs::write(&path, changed).unwrap();
    assert_eq!(
        layout::active(root).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    assert_eq!(counts().0, 1);
    fs::write(&path, &original).unwrap();
    assert!(layout::active(root).unwrap());
    assert_eq!(counts().0, 2);

    fs::remove_file(&path).unwrap();
    assert_eq!(
        layout::active(root).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    fs::write(&path, &original).unwrap();
    assert!(layout::active(root).unwrap());
    assert_eq!(counts().0, 3);

    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_RECEIPT as u64 + 1)
        .unwrap();
    assert_eq!(
        layout::active(root).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
    assert_eq!(counts().0, 3);
    fs::write(&path, &original).unwrap();
    assert!(layout::active(root).unwrap());
    assert_eq!(counts().0, 4);

    fs::write(
        root.path().join("WIKI.md"),
        include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    assert_eq!(
        layout::active(root).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    fs::write(root.path().join("WIKI.md"), marker).unwrap();
    assert!(layout::active(root).unwrap());
    assert_eq!(counts().0, 5);
    drop(scope);
    no_scope();
    assert!(layout::active(root).unwrap());
    assert_eq!(counts().0, 6);
}

#[test]
fn import_epoch_memo_is_bound_to_canonical_root_even_for_identical_vault_bytes() {
    let (_temp, vault) = fixture();
    let root = vault.root();
    let (activation, _, bytes) = activation(root);
    let other_temp = tempfile::tempdir().unwrap();
    fs::copy(
        root.path().join("WIKI.md"),
        other_temp.path().join("WIKI.md"),
    )
    .unwrap();
    let other = VaultRoot::explicit(other_temp.path()).unwrap();
    let other_plan = other.path().join(format!(
        ".wiki/state/storage/receipts/{}.plan.json",
        activation.migration_id
    ));
    fs::create_dir_all(other_plan.parent().unwrap()).unwrap();
    fs::write(&other_plan, &bytes).unwrap();
    fs::write(
        other.path().join(layout::ACTIVE),
        layout::encode(&activation).unwrap(),
    )
    .unwrap();
    let _scope = ImportEpochScope::begin(root, true);
    assert!(layout::active(root).unwrap());
    assert!(layout::active(&other).unwrap());
    assert!(layout::active(&other).unwrap());
    assert!(layout::active(root).unwrap());
    assert_eq!(counts(), (3, 1));
    fs::write(other_plan, b"tampered").unwrap();
    assert!(layout::active(&other).is_err());
    assert!(layout::active(root).unwrap());
    assert_eq!(counts(), (3, 2));
}

#[test]
fn import_epoch_memo_activation_identity_and_changed_authenticated_plan_miss() {
    let (_temp, vault) = fixture();
    let root = vault.root();
    let (activation, path, bytes) = activation(root);
    let _scope = ImportEpochScope::begin(root, true);
    verify_activation(root, &activation).unwrap();
    let mut different_version = activation.clone();
    different_version.version += 1;
    // The semantic helper has always left the current Layout-version check to
    // active(); a different version must still miss its semantic memo.
    verify_activation(root, &different_version).unwrap();
    assert_eq!(counts().0, 2);
    fs::write(
        root.path().join(layout::ACTIVE),
        layout::encode(&different_version).unwrap(),
    )
    .unwrap();
    assert_eq!(
        layout::active(root).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    fs::write(
        root.path().join(layout::ACTIVE),
        layout::encode(&activation).unwrap(),
    )
    .unwrap();

    let mut wrong_vault = activation.clone();
    wrong_vault.vault_id = RecordId::new("other_vault").unwrap();
    assert!(verify_activation(root, &wrong_vault).is_err());
    let mut wrong_migration = activation.clone();
    wrong_migration.migration_id = RecordId::new("other_migration").unwrap();
    let other_plan = root
        .path()
        .join(".wiki/state/storage/receipts/other_migration.plan.json");
    fs::write(other_plan, &bytes).unwrap();
    assert!(verify_activation(root, &wrong_migration).is_err());
    assert_eq!(counts().0, 4);

    let mut epoch: Epoch = layout::decode(&bytes).unwrap();
    epoch.retain_undo_changes += 1;
    replace(root, &activation, &path, &layout::encode(&epoch).unwrap());
    assert!(layout::active(root).unwrap());
    assert!(layout::active(root).unwrap());
    assert_eq!(counts(), (5, 1));
}

#[test]
fn import_epoch_memo_failed_decode_checksum_epoch_or_bounds_are_never_inserted() {
    let (_temp, vault) = fixture();
    let root = vault.root();
    let (activation, path, bytes) = activation(root);
    let _scope = ImportEpochScope::begin(root, true);
    let mut envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    envelope["checksum"] = serde_json::to_value(Blake3Hash::digest(b"wrong")).unwrap();
    let bad_checksum = serde_json::to_vec(&envelope).unwrap();
    let mut bad_epoch: Epoch = layout::decode(&bytes).unwrap();
    bad_epoch.version += 1;
    let mut bad_bounds: Epoch = layout::decode(&bytes).unwrap();
    bad_bounds.copies.push(CopyAction {
        source: rel("one.bin").unwrap(),
        destination: rel("two.bin").unwrap(),
        hash: Blake3Hash::digest(b"unused"),
        bytes: u64::MAX,
    });
    let failures = [
        b"{".to_vec(),
        bad_checksum,
        layout::encode(&bad_epoch).unwrap(),
        layout::encode(&bad_bounds).unwrap(),
    ];
    for (index, bad) in failures.iter().enumerate() {
        replace(root, &activation, &path, bad);
        assert!(layout::active(root).is_err());
        assert!(layout::active(root).is_err());
        assert_eq!(counts(), ((index + 1) * 2, 0));
    }
    replace(root, &activation, &path, &bytes);
    assert!(layout::active(root).unwrap());
    assert!(layout::active(root).unwrap());
    assert_eq!(counts(), (9, 1));
}

#[test]
fn import_epoch_memo_custom_readers_fully_validate_and_allow_reentrant_scope_callbacks() {
    let (_temp, vault) = fixture();
    let root = vault.root();
    let (activation, _, _) = activation(root);
    let _scope = ImportEpochScope::begin(root, true);
    assert!(layout::active(root).unwrap());
    for _ in 0..2 {
        layout::ValidatedLayout::capture_with_reader(root, &mut |path, max| {
            layout::raw_read(root, path, max)
        })
        .unwrap();
        verify_activation_with_reader(&activation, &mut |path, max| {
            layout::raw_read(root, path, max)
        })
        .unwrap();
    }
    assert_eq!(counts(), (5, 0));
    let error = verify_activation_with_reader(&activation, &mut |_, _| {
        Err(WikiError::new(ErrorCode::ContentConflict, "callback error"))
    })
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert!(verify_activation_with_reader(&activation, &mut |_, _| Ok(None)).is_err());
    verify_activation_with_reader(&activation, &mut |path, max| {
        let _nested_disabled = ImportEpochScope::begin(root, false);
        assert!(layout::active(root)?);
        layout::raw_read(root, path, max)
    })
    .unwrap();
    assert_eq!(counts(), (7, 0));
    assert!(layout::active(root).unwrap());
    assert!(layout::active(root).unwrap());
    assert_eq!(counts(), (8, 1));
}

#[test]
fn import_epoch_memo_nested_disabled_scopes_and_stale_guards_do_not_share_authority() {
    let (_temp, vault) = fixture();
    let root = vault.root();
    let outer = ImportEpochScope::begin(root, true);
    assert!(layout::active(root).unwrap());
    {
        let _nested = ImportEpochScope::begin(root, true);
        assert!(layout::active(root).unwrap());
        assert!(layout::active(root).unwrap());
    }
    assert!(layout::active(root).unwrap());
    assert!(layout::active(root).unwrap());
    assert_eq!(counts(), (4, 1));
    {
        let _disabled = ImportEpochScope::begin(root, false);
        assert!(layout::active(root).unwrap());
        assert!(layout::active(root).unwrap());
    }
    assert!(layout::active(root).unwrap());
    assert_eq!(counts().0, 7);
    let stale = ImportEpochScope::begin(root, true);
    drop(outer);
    no_scope();
    let fresh = ImportEpochScope::begin(root, true);
    assert!(layout::active(root).unwrap());
    drop(stale);
    assert!(layout::active(root).unwrap());
    assert_eq!(counts(), (8, 2));
    drop(fresh);
    no_scope();
}

#[test]
fn import_epoch_memo_error_unwind_and_disabled_scope_teardown() {
    let (_temp, vault) = fixture();
    let root = vault.root();
    let error: Result<()> = (|| {
        let _scope = ImportEpochScope::begin(root, true);
        assert!(layout::active(root)?);
        Err(WikiError::invalid("operation error"))
    })();
    assert!(error.is_err());
    no_scope();
    assert!(
        std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _scope = ImportEpochScope::begin(root, true);
            assert!(layout::active(root).unwrap());
            panic!("operation unwind");
        }))
        .is_err()
    );
    no_scope();
    let _disabled = ImportEpochScope::begin(root, false);
    assert!(layout::active(root).unwrap());
    assert!(layout::active(root).unwrap());
    assert_eq!(counts(), (4, 0));
    no_scope();
}

#[test]
#[cfg(unix)]
fn import_epoch_memo_public_library_run_resume_own_and_release_scopes() {
    use crate::app::{OfflineApp, OperationOptions, offline::init, prepare_source_import};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("vault");
    let options = OperationOptions {
        offline: true,
        lock_timeout_ms: 200,
        ..Default::default()
    };
    init(&root, "Memo import fixture", options).unwrap();
    let fs = VaultFs::new(VaultRoot::explicit(&root).unwrap());
    {
        let writer = WriterPermit::acquire(fs.root(), Duration::ZERO).unwrap();
        cleanup(&fs, &writer, &StorageOptions::default()).unwrap();
    }
    let app = OfflineApp::new(fs, options).unwrap();
    app.index_rebuild_normalized().unwrap();
    let mut rows = Vec::new();
    for index in 0..2 {
        let path = temp.path().join(format!("input-{index}.txt"));
        fs::write(
            &path,
            format!("Memo capture {index}: verified source bytes.\n"),
        )
        .unwrap();
        rows.extend(serde_json::to_vec(&serde_json::json!({"path":path,"title":format!("Capture {index}"),"media_type":"text/plain"})).unwrap());
        rows.push(b'\n');
    }
    let list = temp.path().join("inputs.jsonl");
    let manifest = temp.path().join("manifest.jsonl");
    fs::write(&list, rows).unwrap();
    prepare_source_import(&list, &manifest, false).unwrap();
    reset();
    let first = app
        .source_import_run(&manifest, "memo direct library", 1, 1)
        .unwrap();
    assert_eq!(first.imported_items, 1);
    assert!(!first.completed);
    assert!(
        counts().1 > 0,
        "library run did not reuse its own predicate"
    );
    no_scope();
    let decoded = counts().0;
    assert!(layout::active(app.fs().root()).unwrap());
    assert_eq!(counts().0, decoded + 1);
    reset();
    let completed = app.source_import_resume("memo direct library", 1).unwrap();
    assert_eq!(completed.imported_items, 2);
    assert!(completed.completed);
    assert!(
        counts().1 > 0,
        "library resume did not reuse its own predicate"
    );
    no_scope();
}
