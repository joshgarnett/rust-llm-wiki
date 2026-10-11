//! Bounded private setup writes from an inspected snapshot; no credential parsing.
use super::local_io;
use crate::domain::{ErrorCode, Result, WikiError};
use serde::Serialize;
use std::{
    fmt,
    path::{Path, PathBuf},
};

pub const MAX_PRIVATE_SETUP_BYTES: usize = 65_536;

#[derive(Clone)]
pub struct PrivateFileSnapshot {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
    max_bytes: usize,
    parents: local_io::ParentSnapshot,
}
impl fmt::Debug for PrivateFileSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrivateFileSnapshot")
            .field("path", &self.path)
            .field("present", &self.bytes.is_some())
            .field("max_bytes", &self.max_bytes)
            .finish()
    }
}
impl PrivateFileSnapshot {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn bytes(&self) -> Option<&[u8]> {
        self.bytes.as_deref()
    }
    pub fn present(&self) -> bool {
        self.bytes.is_some()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PrivateFileWriteReport {
    pub path: PathBuf,
    pub changed: bool,
    pub applied: bool,
    pub durable: bool,
    pub failure: Option<WikiError>,
}

/// Inspect without making directories, locks, stage files or resolving credentials.
pub fn load_private_file(path: &Path, max_bytes: usize) -> Result<PrivateFileSnapshot> {
    if max_bytes == 0 || max_bytes > MAX_PRIVATE_SETUP_BYTES {
        return Err(local_io::error(
            path,
            "inspect private setup destination",
            "byte limit must be 1..=65536",
        ));
    }
    let path = local_io::effective_path(path)?;
    let parents = local_io::ParentSnapshot::capture(&path)?;
    let bytes = read_private(&path, max_bytes)?;
    parents.verify(&path)?;
    Ok(PrivateFileSnapshot {
        path,
        bytes,
        max_bytes,
        parents,
    })
}
fn read_private(path: &Path, max_bytes: usize) -> Result<Option<Vec<u8>>> {
    #[cfg(unix)]
    {
        local_io::Directory::open(path, false)?
            .map(|directory| {
                directory.read_private(path.file_name().expect("validated name"), max_bytes)
            })
            .transpose()
            .map(Option::flatten)
    }
    #[cfg(not(unix))]
    {
        Err(local_io::unsupported(path))
    }
}
fn conflict(path: &Path, reason: &str) -> WikiError {
    WikiError::new(
        ErrorCode::ContentConflict,
        format!("private setup destination {}: {reason}", path.display()),
    )
}

/// Replace only an unchanged inspected private file after an explicit replacement choice.
/// An identical existing file is idempotent. Previews perform zero writes.
/// If publication succeeds but a later check fails, the report records both facts.
pub fn write_private_file(
    snapshot: &PrivateFileSnapshot,
    bytes: &[u8],
    replace_existing: bool,
    dry_run: bool,
    timeout_ms: u64,
) -> Result<PrivateFileWriteReport> {
    let path = snapshot.path();
    if bytes.is_empty() || bytes.len() > snapshot.max_bytes || timeout_ms > 30_000 {
        return Err(local_io::error(
            path,
            "validate private setup write",
            "nonempty bounded bytes and a timeout of at most 30000 ms required",
        ));
    }
    let changed = snapshot.bytes.as_deref() != Some(bytes);
    if changed && snapshot.present() && !replace_existing {
        return Err(conflict(
            path,
            "existing private file differs; an explicit replacement choice is required",
        ));
    }
    snapshot.parents.verify(path)?;
    if read_private(path, snapshot.max_bytes)? != snapshot.bytes {
        return Err(conflict(path, "file changed since setup inspection"));
    }
    #[cfg(unix)]
    if let Some(directory) = local_io::Directory::open(path, false)? {
        directory.verify_writable()?;
    }
    let mut report = PrivateFileWriteReport {
        path: path.to_path_buf(),
        changed,
        applied: false,
        durable: false,
        failure: None,
    };
    if dry_run {
        return Ok(report);
    }
    #[cfg(unix)]
    {
        snapshot.parents.verify(path)?;
        let directory = local_io::Directory::open(path, true)?.expect("created parent");
        let guard = directory.lock(timeout_ms)?;
        let check = || -> Result<()> {
            snapshot.parents.verify(path)?;
            directory.verify_lock(&guard.file, &guard.name)?;
            if directory.read_private(path.file_name().expect("name"), snapshot.max_bytes)?
                != snapshot.bytes
            {
                return Err(conflict(path, "file changed since setup inspection"));
            }
            Ok(())
        };
        check()?;
        if !changed {
            let result = directory
                .sync_existing(
                    path.file_name().expect("name"),
                    snapshot.max_bytes,
                    bytes,
                    true,
                )
                .and_then(|_| check());
            report.durable = result.is_ok();
            report.failure = result.err();
            return Ok(report);
        }
        let mut stage = directory.stage(bytes)?;
        #[cfg(test)]
        test_checkpoint("staged", path)?;
        check()?;
        let mut publication = local_io::PublicationState::default();
        let result = directory.publish_tracked(&mut stage, snapshot.present(), &mut publication);
        report.applied = publication.applied;
        report.durable = publication.durable;
        if let Err(error) = result {
            if !report.applied {
                return Err(error);
            }
            report.failure = Some(error);
        }
        #[cfg(test)]
        if report.failure.is_none()
            && let Err(error) = test_checkpoint("published", path)
        {
            report.failure = Some(error);
        }
        Ok(report)
    }
    #[cfg(not(unix))]
    {
        Err(local_io::unsupported(path))
    }
}

#[cfg(test)]
type CheckpointHook = Box<dyn FnMut(&str, &Path) -> Result<()>>;
#[cfg(test)]
std::thread_local! { static CHECKPOINT_HOOK: std::cell::RefCell<Option<CheckpointHook>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
fn test_checkpoint(stage: &str, path: &Path) -> Result<()> {
    CHECKPOINT_HOOK.with(|hook| {
        hook.borrow_mut()
            .as_mut()
            .map_or(Ok(()), |hook| hook(stage, path))
    })
}
#[cfg(test)]
#[path = "setup_io_tests.rs"]
mod tests;
