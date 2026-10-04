//! One private, never-reused audit file. Cleanup never enumerates or recursively deletes.
use crate::{
    domain::{ErrorCode, Result, WikiError},
    vault::{DurableIo, NativeIo},
};
use rusqlite::{Connection, OpenFlags};
use std::{
    fs::{self, File},
    path::PathBuf,
};

pub(crate) struct CheckScratch {
    directory: PathBuf,
    database: PathBuf,
    file: File,
    #[cfg(unix)]
    directory_handle: File,
    #[cfg(windows)]
    directory_handle: Option<crate::vault::windows_security::DirectoryGuard>,
    connection: Option<Connection>,
    cleaned: bool,
}
fn io(action: &str, error: impl std::fmt::Display) -> WikiError {
    WikiError::new(
        ErrorCode::Internal,
        format!("audit scratch {action}: {error}"),
    )
}
impl CheckScratch {
    pub(crate) fn create() -> Result<Self> {
        let parent = fs::canonicalize(std::env::temp_dir())
            .map_err(|e| io("resolve temporary directory", e))?;
        let directory = parent.join(format!("lwiki-check-{}", uuid::Uuid::now_v7().simple()));
        NativeIo
            .create_private_directory(&directory)
            .map_err(|e| io("create private directory", e))?;
        let database = directory.join("reference.sqlite");
        let result = (|| {
            #[cfg(unix)]
            let directory_handle = {
                use std::os::unix::fs::OpenOptionsExt;
                fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&directory)
                    .map_err(|e| io("pin directory", e))?
            };
            #[cfg(windows)]
            let directory_handle = crate::vault::windows_security::open_pinned_directory(
                &directory,
                crate::vault::acl_policy::Protection::Private,
            )
            .map_err(|e| io("pin private directory", e))?;
            let file = NativeIo
                .create_private_stage(&database)
                .map_err(|e| io("create file", e))?;
            let mut scratch = Self {
                directory: directory.clone(),
                database: database.clone(),
                file,
                #[cfg(unix)]
                directory_handle,
                #[cfg(windows)]
                directory_handle: Some(directory_handle),
                connection: None,
                cleaned: false,
            };
            let opened = (|| {
                scratch.verify()?;
                let connection = Connection::open_with_flags(
                    &database,
                    OpenFlags::SQLITE_OPEN_READ_WRITE
                        | OpenFlags::SQLITE_OPEN_NO_MUTEX
                        | OpenFlags::SQLITE_OPEN_NOFOLLOW,
                )
                .map_err(|e| io("open owned file", e))?;
                scratch.connection = Some(connection);
                scratch.verify()
            })();
            if let Err(mut error) = opened {
                let cleanup = scratch.cleanup();
                error.details = serde_json::json!({
                    "scratch_directory":directory,"scratch_cleaned":cleanup.is_ok(),
                    "cleanup_errors":cleanup.err().into_iter().collect::<Vec<_>>(),
                });
                return Err(error);
            }
            Ok(scratch)
        })();
        result.map_err(|mut error| {
            // Only this exact private create-new directory was allocated here.
            // An unexpected occupant makes remove_dir fail rather than being erased.
            let cleanup = fs::remove_dir(&directory);
            if let Err(cleanup) = cleanup
                && cleanup.kind() != std::io::ErrorKind::NotFound
            {
                error.details = serde_json::json!({"cause":error.details,
                    "scratch_directory":directory,"scratch_cleaned":false,
                    "cleanup_error":cleanup.to_string()});
            }
            error
        })
    }
    pub(crate) fn connection(&self) -> &Connection {
        self.connection
            .as_ref()
            .expect("audit scratch remains open")
    }
    pub(crate) fn verify(&self) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            for (path, file, directory) in [
                (&self.directory, &self.directory_handle, true),
                (&self.database, &self.file, false),
            ] {
                let named = fs::symlink_metadata(path).map_err(|e| io("inspect binding", e))?;
                let held = file.metadata().map_err(|e| io("inspect handle", e))?;
                if named.file_type().is_symlink()
                    || named.dev() != held.dev()
                    || named.ino() != held.ino()
                    || named.uid() != unsafe { libc::geteuid() }
                    || named.mode() & 0o077 != 0
                    || if directory {
                        !named.is_dir()
                    } else {
                        !named.is_file() || named.nlink() != 1
                    }
                {
                    return Err(io(
                        "verify private ownership",
                        "binding or protection changed",
                    ));
                }
            }
        }
        #[cfg(windows)]
        {
            self.directory_handle
                .as_ref()
                .expect("audit directory remains pinned")
                .verify_binding()
                .map_err(|e| io("verify directory", e))?;
            crate::vault::windows_security::validate_same_file(
                &self.file,
                &self.database,
                crate::vault::acl_policy::Protection::Private,
            )
            .map_err(|e| io("verify file", e))?;
        }
        #[cfg(not(any(unix, windows)))]
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "private audit scratch unavailable on this platform",
        ));
        Ok(())
    }
    pub(crate) fn logical_bytes(&self) -> Result<u64> {
        self.verify()?;
        Ok(self
            .file
            .metadata()
            .map_err(|e| io("inspect size", e))?
            .len())
    }
    pub(crate) fn cleanup(mut self) -> Result<()> {
        self.cleanup_inner().map_err(|mut error| {
            error.details =
                serde_json::json!({"scratch_directory":self.directory,"scratch_cleaned":false});
            error
        })
    }
    fn cleanup_inner(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        // Statements are owned by the caller's completed inner scope.
        if let Some(connection) = self.connection.take() {
            if let Err((connection, error)) = connection.close() {
                drop(connection);
                return Err(io("close SQLite", error));
            }
        }
        self.verify()?;
        fs::remove_file(&self.database).map_err(|e| io("remove owned file", e))?;
        self.cleaned = true;
        // Windows directory pins deny deletion while open. Release only after
        // verifying ownership and removing the exact pinned database file.
        #[cfg(windows)]
        drop(self.directory_handle.take());
        fs::remove_dir(&self.directory).map_err(|e| io("remove exact owned directory", e))?;
        Ok(())
    }
}
impl Drop for CheckScratch {
    fn drop(&mut self) {
        let _ = self.cleanup_inner();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audit_scratch_cleanup_removes_only_its_exact_files() {
        let scratch = CheckScratch::create().unwrap();
        let directory = scratch.directory.clone();
        scratch
            .connection()
            .execute_batch("PRAGMA journal_mode=OFF; CREATE TABLE probe(value);")
            .unwrap();
        assert!(scratch.logical_bytes().unwrap() > 0);
        scratch.cleanup().unwrap();
        assert!(!directory.exists());
    }
    #[test]
    fn audit_scratch_cleanup_reports_unexpected_occupant_and_preserves_it() {
        let scratch = CheckScratch::create().unwrap();
        let directory = scratch.directory.clone();
        let foreign = directory.join("unfamiliar");
        fs::write(&foreign, b"preserve").unwrap();
        let error = scratch.cleanup().unwrap_err();
        assert_eq!(error.details["scratch_cleaned"], false);
        assert_eq!(fs::read(&foreign).unwrap(), b"preserve");
        fs::remove_file(foreign).unwrap();
        fs::remove_dir(directory).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn audit_scratch_replaced_binding_is_never_deleted() {
        let scratch = CheckScratch::create().unwrap();
        let directory = scratch.directory.clone();
        let old = directory.join("original");
        fs::rename(&scratch.database, &old).unwrap();
        fs::write(&scratch.database, b"unfamiliar bytes").unwrap();
        let database = scratch.database.clone();
        let error = scratch.cleanup().unwrap_err();
        assert_eq!(error.details["scratch_cleaned"], false);
        assert_eq!(fs::read(&database).unwrap(), b"unfamiliar bytes");
        fs::remove_file(database).unwrap();
        fs::remove_file(old).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
