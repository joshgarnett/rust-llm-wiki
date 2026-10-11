//! Original Unix authority handle operations for targets without search-only
//! route descriptors. Selected by descriptor::supported(), never mixed into
//! the supported descriptor boundary.
use super::*;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
pub(super) fn parents(path: &Path) -> Result<Vec<(PathBuf, File)>> {
    let mut handles = Vec::new();
    for parent in path
        .ancestors()
        .skip(1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent)
            .map_err(|e| io("pin authority ancestor", e))?;
        handles.push((parent.to_path_buf(), file));
    }
    Ok(handles)
}
pub(super) fn open_file(path: &Path, writable: bool) -> Result<File> {
    fs::OpenOptions::new()
        .read(true)
        .write(writable)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| io("open operation authority", e))
}
pub(super) fn same_file(path: &Path, file: &File, directory: bool) -> Result<()> {
    let held = file
        .metadata()
        .map_err(|e| io("inspect held authority handle", e))?;
    let named = inspect(path).map_err(|e| io("inspect authority handle binding", e))?;
    if named.file_type().is_symlink()
        || held.dev() != named.dev()
        || held.ino() != named.ino()
        || if directory {
            !held.is_dir()
        } else {
            !held.is_file() || held.nlink() != 1
        }
    {
        return Err(recovery(
            "authority handle binding changed or has unsafe type",
        ));
    }
    Ok(())
}
