//! One persistent inode supplies OS writer authority; PID text is only diagnostic.
use super::fs::{DurableIo, NativeIo};
use super::paths::{VaultRoot, io_error};
use crate::domain::{ErrorCode, Result, VaultRelativePath, WikiError};
use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, Write},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct WriterPermit {
    file: File,
    root: VaultRoot,
}
impl WriterPermit {
    pub fn acquire(root: &VaultRoot, timeout: Duration) -> Result<Self> {
        // Bootstrap state without following any managed symlink. Never remove the lock file.
        for relative in [".wiki", ".wiki/state"] {
            let relative = VaultRelativePath::new(relative)?;
            root.validate_portable_paths(std::slice::from_ref(&relative))?;
            let path = root.resolve(&relative)?;
            match fs::create_dir(&path) {
                Ok(()) => {
                    NativeIo
                        .sync_directory(&path)
                        .map_err(|e| io_error("sync lock directory", e))?;
                    NativeIo
                        .sync_directory(path.parent().expect("managed directory has parent"))
                        .map_err(|e| io_error("sync lock directory parent", e))?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
                Err(e) => return Err(io_error("create lock directory", e)),
            }
        }
        let relative = VaultRelativePath::new(".wiki/state/writer.lock")?;
        root.validate_portable_paths(std::slice::from_ref(&relative))?;
        let path = root.resolve(&relative)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| io_error("open writer lock", e))?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    let remaining = timeout.saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        let mut error = WikiError::new(
                            ErrorCode::ContentConflict,
                            "writer lock contention timeout",
                        );
                        error.retryable = true;
                        return Err(error);
                    }
                    thread::sleep(remaining.min(Duration::from_millis(10)));
                }
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(io_error("acquire writer lock", e));
                }
            }
        }
        file.set_len(0)
            .map_err(|e| io_error("truncate lock diagnostic", e))?;
        file.rewind()
            .map_err(|e| io_error("seek lock diagnostic", e))?;
        writeln!(file, "pid={}", std::process::id())
            .map_err(|e| io_error("write lock diagnostic", e))?;
        file.sync_all()
            .map_err(|e| io_error("sync lock diagnostic", e))?;
        Ok(Self {
            file,
            root: root.clone(),
        })
    }
    pub fn root(&self) -> &VaultRoot {
        &self.root
    }
    pub(crate) fn require_root(&self, root: &VaultRoot) -> Result<()> {
        if &self.root == root {
            Ok(())
        } else {
            Err(WikiError::invalid(
                "writer permit belongs to another canonical vault",
            ))
        }
    }
}
impl Drop for WriterPermit {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}
