use super::*;
use std::fs;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temporary.path()).unwrap();
    (temporary, base)
}

#[test]
fn private_preview_is_write_free_and_publication_is_private_idempotent_and_redacted() {
    let (_temporary, base) = fixture();
    let path = base.join("new/private/credential.key");
    let snapshot = load_private_file(&path, 16384).unwrap();
    let secret = b"synthetic-secret-sentinel";
    let preview = write_private_file(&snapshot, secret, false, true, 5000).unwrap();
    assert!(preview.changed && !preview.applied && !preview.durable);
    assert!(!base.join("new").exists());
    let published = write_private_file(&snapshot, secret, false, false, 5000).unwrap();
    assert!(published.applied && published.durable && published.failure.is_none());
    assert!(!format!("{published:?}").contains("synthetic-secret-sentinel"));
    let loaded = load_private_file(&path, 16384).unwrap();
    assert_eq!(loaded.bytes(), Some(secret.as_slice()));
    assert!(!format!("{loaded:?}").contains("synthetic-secret-sentinel"));
    let retry = write_private_file(&loaded, secret, false, false, 5000).unwrap();
    assert!(!retry.changed && !retry.applied && retry.durable && retry.failure.is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[test]
fn private_changes_need_explicit_replacement_and_unchanged_snapshot() {
    let (_temporary, base) = fixture();
    let path = base.join("credential.key");
    let missing = load_private_file(&path, 16384).unwrap();
    write_private_file(&missing, b"first-key", false, false, 5000).unwrap();
    let first = load_private_file(&path, 16384).unwrap();
    for dry_run in [true, false] {
        let error = write_private_file(&first, b"next-key", false, dry_run, 5000).unwrap_err();
        assert_eq!(error.code, ErrorCode::ContentConflict);
        assert!(!format!("{error:?}").contains("first-key"));
        assert!(!format!("{error:?}").contains("next-key"));
        assert_eq!(fs::read(&path).unwrap(), b"first-key");
    }
    write_private_file(&first, b"next-key", true, false, 5000).unwrap();
    assert_eq!(
        write_private_file(&first, b"different-key", true, false, 5000)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    assert_eq!(fs::read(&path).unwrap(), b"next-key");
    assert_eq!(
        write_private_file(&missing, b"first-key", false, false, 5000)
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
}

#[test]
fn private_publication_report_exposes_post_publication_failure_without_bytes() {
    let (_temporary, base) = fixture();
    let path = base.join("credential.key");
    let snapshot = load_private_file(&path, 16384).unwrap();
    CHECKPOINT_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|stage, _| {
            if stage == "published" {
                Err(WikiError::new(
                    ErrorCode::Internal,
                    "injected final failure",
                ))
            } else {
                Ok(())
            }
        }))
    });
    let report = write_private_file(&snapshot, b"synthetic-secret", false, false, 5000).unwrap();
    CHECKPOINT_HOOK.with(|hook| *hook.borrow_mut() = None);
    assert!(report.applied && report.durable && report.failure.is_some());
    assert!(!format!("{report:?}").contains("synthetic-secret"));
    assert_eq!(fs::read(path).unwrap(), b"synthetic-secret");
}

#[test]
fn identical_private_retry_confirms_file_and_parent_sync_after_publication_failure() {
    let (_temporary, base) = fixture();
    let path = base.join("provider-or-key-file");
    let snapshot = load_private_file(&path, 16384).unwrap();
    local_io::SYNC_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|phase, _| {
            if phase == "publication_directory" {
                Err(WikiError::new(
                    ErrorCode::Internal,
                    "injected publication sync failure",
                ))
            } else {
                Ok(())
            }
        }))
    });
    let published = write_private_file(&snapshot, b"private-sentinel", false, false, 5000);
    local_io::SYNC_HOOK.with(|hook| *hook.borrow_mut() = None);
    let published = published.unwrap();
    assert!(published.applied && !published.durable && published.failure.is_some());
    let reload = load_private_file(&path, 16384).unwrap();
    let phases = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded = phases.clone();
    local_io::SYNC_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |phase, _| {
            recorded.borrow_mut().push(phase.to_string());
            Ok(())
        }))
    });
    let retried = write_private_file(&reload, b"private-sentinel", false, false, 5000);
    local_io::SYNC_HOOK.with(|hook| *hook.borrow_mut() = None);
    let retried = retried.unwrap();
    assert!(!retried.applied && !retried.changed && retried.durable && retried.failure.is_none());
    assert_eq!(*phases.borrow(), ["existing_file", "directory"]);
    assert_eq!(fs::read(&path).unwrap(), b"private-sentinel");
}

