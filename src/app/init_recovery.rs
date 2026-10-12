//! Bootstrap identity publication without adopting an interrupted, unmarked tree.
//!
//! New roots are assembled under a unique sibling name. Existing author folders
//! receive their complete marker first: all later state is ordinary lazy state.
//! No interrupted staging directory is adopted or recursively removed.
use crate::{
    domain::{ErrorCode, RecordId, Result, VaultRelativePath, WikiError},
    vault::{DirectorySync, DurableIo, NativeIo, VaultFs, VaultRoot, WriterPermit},
};
use std::{fs, fs::File, io::Read, path::Path, time::Duration};

fn io(action: &str, error: std::io::Error) -> WikiError {
    WikiError::new(ErrorCode::Internal, format!("{action}: {error}"))
}

/// Reestablish identity publication durability when setup selects a saved wiki.
/// This never creates a lock, directory, marker, or other retained state.
pub(crate) fn verify_init_durability(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Seek;
        let root = VaultRoot::explicit(path)?;
        crate::storage::layout::layout_kind(&root)?;
        VaultFs::new(root.clone()).require_storage_ready()?;
        let root_path = root.path();
        let parent_path = root_path.parent().unwrap_or(root_path);
        let marker_path = root_path.join("WIKI.md");
        let root_file = File::open(root_path).map_err(|e| io("open wiki folder for sync", e))?;
        let parent_file =
            File::open(parent_path).map_err(|e| io("open wiki parent for sync", e))?;
        let mut marker_file =
            File::open(&marker_path).map_err(|e| io("open wiki marker for sync", e))?;
        let verify_bindings = |marker: &File| -> Result<()> {
            same_directory(&root_file, root_path)?;
            same_directory(&parent_file, parent_path)?;
            same_file(marker, &marker_path)
        };
        verify_bindings(&marker_file)?;
        let read = |file: &mut File| -> Result<Vec<u8>> {
            file.rewind().map_err(|e| io("rewind wiki marker", e))?;
            let mut bytes = Vec::new();
            file.take(crate::app::MAX_INPUT_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| io("read wiki marker for sync", e))?;
            if bytes.len() > crate::app::MAX_INPUT_BYTES {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "wiki marker exceeds selection byte limit",
                ));
            }
            Ok(bytes)
        };
        let before = read(&mut marker_file)?;
        if !crate::records::parse_note(&before)
            .canonical
            .as_ref()
            .is_some_and(|record| record.kind() == crate::domain::RecordKind::Vault)
        {
            return Err(WikiError::invalid("WIKI.md must be a valid vault record"));
        }
        checkpoint("durability_opened", root_path)?;
        verify_bindings(&marker_file)?;
        marker_file
            .sync_all()
            .map_err(|e| io("sync existing wiki marker", e))?;
        root_file
            .sync_all()
            .map_err(|e| io("sync existing wiki folder", e))?;
        parent_file
            .sync_all()
            .map_err(|e| io("sync existing wiki parent", e))?;
        checkpoint("durability_synced", root_path)?;
        verify_bindings(&marker_file)?;
        if read(&mut marker_file)? != before {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "wiki marker changed while confirming initialization durability",
            ));
        }
        verify_bindings(&marker_file)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(WikiError::new(
            ErrorCode::Internal,
            "this platform cannot confirm wiki directory durability",
        ))
    }
}

#[cfg(unix)]
fn same_directory(file: &File, path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let held = file
        .metadata()
        .map_err(|e| io("inspect held wiki directory", e))?;
    let named = fs::symlink_metadata(path).map_err(|e| io("inspect wiki directory binding", e))?;
    if !held.is_dir()
        || !named.is_dir()
        || named.file_type().is_symlink()
        || held.dev() != named.dev()
        || held.ino() != named.ino()
    {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "wiki directory changed while confirming initialization durability",
        ));
    }
    Ok(())
}

