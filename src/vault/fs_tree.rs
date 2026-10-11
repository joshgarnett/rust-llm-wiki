//! Bounded, authenticated complete Revision installation. Namespace races retain
//! the existing guarded-filesystem limitation; rename itself never clobbers.
use super::*;
use crate::domain::RecordId;

const MAX_BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
fn identity(path: &Path, directory: bool) -> Result<Identity> {
    let m = fs::symlink_metadata(path).map_err(|e| io_error("inspect revision identity", e))?;
    identity_metadata(&m, directory)
}
fn identity_metadata(m: &fs::Metadata, directory: bool) -> Result<Identity> {
    if m.file_type().is_symlink() || if directory { !m.is_dir() } else { !m.is_file() } {
        return Err(conflict("revision tree entry type changed"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !directory && m.nlink() != 1 {
            return Err(conflict("revision member has multiple links"));
        }
        Ok(Identity {
            device: m.dev(),
            inode: m.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "revision tree identity unavailable",
        ))
    }
}
struct Member {
    name: String,
    hash: Blake3Hash,
    length: usize,
    identity: Identity,
}
pub(crate) struct StagedRevisionTree {
    vault: VaultFs,
    target: VaultRelativePath,
    path: PathBuf,
    members: Vec<Member>,
    chain: Vec<(PathBuf, Identity)>,
    root_identity: Identity,
    installed: bool,
}
fn membership<'a>(
    target: &VaultRelativePath,
    paths: impl Iterator<Item = &'a VaultRelativePath>,
) -> Result<Vec<String>> {
    let parts: Vec<_> = target.as_str().split('/').collect();
    if parts.len() != 4 || parts[0] != "sources" || parts[2] != "revisions" {
        return Err(WikiError::invalid(
            "tree target must be canonical Revision root",
        ));
    }
    RecordId::new(parts[1])?;
    RecordId::new(parts[3])?;
    let prefix = format!("{}/", target.as_str());
    let mut names = Vec::with_capacity(3);
    for path in paths {
        if names.len() == 3 {
            return Err(WikiError::invalid(
                "Revision tree requires exactly three members",
            ));
        }
        let name = path
            .as_str()
            .strip_prefix(&prefix)
            .ok_or_else(|| WikiError::invalid("member escapes Revision root"))?;
        if !["revision.md", "original.bin", "content.md"].contains(&name)
            || names.iter().any(|n| n == name)
        {
            return Err(WikiError::invalid(
                "Revision tree requires exact flat capture membership",
            ));
        }
        names.push(name.to_owned());
    }
    if names.len() != 3 {
        return Err(WikiError::invalid(
            "Revision tree requires exactly three members",
        ));
    }
    Ok(names)
}
fn capture_chain(root: &VaultRoot, path: &Path) -> Result<Vec<(PathBuf, Identity)>> {
    let relative = path
        .strip_prefix(root.path())
        .map_err(|_| WikiError::invalid("tree escapes vault"))?;
    let mut current = root.path().to_path_buf();
    let mut result = vec![(current.clone(), identity(&current, true)?)];
    for part in relative.components() {
        current.push(part);
        result.push((current.clone(), identity(&current, true)?));
    }
    Ok(result)
}
fn check_chain(chain: &[(PathBuf, Identity)]) -> Result<()> {
    for (path, expected) in chain {
        if identity(path, true)? != *expected {
            return Err(conflict("Revision parent identity changed"));
        }
    }
    Ok(())
}
fn absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(conflict("Revision destination is occupied")),
        Err(e) => Err(io_error("inspect Revision destination", e)),
    }
}
fn authenticate(
    vault: &VaultFs,
    path: &Path,
    root_identity: Identity,
    members: &[Member],
    sync: bool,
) -> Result<()> {
    if identity(path, true)? != root_identity {
        return Err(conflict("Revision root identity changed"));
    }
    let mut count = 0;
    for entry in fs::read_dir(path).map_err(|e| io_error("enumerate Revision membership", e))? {
        let entry = entry.map_err(|e| io_error("inspect Revision member", e))?;
        count += 1;
        if count > 3
            || !members
                .iter()
                .any(|m| entry.file_name() == std::ffi::OsStr::new(&m.name))
        {
            return Err(conflict("Revision tree contains unfamiliar entries"));
        }
    }
    if count != 3 {
        return Err(conflict("Revision membership is incomplete"));
    }
    for member in members {
        authenticate_member(vault, path, root_identity, member, sync)?;
    }
    if identity(path, true)? != root_identity {
        return Err(conflict("Revision root changed during authentication"));
    }
    Ok(())
}
// This proof works after earlier owned members have been removed: it does not
// infer or require complete membership. The immediate check-to-unlink race remains.
fn authenticate_member(
    vault: &VaultFs,
    path: &Path,
    root_identity: Identity,
    member: &Member,
    sync: bool,
) -> Result<()> {
    if identity(path, true)? != root_identity {
        return Err(conflict("Revision root identity changed"));
    }
    let file_path = path.join(&member.name);
    if identity(&file_path, false)? != member.identity {
        return Err(conflict("Revision member identity changed"));
    }
    let relative = VaultRelativePath::new(
        file_path
            .strip_prefix(vault.root.path())
            .unwrap()
            .to_str()
            .ok_or_else(|| WikiError::invalid("tree path UTF-8 invalid"))?,
    )?;
    let (length, hash) =
        hash_regular_raw_bounded(&vault.root, &relative, member.length, "revision-tree-read")
            .map_err(|e| {
                if e.code == ErrorCode::BudgetExceeded {
                    conflict("Revision member grew after commitment")
                } else {
                    e
                }
            })?
            .ok_or_else(|| conflict("Revision member disappeared"))?;
    if length != member.length as u64
        || blake3::Hash::from(hash).to_hex().as_str() != member.hash.hex()
    {
        return Err(conflict("Revision member bytes changed"));
    }
    if sync {
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&file_path)
        };
        #[cfg(not(unix))]
        let file = File::open(&file_path);
        let file = file.map_err(|e| io_error("open Revision member sync", e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let held = file
                .metadata()
                .map_err(|e| io_error("inspect synced member", e))?;
            if !held.is_file()
                || held.nlink() != 1
                || held.dev() != member.identity.device
                || held.ino() != member.identity.inode
            {
                return Err(conflict("synced member identity changed"));
            }
        }
        vault
            .io
            .sync_file(&file)
            .map_err(|e| io_error("sync Revision member", e))?;
    }
    if identity(&file_path, false)? != member.identity {
        return Err(conflict("Revision member changed during authentication"));
    }
    if identity(path, true)? != root_identity {
        return Err(conflict(
            "Revision root changed during member authentication",
        ));
    }
    Ok(())
}
// Exact flat membership bounds the chain to six captured paths. Reserve 128
// full-path units (including growth slack) for chain copies, joined paths,
// validation temporaries and descriptors, plus the 64KiB read buffer, <=8KiB
// hash state and 16KiB fixed bookkeeping. Checked arithmetic refuses overflow.
fn workspace_bound(root_capacity: usize, target_capacity: usize) -> Option<usize> {
    root_capacity
        .checked_add(target_capacity)?
        .checked_add(256)?
        .checked_mul(128)?
        .checked_add(88 * 1024)
}
fn sync_dirs(vault: &VaultFs, dirs: &[&Path]) -> Result<DirectorySync> {
    for path in dirs {
        if vault
            .io
            .sync_directory(path)
            .map_err(|e| io_error("sync Revision directory", e))?
            != DirectorySync::Supported
        {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "Revision directory durability unavailable",
            ));
        }
    }
    Ok(DirectorySync::Supported)
}
impl Drop for StagedRevisionTree {
    fn drop(&mut self) {
        if self.installed
            || check_chain(&self.chain).is_err()
            || authenticate(
                &self.vault,
                &self.path,
                self.root_identity,
                &self.members,
                false,
            )
            .is_err()
        {
            return;
        }
        // Never recursive-delete: every entry has just matched the owned proof.
        for member in &self.members {
            if check_chain(&self.chain).is_err()
                || identity(&self.path, true).ok() != Some(self.root_identity)
                || authenticate_member(&self.vault, &self.path, self.root_identity, member, false)
                    .is_err()
            {
                return;
            }
            if check_chain(&self.chain).is_err() {
                return;
            }
            if self.vault.io.remove(&self.path.join(&member.name)).is_err() {
                return;
            }
        }
        if identity(&self.path, true).ok() == Some(self.root_identity) {
            let _ = self.vault.io.remove_directory(&self.path);
        }
    }
}
impl VaultFs {
    pub(crate) fn supports_revision_tree_install(&self) -> bool {
        self.io.supports_directory_install()
    }
    fn require_tree_capability(&self) -> Result<()> {
        if !self.supports_revision_tree_install() {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "complete Revision installation unavailable",
            ));
        }
        Ok(())
    }
    /// Pure bounded accounting, callable before retaining manifest3. Tuple
    /// capacities cover each full logical path and its owned payload buffer.
    pub(crate) fn admit_revision_tree_input(
        &self,
        target: &VaultRelativePath,
        members: impl Iterator<Item = (usize, usize)>,
    ) -> Result<()> {
        let reject = || {
            WikiError::new(
                ErrorCode::BudgetExceeded,
                "Revision aggregate input and workspace exceed allowance",
            )
        };
        let mut total = workspace_bound(self.root.owned_capacity(), target.owned_capacity())
            .ok_or_else(reject)?;
        let mut count = 0;
        for (path_capacity, payload_capacity) in members {
            if count == 3 {
                return Err(reject());
            }
            count += 1;
            total = total
                .checked_add(path_capacity)
                .and_then(|n| n.checked_add(payload_capacity))
                .ok_or_else(reject)?;
            if total > MAX_BYTES {
                return Err(reject());
            }
        }
        if count != 3 || total > MAX_BYTES {
            return Err(reject());
        }
        Ok(())
    }
    pub(crate) fn stage_revision_tree(
        &self,
        target: &VaultRelativePath,
        members: &[(VaultRelativePath, Vec<u8>)],
        permit: &WriterPermit,
    ) -> Result<StagedRevisionTree> {
        permit.require_root(&self.root)?;
        self.require_tree_capability()?;
        // The identical pure admission also runs before manifest3 retention.
        self.admit_revision_tree_input(
            target,
            members
                .iter()
                .map(|(p, b)| (p.owned_capacity(), b.capacity())),
        )?;
        self.require_storage_ready()?;
        let names = membership(target, members.iter().map(|(p, _)| p))?;
        self.validate_paths(std::slice::from_ref(target))?;
        let destination = self.root.resolve(target)?;
        absent(&destination)?;
        let parent = destination.parent().unwrap();
        let parent_relative = VaultRelativePath::new(
            parent
                .strip_prefix(self.root.path())
                .unwrap()
                .to_str()
                .unwrap(),
        )?;
        if self.ensure_directory(&parent_relative, permit)? != DirectorySync::Supported {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "Revision parent durability unavailable",
            ));
        }
        let container_relative =
            VaultRelativePath::new(format!("{}/.wiki", parent_relative.as_str()))?;
        self.root
            .validate_portable_paths(std::slice::from_ref(&container_relative))?;
        let container = self.root.resolve_raw(&container_relative)?;
        match self.io.create_private_directory(&container) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                identity(&container, true)?;
            }
            Err(e) => return Err(io_error("create Revision stage container", e)),
        }
        sync_dirs(self, &[&container, parent])?;
        let chain = capture_chain(&self.root, &container)?;
        let path = container.join(format!("tree-{}", uuid::Uuid::now_v7()));
        self.io
            .create_private_directory(&path)
            .map_err(|e| io_error("create unique Revision stage", e))?;
        let root_identity = identity(&path, true)?;
        if root_identity.device != chain.last().unwrap().1.device
            || root_identity.device != identity(parent, true)?.device
        {
            return Err(conflict("Revision rename crosses filesystems"));
        }
        let mut staged = StagedRevisionTree {
            vault: self.clone(),
            target: target.clone(),
            path,
            members: Vec::with_capacity(3),
            chain,
            root_identity,
            installed: false,
        };
        for ((_, bytes), name) in members.iter().zip(names) {
            check_chain(&staged.chain)?;
            if identity(&staged.path, true)? != staged.root_identity {
                return Err(conflict(
                    "Revision stage root changed before member creation",
                ));
            }
            let member_path = staged.path.join(&name);
            let mut file = self
                .io
                .create_private_stage(&member_path)
                .map_err(|e| io_error("create Revision staged member", e))?;
            #[cfg(unix)]
            let created_identity = {
                use std::os::unix::fs::MetadataExt;
                let held = file
                    .metadata()
                    .map_err(|e| io_error("inspect created Revision member", e))?;
                if !held.is_file() || held.nlink() != 1 {
                    return Err(conflict(
                        "created Revision member must be single-link regular file",
                    ));
                }
                Identity {
                    device: held.dev(),
                    inode: held.ino(),
                }
            };
            #[cfg(not(unix))]
            let created_identity = identity(&member_path, false)?;
            if identity(&member_path, false)? != created_identity {
                return Err(conflict("created Revision member pathname changed"));
            }
            self.io
                .write_stage(&mut file, bytes)
                .map_err(|e| io_error("write Revision staged member", e))?;
            self.io
                .sync_file(&file)
                .map_err(|e| io_error("sync Revision staged member", e))?;
            check_chain(&staged.chain)?;
            if identity(&staged.path, true)? != staged.root_identity {
                return Err(conflict("Revision stage root changed during member write"));
            }
            if identity(&member_path, false)? != created_identity {
                return Err(conflict("written Revision member pathname changed"));
            }
            staged.members.push(Member {
                name,
                hash: Blake3Hash::digest(bytes),
                length: bytes.len(),
                identity: created_identity,
            });
        }
        authenticate(self, &staged.path, root_identity, &staged.members, false)?;
        sync_dirs(self, &[&staged.path, &container])?;
        check_chain(&staged.chain)?;
        authenticate(self, &staged.path, root_identity, &staged.members, false)?;
        Ok(staged)
    }
    pub(crate) fn install_revision_tree(
        &self,
        mut staged: StagedRevisionTree,
        permit: &WriterPermit,
    ) -> Result<DirectorySync> {
        permit.require_root(&self.root)?;
        self.require_storage_ready()?;
        self.require_tree_capability()?;
        if self.root != staged.vault.root {
            return Err(WikiError::invalid(
                "Revision stage belongs to another vault",
            ));
        }
        self.validate_paths(std::slice::from_ref(&staged.target))?;
        let destination = self.root.resolve(&staged.target)?;
        check_chain(&staged.chain)?;
        authenticate(
            self,
            &staged.path,
            staged.root_identity,
            &staged.members,
            false,
        )?;
        authenticate(
            self,
            &staged.path,
            staged.root_identity,
            &staged.members,
            true,
        )?;
        sync_dirs(self, &[&staged.path])?;
        check_chain(&staged.chain)?;
        authenticate(
            self,
            &staged.path,
            staged.root_identity,
            &staged.members,
            false,
        )?;
        absent(&destination)?;
        self.io
            .install_directory_noreplace(&staged.path, &destination)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    conflict("Revision destination became occupied")
                } else {
                    io_error("install complete Revision", e)
                }
            })?;
        staged.installed = true;
        check_chain(&staged.chain)?;
        authenticate(
            self,
            &destination,
            staged.root_identity,
            &staged.members,
            false,
        )?;
        authenticate(
            self,
            &destination,
            staged.root_identity,
            &staged.members,
            true,
        )?;
        sync_dirs(self, &[&destination])?;
        let support = sync_dirs(
            self,
            &[staged.path.parent().unwrap(), destination.parent().unwrap()],
        )?;
        check_chain(&staged.chain)?;
        authenticate(
            self,
            &destination,
            staged.root_identity,
            &staged.members,
            false,
        )?;
        Ok(support)
    }
    pub(crate) fn sync_revision_tree(
        &self,
        target: &VaultRelativePath,
        expected: &[(VaultRelativePath, Blake3Hash)],
        permit: &WriterPermit,
    ) -> Result<DirectorySync> {
        permit.require_root(&self.root)?;
        self.require_storage_ready()?;
        self.require_tree_capability()?;
        let bookkeeping = workspace_bound(self.root.owned_capacity(), target.owned_capacity())
            .and_then(|base| {
                expected.iter().try_fold(base, |n, (p, h)| {
                    n.checked_add(p.owned_capacity())?
                        .checked_add(h.owned_capacity())
                })
            });
        if expected.len() > 3 || bookkeeping.is_none_or(|n| n > MAX_BYTES) {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "Revision recovery bookkeeping exceeds allowance",
            ));
        }
        if expected.len() != 3 {
            return Err(WikiError::invalid(
                "Revision recovery requires three members",
            ));
        }
        let names = membership(target, expected.iter().map(|(p, _)| p))?;
        self.validate_paths(std::slice::from_ref(target))?;
        let path = self.root.resolve(target)?;
        let chain = capture_chain(&self.root, path.parent().unwrap())?;
        let root_identity = identity(&path, true)?;
        let mut total = 0usize;
        let mut members = Vec::with_capacity(3);
        for ((_, hash), name) in expected.iter().zip(names) {
            let file = path.join(&name);
            let metadata =
                fs::symlink_metadata(&file).map_err(|e| io_error("inspect recovery member", e))?;
            let member_identity = identity_metadata(&metadata, false)?;
            let length = usize::try_from(metadata.len()).map_err(|_| {
                WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "Revision length exceeds host size",
                )
            })?;
            total = total.checked_add(length).ok_or_else(|| {
                WikiError::new(ErrorCode::BudgetExceeded, "Revision length overflow")
            })?;
            if total > MAX_BYTES {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "Revision recovery exceeds aggregate allowance",
                ));
            }
            members.push(Member {
                name,
                hash: hash.clone(),
                length,
                identity: member_identity,
            });
        }
        let container_relative = VaultRelativePath::new(format!(
            "{}/.wiki",
            target.as_str().rsplit_once('/').unwrap().0
        ))?;
        self.root
            .validate_portable_paths(std::slice::from_ref(&container_relative))?;
        let container = self.root.resolve_raw(&container_relative)?;
        let container_identity = match fs::symlink_metadata(&container) {
            Ok(_) => Some(identity(&container, true)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(io_error("inspect recovery stage container", e)),
        };
        authenticate(self, &path, root_identity, &members, true)?;
        sync_dirs(self, &[&path])?;
        if let Some(id) = container_identity {
            if identity(&container, true)? != id {
                return Err(conflict("recovery stage container changed"));
            }
            sync_dirs(self, &[&container])?;
        }
        sync_dirs(self, &[path.parent().unwrap()])?;
        for (dir, _) in chain.iter().rev() {
            sync_dirs(self, &[dir])?;
        }
        check_chain(&chain)?;
        match container_identity {
            Some(id) if identity(&container, true)? != id => {
                return Err(conflict("recovery stage container changed"));
            }
            None => absent(&container)?,
            _ => {}
        }
        authenticate(self, &path, root_identity, &members, false)?;
        Ok(DirectorySync::Supported)
    }
}
// OS-family availability only: kernels/filesystems can still reject the native
// operation. A retained manifest3 must preserve that error, never fall back.
// Deployment-specific runtime capability qualification remains required.
pub(super) fn native_directory_install_supported() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos"))
}
pub(super) fn rename_directory_noreplace(staged: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let from = std::ffi::CString::new(staged.as_os_str().as_bytes())
            .map_err(|_| std::io::ErrorKind::InvalidInput)?;
        let to = std::ffi::CString::new(target.as_os_str().as_bytes())
            .map_err(|_| std::io::ErrorKind::InvalidInput)?;
        #[cfg(target_os = "macos")]
        let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (staged, target);
        Err(std::io::ErrorKind::Unsupported.into())
    }
}
#[cfg(test)]
#[path = "fs_tree_tests.rs"]
mod tests;
