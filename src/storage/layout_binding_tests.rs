use super::*;
use crate::storage::{self, StorageOptions};
use std::{cell::RefCell, fs, time::Duration};

const MARKER: &[u8] = include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md");
const PENDING: &str = ".wiki/state/storage/cleanup.json";

fn rel(path: &str) -> VaultRelativePath {
    VaultRelativePath::new(path).unwrap()
}
fn fixture(migrated: bool) -> (tempfile::TempDir, VaultFs) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"), MARKER).unwrap();
    let vault = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    if migrated {
        let writer = WriterPermit::acquire(vault.root(), Duration::ZERO).unwrap();
        storage::cleanup(&vault, &writer, &StorageOptions::default()).unwrap();
    }
    (temp, vault)
}
fn activation(root: &VaultRoot) -> (Layout, VaultRelativePath) {
    let layout: Layout = decode(&raw_read(root, &rel(ACTIVE), 4096).unwrap().unwrap()).unwrap();
    let plan = rel(&format!(
        ".wiki/state/storage/receipts/{}.plan.json",
        layout.migration_id
    ));
    (layout, plan)
}
fn capture(root: &VaultRoot) -> Result<ValidatedLayout> {
    ValidatedLayout::capture_with_reader(root, &mut |path, max| raw_read(root, path, max))
}

#[test]
fn validated_binding_matches_real_layout_and_maps_without_more_reads() {
    for migrated in [false, true] {
        let (_temp, vault) = fixture(migrated);
        let root = vault.root();
        let calls = RefCell::new(Vec::new());
        let binding = ValidatedLayout::capture_with_reader(root, &mut |path, max| {
            calls.borrow_mut().push((path.clone(), max));
            raw_read(root, path, max)
        })
        .unwrap();
        assert_eq!(binding.retained(root).unwrap(), migrated);
        assert_eq!(active(root).unwrap(), migrated);
        let initial_calls = calls.borrow().clone();
        assert_eq!(initial_calls.len(), 3);
        assert_eq!(initial_calls[0], (rel(ACTIVE), 4096));
        assert_eq!(initial_calls[1], (rel("WIKI.md"), 1024 * 1024));
        if migrated {
            let (layout, plan) = activation(root);
            assert_eq!(initial_calls[2], (plan, 64 * 1024 * 1024));
            super::super::cleanup::verify_activation(root, &layout).unwrap();
        } else {
            assert_eq!(initial_calls[2], (rel("WIKI.md"), 1024 * 1024));
        }
        for name in [
            "changes",
            "changes/change/manifest.json",
            "knowledge/extractions/packets/p.md",
            "runs/run/events/e.json",
            "runs/run/checkpoints/c.json",
            "runs/run/run.md",
            "runs/run/research.md",
            "runs/run/outputs/data.json",
            "runs/run/outputs/report_x.md",
            "runs/run/report.md",
            "knowledge/pages/page.md",
            "WIKI.md",
        ] {
            let path = rel(name);
            assert_eq!(
                binding.map(root, &path).unwrap(),
                physical_relative(root, &path).unwrap()
            );
        }
        assert_eq!(*calls.borrow(), initial_calls);
        let (_other_temp, other) = fixture(false);
        assert!(binding.retained(other.root()).is_err());
        assert!(binding.map(other.root(), &rel("changes")).is_err());
    }
}

#[test]
fn activation_corruption_and_callback_observations_cannot_fall_back_to_disk() {
    let (_temp, vault) = fixture(true);
    let root = vault.root();
    let (layout, plan) = activation(root);
    for replacement in [None, Some(b"corrupt receipt".to_vec())] {
        let error = ValidatedLayout::capture_with_reader(root, &mut |path, max| {
            if path == &plan {
                Ok(replacement.clone())
            } else {
                raw_read(root, path, max)
            }
        })
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::RecoveryRequired);
    }
    let mut wrong = layout.clone();
    wrong.version = 1;
    let wrong_bytes = encode(&wrong).unwrap();
    let error = ValidatedLayout::capture_with_reader(root, &mut |path, max| {
        if path == &rel(ACTIVE) {
            Ok(Some(wrong_bytes.clone()))
        } else {
            raw_read(root, path, max)
        }
    })
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryRequired);
    let error = ValidatedLayout::capture_with_reader(root, &mut |path, max| {
        if path == &rel("WIKI.md") {
            Ok(Some(MARKER.to_vec()))
        } else {
            raw_read(root, path, max)
        }
    })
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryRequired);
    assert!(
        active(root).unwrap(),
        "callback failures did not modify the valid fixture"
    );
}