pub(super) fn initialize(
    destination: &Path,
    id: &RecordId,
    marker: &[u8],
    existing: bool,
    directories: &[&str],
    timeout_ms: u64,
    resuming: bool,
) -> Result<()> {
    let parent = destination.parent().unwrap_or(destination);
    let working = if existing {
        destination.to_path_buf()
    } else {
        parent.join(format!(".lwiki-init-{}", id.as_str()))
    };
    let mut retained = Vec::new();
    let mut published = false;
    let mut durable = false;
    let mut state_started = false;
    let result = (|| {
        if !existing {
            NativeIo
                .create_private_directory(&working)
                .map_err(|e| io("create temporary wiki folder", e))?;
            retained.push(working.clone());
            checkpoint("root_created", &working)?;
        }
        let root = VaultRoot::for_initialization(&working)?;
        #[cfg(unix)]
        let held_root = File::open(&working).map_err(|e| io("hold initialization root", e))?;
        let (marker, authority) = if resuming {
            (marker.to_vec(), Vec::new())
        } else {
            crate::storage::source_layout::bootstrap_files(marker)?
        };
        let marker = marker.as_slice();
        let target = root.resolve_raw(&VaultRelativePath::new("WIKI.md")?)?;
        state_started = true;
        if !resuming {
            root.validate_portable_paths(&[
                VaultRelativePath::new("WIKI.md")?,
                VaultRelativePath::new(".wiki")?,
            ])?;
            stage_bootstrap_authority(&working, &authority, &mut retained)?;
            retained.push(working.join(".wiki"));
        }
        state_started = true;
        #[cfg(unix)]
        let held_authority =
            File::open(working.join(".wiki")).map_err(|e| io("hold bootstrap authority", e))?;
        checkpoint("bootstrap_published", &working)?;
        let stage = working.join(format!(".lwiki-marker-{}.tmp", uuid::Uuid::now_v7()));
        let mut file = NativeIo
            .create_private_stage(&stage)
            .map_err(|e| io("create wiki marker stage", e))?;
        retained.push(stage.clone());
        NativeIo
            .write_stage(&mut file, marker)
            .map_err(|e| io("write wiki marker", e))?;
        NativeIo
            .sync_file(&file)
            .map_err(|e| io("sync wiki marker", e))?;
        checkpoint("marker_staged", &working)?;
        same_file(&file, &stage)?;
        let reader = File::open(&stage).map_err(|e| io("open marker stage for verification", e))?;
        same_file(&reader, &stage)?;
        let mut staged_bytes = Vec::new();
        reader
            .take(marker.len() as u64 + 1)
            .read_to_end(&mut staged_bytes)
            .map_err(|e| io("verify marker stage bytes", e))?;
        if staged_bytes != marker {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "wiki marker stage bytes changed",
            ));
        }
        same_file(&file, &stage)?;
        // Complete ownership and exact marker bytes are checked again after
        // staging. The winner of .wiki publication alone chooses the marker.
        if crate::storage::source_layout::resume_bootstrap_marker(&root, None)?.as_deref()
            != Some(marker)
        {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "initialization authority changed before marker publication",
            ));
        }
        #[cfg(unix)]
        {
            same_directory(&held_root, &working)?;
            same_directory(&held_authority, &working.join(".wiki"))?;
        }
        sync_bootstrap_authority(&working)?;
        if crate::storage::source_layout::resume_bootstrap_marker(&root, None)?.as_deref()
            != Some(marker)
        {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "initialization authority changed while syncing",
            ));
        }
        // A hard link gives atomic absent-only publication on every supported
        // filesystem. An editor's concurrently created marker is never replaced.
        fs::hard_link(&stage, &target).map_err(|e| io("publish wiki marker", e))?;
        retained.push(target.clone());
        published = existing;
        checkpoint("marker_published", &working)?;
        let marker_sync = NativeIo
            .sync_directory(&working)
            .map_err(|e| io("sync published wiki marker", e))?;
        durable = existing && marker_sync == DirectorySync::Supported;
        crate::changes::journal::require_sync(marker_sync)?;
        checkpoint("marker_synced", &working)?;
        // Never unlink a substituted stage name. The held inode remains the
        // authority even after the marker adds a second link to that inode.
        same_file(&file, &stage)?;
        fs::remove_file(&stage).map_err(|e| io("remove published marker stage", e))?;
        let handle = VaultFs::new(root.clone());
        let writer = WriterPermit::acquire(&root, Duration::from_millis(timeout_ms))?;
        let knowledge = working.join("knowledge");
        NativeIo
            .create_directory(&knowledge)
            .map_err(|e| io("create knowledge folder", e))?;
        retained.push(knowledge.clone());
        NativeIo
            .sync_directory(&knowledge)
            .map_err(|e| io("sync knowledge folder", e))?;
        for directory in directories {
            let relative = VaultRelativePath::new(*directory)?;
            root.validate_portable_paths(std::slice::from_ref(&relative))?;
            let path = root.resolve(&relative)?;
            let parent = path
                .parent()
                .ok_or_else(|| WikiError::invalid("initialization directory parent missing"))?;
            if !parent.is_dir() {
                let relative_parent = parent
                    .strip_prefix(root.path())
                    .map_err(|_| WikiError::invalid("initialization directory escaped root"))?
                    .to_str()
                    .ok_or_else(|| WikiError::invalid("initialization parent is not UTF-8"))?;
                handle.ensure_directory(&VaultRelativePath::new(relative_parent)?, &writer)?;
            }
            NativeIo
                .create_directory(&path)
                .map_err(|e| io("create wiki folder", e))?;
            retained.push(path);
            handle.ensure_directory(&relative, &writer)?;
            checkpoint("managed_directory", &working)?;
        }
        drop(writer);
        checkpoint("layout_complete", &working)?;
        if !existing {
            // The destination can appear after preflight. Publication must fail
            // rather than replace even an empty directory belonging to someone else.
            let parent_root = VaultRoot::for_initialization(parent)?;
            let name = destination
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| WikiError::invalid("wiki folder name must be UTF-8"))?;
            parent_root.validate_portable_paths(&[VaultRelativePath::new(name)?])?;
            publish_directory(&working, destination)?;
            for path in &mut retained {
                *path = destination.join(path.strip_prefix(&working).expect("owned stage path"));
            }
            published = true;
            checkpoint("root_published", destination)?;
            let root_sync = NativeIo
                .sync_directory(parent)
                .map_err(|e| io("sync published wiki parent", e))?;
            durable = root_sync == DirectorySync::Supported;
            crate::changes::journal::require_sync(root_sync)?;
        }
        Ok(())
    })();
    result.map_err(|mut error: WikiError| {
        retained.retain(|path| fs::symlink_metadata(path).is_ok());
        // The writer may have created state before failing. Report it even if
        // no subsequent layout operation completed.
        let current = if published { destination } else { &working };
        for relative in [".wiki", ".wiki/state", ".wiki/state/writer.lock"] {
            let path = current.join(relative);
            if state_started && fs::symlink_metadata(&path).is_ok() && !retained.contains(&path) {
                retained.push(path);
            }
        }
        let cause = std::mem::take(&mut error.details);
        error.details = serde_json::json!({
            "cause_details": cause,
            "initialization": {
                "applied": !retained.is_empty(), "published": published,
                "durable": durable, "path": destination,
                "wiki_id": if published { Some(id.as_str()) } else { None },
                "retained_paths": retained,
            }
        });
        error.hint = Some(if published {
            "Your wiki marker was saved. Retry setup with the same wiki folder; ordinary wiki commands create any remaining working folders as needed.".into()
        } else {
            "Retry the same setup command. Any temporary paths listed in initialization.retained_paths were preserved; no existing folder was adopted or removed.".into()
        });
        error
    })
}

