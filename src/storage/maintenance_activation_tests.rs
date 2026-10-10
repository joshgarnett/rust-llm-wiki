use super::*;
use crate::maintenance_parallel::{self, Job};
use std::{fs, time::Duration};

fn fixture(retained: bool) -> (tempfile::TempDir, VaultFs, WriterPermit) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let owner = WriterPermit::acquire(fs.root(), Duration::ZERO).unwrap();
    if retained {
        crate::storage::cleanup(&fs, &owner, &crate::storage::StorageOptions::default()).unwrap();
    }
    (temp, fs, owner)
}
fn observe(fs: &VaultFs) -> crate::domain::Result<bool> {
    let root = fs.root().clone();
    maintenance_parallel::run_batch(vec![Job::new(8 * 1024 * 1024, move || {
        layout::active(&root)
    })])?
    .into_iter()
    .next()
    .unwrap()
}

#[test]
#[cfg(unix)]
fn stable_proof_rereads_the_same_three_witnesses_on_every_activation() {
    let (_temp, fs, owner) = fixture(true);
    maintenance_parallel::command_scope(|| {
        let proof = prepare_parallel_activation(&owner, &fs)?.unwrap();
        crate::catalog::query_diagnostics::begin();
        assert!(layout::active(fs.root())?);
        let original = crate::catalog::query_diagnostics::end();
        crate::catalog::query_diagnostics::begin();
        assert!(observe(&fs)?);
        assert!(observe(&fs)?);
        let parallel = crate::catalog::query_diagnostics::end();
        let expected = original
            .reads
            .iter()
            .map(|read| (&read.path, read.bytes))
            .collect::<Vec<_>>();
        assert_eq!(expected.len(), 3);
        assert_eq!(
            parallel
                .reads
                .iter()
                .take(3)
                .map(|read| (&read.path, read.bytes))
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            parallel
                .reads
                .iter()
                .skip(3)
                .map(|read| (&read.path, read.bytes))
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(proof.witnesses.len(), 3);
        assert_eq!(
            maintenance_parallel::metrics().owner_activation_validations,
            1
        );
        assert_eq!(maintenance_parallel::metrics().owner_activation_reads, 3);
        assert_eq!(
            maintenance_parallel::metrics().owner_activation_read_bytes,
            proof.witnesses.iter().map(|w| w.len).sum::<u64>()
        );
        assert!(prepare_parallel_activation(&owner, &fs)?.is_some());
        assert_eq!(
            maintenance_parallel::metrics().owner_activation_validations,
            1
        );
        assert!(!owner_capture_active());
        Ok(())
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
fn changed_each_authority_and_restore_preserve_observed_failure() {
    for index in 0..3 {
        let (temp, fs, owner) = fixture(true);
        maintenance_parallel::command_scope(|| {
            let proof = prepare_parallel_activation(&owner, &fs)?.unwrap();
            let path = temp.path().join(proof.witnesses[index].path.as_str());
            let original = fs::read(&path).unwrap();
            let mut changed = original.clone();
            changed[0] ^= 1;
            let mtime = fs::metadata(&path).unwrap().modified().unwrap();
            fs::write(&path, changed).unwrap();
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(mtime))
                .unwrap();
            let failed = observe(&fs);
            fs::write(&path, original).unwrap();
            assert!(matches!(failed, Err(error) if error.code == ErrorCode::ContentConflict));
            assert_eq!(maintenance_parallel::metrics().completed, 1);
            assert_eq!(maintenance_parallel::metrics().failed, 1);
            assert!(observe(&fs)?);
            assert_eq!(maintenance_parallel::metrics().completed, 2);
            Ok(())
        })
        .unwrap();
    }
}

#[test]
#[cfg(unix)]
fn equivalent_marker_bytes_after_proof_conflict_but_before_proof_are_valid() {
    for before in [false, true] {
        let (temp, fs, owner) = fixture(true);
        maintenance_parallel::command_scope(|| {
            if !before {
                assert!(prepare_parallel_activation(&owner, &fs)?.is_some());
            }
            let path = temp.path().join("WIKI.md");
            let mut marker = fs::read(&path).unwrap();
            marker.extend_from_slice(b"\n");
            fs::write(path, marker).unwrap();
            assert!(layout::active(fs.root())?);
            if before {
                assert!(prepare_parallel_activation(&owner, &fs)?.is_some());
                assert!(observe(&fs)?);
            } else {
                assert!(
                    matches!(observe(&fs), Err(error) if error.code == ErrorCode::ContentConflict)
                );
            }
            Ok(())
        })
        .unwrap();
    }
}

#[test]
#[cfg(unix)]
fn wrong_root_unproved_worker_and_nested_proof_are_refused_without_decoding() {
    let (_temp, fs, owner) = fixture(true);
    let (_other, other, _owner) = fixture(true);
    maintenance_parallel::command_scope(|| {
        assert!(matches!(observe(&fs), Err(error) if error.code == ErrorCode::Internal));
        assert!(prepare_parallel_activation(&owner, &fs)?.is_some());
        assert!(matches!(observe(&other), Err(error) if error.code == ErrorCode::ContentConflict));
        let root = fs.root().clone();
        for result in maintenance_parallel::run_batch(vec![Job::new(8 * 1024 * 1024, move || {
            assert!(worker_context_active());
            assert!(matches!(enter_worker(None), Err(error) if error.code == ErrorCode::Internal));
            assert!(matches!(ValidatedLayout::capture_with_reader(&root, &mut |path, limit| layout::raw_read(&root, path, limit)), Err(error) if error.code == ErrorCode::Internal));
            layout::active(&root)
        })])? { assert!(result?); }
        assert!(!worker_context_active());
        Ok(())
    }).unwrap();
}

#[test]
fn legacy_import_and_recovery_select_owner_before_dispatch() {
    for retained in [false, true] {
        let (_temp, fs, owner) = fixture(retained);
        maintenance_parallel::command_scope(|| {
            if !retained {
                assert!(prepare_parallel_activation(&owner, &fs)?.is_none());
            } else if cfg!(unix) {
                assert!(prepare_parallel_activation(&owner, &fs)?.is_some());
            }
            let _import = crate::storage::ImportEpochScope::begin(fs.root(), true);
            assert!(prepare_parallel_activation(&owner, &fs)?.is_none());
            assert!(prepare_parallel_activation(&owner, &fs.for_storage_recovery())?.is_none());
            assert_eq!(maintenance_parallel::metrics().submitted, 0);
            Ok(())
        })
        .unwrap();
    }
}

#[test]
#[cfg(unix)]
fn panic_drops_worker_proof_before_next_job() {
    let (_temp, fs, owner) = fixture(true);
    maintenance_parallel::command_scope(|| {
        assert!(prepare_parallel_activation(&owner, &fs)?.is_some());
        let failed =
            maintenance_parallel::run_batch(vec![Job::<()>::new(8 * 1024 * 1024, || {
                assert!(worker_context_active());
                panic!("injected activation context panic")
            })])?;
        assert!(failed.into_iter().next().unwrap().is_err());
        assert!(observe(&fs)?);
        assert!(!worker_context_active());
        Ok(())
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
fn large_valid_receipt_is_owner_validated_and_hostile_replacement_only_hashed() {
    let (temp, fs, owner) = fixture(true);
    let layout_path = temp.path().join(layout::ACTIVE);
    let mut activation: Layout = layout::decode(&fs::read(&layout_path).unwrap()).unwrap();
    let receipt_path = temp.path().join(format!(
        ".wiki/state/storage/receipts/{}.plan.json",
        activation.migration_id
    ));
    let mut epoch: serde_json::Value = layout::decode(&fs::read(&receipt_path).unwrap()).unwrap();
    epoch["protected"] =
        serde_json::json!([{"path":"unclassified.bin", "reason":"x".repeat(1024 * 1024)}]);
    let bytes = layout::encode(&epoch).unwrap();
    activation.migration_hash = crate::domain::Blake3Hash::digest(&bytes);
    fs::write(&receipt_path, &bytes).unwrap();
    fs::write(&layout_path, layout::encode(&activation).unwrap()).unwrap();
    maintenance_parallel::command_scope(|| {
        let proof = prepare_parallel_activation(&owner, &fs)?.unwrap();
        assert!(proof.witnesses[2].len > 1024 * 1024);
        assert!(observe(&fs)?);
        fs::write(&receipt_path, vec![b'!'; bytes.len()]).unwrap();
        assert!(matches!(observe(&fs), Err(error) if error.code == ErrorCode::ContentConflict));
        assert_eq!(
            maintenance_parallel::metrics().owner_activation_validations,
            1
        );
        assert_eq!(maintenance_parallel::metrics().owner_activation_reads, 3);
        assert_eq!(WORKSPACE_BYTES, 72 * 1024);
        Ok(())
    })
    .unwrap();
}

#[test]
fn pending_cleanup_selects_owner_without_constructing_a_partial_proof() {
    let (temp, fs, owner) = fixture(true);
    fs::write(
        temp.path().join(".wiki/state/storage/cleanup.json"),
        b"interrupted cleanup",
    )
    .unwrap();
    maintenance_parallel::command_scope(|| {
        assert!(prepare_parallel_activation(&owner, &fs)?.is_none());
        assert_eq!(
            maintenance_parallel::metrics().owner_activation_validations,
            0
        );
        assert_eq!(maintenance_parallel::metrics().submitted, 0);
        Ok(())
    })
    .unwrap();
}