#[test]
fn pending_activation_uses_callback_for_every_marker_and_receipt_read() {
    let (temp, vault) = fixture(true);
    let root = vault.root();
    let (_, plan) = activation(root);
    fs::copy(temp.path().join(plan.as_str()), temp.path().join(PENDING)).unwrap();
    fs::remove_file(temp.path().join(ACTIVE)).unwrap();
    assert!(super::super::cleanup::pending_activation_matches(root).unwrap());
    assert!(!active(root).unwrap());
    let calls = RefCell::new(Vec::new());
    let binding = ValidatedLayout::capture_with_reader(root, &mut |path, max| {
        calls.borrow_mut().push(path.clone());
        raw_read(root, path, max)
    })
    .unwrap();
    assert!(!binding.retained(root).unwrap());
    assert_eq!(
        *calls.borrow(),
        vec![
            rel(ACTIVE),
            rel("WIKI.md"),
            rel("WIKI.md"),
            rel(PENDING),
            rel("WIKI.md"),
            plan.clone(),
            rel("WIKI.md")
        ]
    );
    let mut markers = 0;
    assert!(
        ValidatedLayout::capture_with_reader(root, &mut |path, max| {
            if path == &rel("WIKI.md") {
                markers += 1;
                if markers == 4 {
                    return Ok(None);
                }
            }
            raw_read(root, path, max)
        })
        .is_err(),
        "pending's final vault() read must use the supplied reader"
    );
    fs::remove_file(temp.path().join(plan.as_str())).unwrap();
    assert_eq!(capture(root).unwrap_err().code, ErrorCode::RecoveryRequired);
}

#[test]
fn missing_marker_initialization_preserves_safety_and_meters_directory_work() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".wiki/state")).unwrap();
    fs::create_dir_all(temp.path().join("changes")).unwrap();
    fs::create_dir_all(temp.path().join("runs")).unwrap();
    fs::write(temp.path().join(".wiki/state/writer.lock"), b"").unwrap();
    let root = VaultRoot::for_initialization(temp.path()).unwrap();
    let mut steps = 0;
    let binding = ValidatedLayout::capture_with_reader_budgeted(
        &root,
        &mut |path, max| raw_read(&root, path, max),
        &mut || {
            steps += 1;
            Ok(())
        },
    )
    .unwrap();
    assert!(!binding.retained(&root).unwrap());
    assert!(!active(&root).unwrap());
    assert!(
        steps > 5,
        "resolver components and directory entry work must be admitted"
    );
    let error = ValidatedLayout::capture_with_reader_budgeted(
        &root,
        &mut |path, max| raw_read(&root, path, max),
        &mut || {
            Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "directory budget exhausted",
            ))
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    fs::write(temp.path().join("changes/history"), b"history").unwrap();
    assert_eq!(
        capture(&root).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    fs::remove_file(temp.path().join("changes/history")).unwrap();
    fs::create_dir_all(temp.path().join(".wiki/retained")).unwrap();
    assert_eq!(
        capture(&root).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
}

#[test]
fn supplied_reader_bound_and_error_are_enforced() {
    let (_temp, vault) = fixture(false);
    let root = vault.root();
    let error =
        ValidatedLayout::capture_with_reader(root, &mut |_, max| Ok(Some(vec![0; max + 1])))
            .unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    let error = ValidatedLayout::capture_with_reader(root, &mut |_, _| {
        Err(WikiError::new(
            ErrorCode::ContentConflict,
            "observed authority changed",
        ))
    })
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert_eq!(error.message, "observed authority changed");
}