fn stage_bootstrap_authority(
    root: &Path,
    files: &[(VaultRelativePath, Vec<u8>)],
    retained: &mut Vec<std::path::PathBuf>,
) -> Result<()> {
    let stage = root.join(format!(".lwiki-state-{}", uuid::Uuid::now_v7()));
    NativeIo
        .create_private_directory(&stage)
        .map_err(|e| io("create bootstrap stage", e))?;
    retained.push(stage.clone());
    for directory in ["state", "state/storage", "state/storage/receipts"] {
        NativeIo
            .create_private_directory(&stage.join(directory))
            .map_err(|e| io("create bootstrap directory", e))?;
    }
    for (path, bytes) in files
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .chain(std::iter::once((
            ".wiki/.gitignore",
            b"cache/\n".as_slice(),
        )))
    {
        let path = stage.join(
            path.strip_prefix(".wiki/")
                .ok_or_else(|| WikiError::invalid("bootstrap path is not private state"))?,
        );
        let mut file = NativeIo
            .create_private_stage(&path)
            .map_err(|e| io("create bootstrap file", e))?;
        NativeIo
            .write_stage(&mut file, bytes)
            .map_err(|e| io("write bootstrap file", e))?;
        NativeIo
            .sync_file(&file)
            .map_err(|e| io("sync bootstrap file", e))?;
    }
    for directory in ["state/storage/receipts", "state/storage", "state", ""] {
        crate::changes::journal::require_sync(
            NativeIo
                .sync_directory(&stage.join(directory))
                .map_err(|e| io("sync bootstrap directory", e))?,
        )?;
    }
    checkpoint("bootstrap_staged", root)?;
    publish_directory(&stage, &root.join(".wiki"))?;
    checkpoint("bootstrap_installed", root)?;
    crate::changes::journal::require_sync(
        NativeIo
            .sync_directory(root)
            .map_err(|e| io("sync bootstrap publication", e))?,
    )?;
    Ok(())
}