#[test]
fn identical_private_retry_retains_sync_failure_and_refuses_a_swapped_file() {
    let (_temporary, base) = fixture();
    let path = base.join("provider-or-key-file");
    let snapshot = load_private_file(&path, 16384).unwrap();
    write_private_file(&snapshot, b"private-sentinel", false, false, 5000).unwrap();
    let loaded = load_private_file(&path, 16384).unwrap();
    local_io::SYNC_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|phase, _| {
            if phase == "existing_file" {
                Err(WikiError::new(
                    ErrorCode::Internal,
                    "injected existing-file sync failure",
                ))
            } else {
                Ok(())
            }
        }))
    });
    let result = write_private_file(&loaded, b"private-sentinel", false, false, 5000);
    local_io::SYNC_HOOK.with(|hook| *hook.borrow_mut() = None);
    let result = result.unwrap();
    assert!(!result.applied && !result.durable && result.failure.is_some());
    local_io::SYNC_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |phase, path| {
            if phase == "existing_file" {
                let other = path.with_file_name("foreign-private-file");
                fs::write(&other, b"foreign-sentinel").unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&other, fs::Permissions::from_mode(0o600)).unwrap();
                }
                fs::rename(&other, path).unwrap();
            }
            Ok(())
        }))
    });
    let result = write_private_file(&loaded, b"private-sentinel", false, false, 5000);
    local_io::SYNC_HOOK.with(|hook| *hook.borrow_mut() = None);
    let result = result.unwrap();
    assert!(!result.applied && !result.durable && result.failure.is_some());
    assert_eq!(fs::read(&path).unwrap(), b"foreign-sentinel");
    assert!(!format!("{result:?}").contains("private-sentinel"));
}

#[test]
fn private_bounds_reject_before_creating_a_destination() {
    let (_temporary, base) = fixture();
    let path = base.join("new/key");
    assert!(load_private_file(&path, 0).is_err());
    assert!(load_private_file(&path, MAX_PRIVATE_SETUP_BYTES + 1).is_err());
    let snapshot = load_private_file(&path, 4).unwrap();
    assert!(write_private_file(&snapshot, b"", false, true, 5000).is_err());
    assert!(write_private_file(&snapshot, b"12345", false, false, 5000).is_err());
    assert!(write_private_file(&snapshot, b"1234", false, false, 30001).is_err());
    assert!(!base.join("new").exists());
}

#[cfg(unix)]
#[test]
fn private_files_refuse_symlinks_hardlinks_nonprivate_files_and_shared_parents() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (_temporary, base) = fixture();
    let target = base.join("original");
    fs::write(&target, b"original-secret").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    let alias = base.join("alias");
    symlink(&target, &alias).unwrap();
    assert!(load_private_file(&alias, 16384).is_err());
    let link = base.join("hardlink");
    fs::hard_link(&target, &link).unwrap();
    assert!(load_private_file(&target, 16384).is_err());
    fs::remove_file(link).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(load_private_file(&target, 16384).is_err());
    let destination = base.join("new-key");
    let snapshot = load_private_file(&destination, 16384).unwrap();
    fs::set_permissions(&base, fs::Permissions::from_mode(0o770)).unwrap();
    let result = write_private_file(&snapshot, b"new-secret", false, false, 5000);
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert!(!base.join("new-key.lock").exists());
    assert!(!destination.exists());
    assert_eq!(fs::read(target).unwrap(), b"original-secret");
}

#[cfg(unix)]
#[test]
fn private_swaps_after_staging_never_overwrite_a_foreign_destination() {
    use std::os::unix::fs::symlink;
    let (_temporary, base) = fixture();
    let target = base.join("author-file");
    fs::write(&target, b"author-bytes").unwrap();
    let path = base.join("credential.key");
    let snapshot = load_private_file(&path, 16384).unwrap();
    let target_for_hook = target.clone();
    CHECKPOINT_HOOK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |stage, path| {
            if stage == "staged" {
                symlink(&target_for_hook, path).unwrap();
            }
            Ok(())
        }))
    });
    let result = write_private_file(&snapshot, b"private-secret", false, false, 5000);
    CHECKPOINT_HOOK.with(|hook| *hook.borrow_mut() = None);
    assert!(result.is_err());
    assert_eq!(fs::read(target).unwrap(), b"author-bytes");
}