fn sync_bootstrap_authority(root: &Path) -> Result<()> {
    // The fixed, fully validated bootstrap namespace is tiny. Sync it again on
    // retry, including a previous rename whose parent sync was interrupted.
    let mut directories = vec![root.join(".wiki")];
    let mut at = 0;
    let mut entries_seen = 0;
    while at < directories.len() {
        for entry in fs::read_dir(&directories[at]).map_err(|e| io("read bootstrap for sync", e))? {
            let entry = entry.map_err(|e| io("read bootstrap entry for sync", e))?;
            entries_seen += 1;
            if entries_seen > 16 {
                return Err(WikiError::invalid("bootstrap changed during sync"));
            }
            let kind = entry
                .file_type()
                .map_err(|e| io("inspect bootstrap for sync", e))?;
            if kind.is_dir() {
                directories.push(entry.path());
            } else if kind.is_file() {
                let file =
                    File::open(entry.path()).map_err(|e| io("open bootstrap for sync", e))?;
                same_file(&file, &entry.path())?;
                NativeIo
                    .sync_file(&file)
                    .map_err(|e| io("sync bootstrap authority", e))?;
                same_file(&file, &entry.path())?;
            } else {
                return Err(WikiError::invalid("bootstrap contains a nonregular entry"));
            }
        }
        at += 1;
        if directories.len() > 4 {
            return Err(WikiError::invalid(
                "bootstrap directory changed during sync",
            ));
        }
    }
    let root = root.to_path_buf();
    for directory in directories.iter().rev().chain(std::iter::once(&root)) {
        crate::changes::journal::require_sync(
            NativeIo
                .sync_directory(directory)
                .map_err(|e| io("sync bootstrap authority directory", e))?,
        )?;
    }
    Ok(())
}

fn same_file(file: &File, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let held = file
            .metadata()
            .map_err(|e| io("inspect marker stage handle", e))?;
        let named = fs::symlink_metadata(path).map_err(|e| io("inspect marker stage name", e))?;
        if !named.is_file()
            || named.file_type().is_symlink()
            || named.dev() != held.dev()
            || named.ino() != held.ino()
        {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "wiki marker stage was replaced",
            ));
        }
    }
    #[cfg(windows)]
    crate::vault::windows_security::validate_same_file(
        file,
        path,
        crate::vault::acl_policy::Protection::Private,
    )
    .map_err(|e| io("verify protected wiki marker stage", e))?;
    Ok(())
}

fn publish_directory(stage: &Path, destination: &Path) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let stage = CString::new(stage.as_os_str().as_bytes())
            .map_err(|_| WikiError::invalid("NUL in wiki path"))?;
        let destination = CString::new(destination.as_os_str().as_bytes())
            .map_err(|_| WikiError::invalid("NUL in wiki path"))?;
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                stage.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        let result =
            unsafe { libc::renamex_np(stage.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
        if result != 0 {
            return Err(io(
                "publish new wiki folder without replacing existing paths",
                std::io::Error::last_os_error(),
            ));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        fs::rename(stage, destination).map_err(|e| io("publish new wiki folder", e))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (stage, destination);
        Err(WikiError::new(
            ErrorCode::Internal,
            "atomic new wiki publication is unsupported on this platform",
        ))
    }
}

#[cfg(test)]
type Checkpoint = Box<dyn Fn(&str, &Path) -> Result<()>>;
#[cfg(test)]
thread_local! { static CHECKPOINT: std::cell::RefCell<Option<Checkpoint>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
pub(crate) fn with_checkpoint<T>(hook: Checkpoint, operation: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CHECKPOINT.with(|slot| *slot.borrow_mut() = None);
        }
    }
    CHECKPOINT.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(hook);
    });
    let _reset = Reset;
    operation()
}
fn checkpoint(name: &str, path: &Path) -> Result<()> {
    #[cfg(test)]
    CHECKPOINT.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(Ok(()), |hook| hook(name, path))
    })?;
    let _ = (name, path);
    Ok(())
}

#[cfg(test)]
#[path = "init_recovery_tests.rs"]
mod tests;
