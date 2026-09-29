//! Conservative Windows held-handle protection. No pathname ACL repair.
//!
//! The boundary excludes the current user, SYSTEM and Administrators as attackers.
//! Remote filesystems, impersonation, reparses, and unrecognized ACLs fail closed.
use super::DirectorySync;
use super::acl_policy::{MAX_ACL_BYTES, MAX_SID_BYTES, Protection, valid_sid, validate_acl};
use std::{
    ffi::c_void,
    fs::File,
    io::{self, Read},
    mem::{size_of, zeroed},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_TOKEN, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{
        Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
        *,
    },
    Storage::FileSystem::*,
    System::{
        Memory::LocalSize,
        SystemServices::{FILE_PERSISTENT_ACLS, SECURITY_DESCRIPTOR_REVISION},
        Threading::{GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    pub(crate) volume_serial: u64,
    pub(crate) file_id: [u8; 16],
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Sharing {
    ReadOnly,
    Lock,
    Stage,
}
impl Sharing {
    fn mask(self) -> u32 {
        match self {
            Self::ReadOnly => FILE_SHARE_READ,
            Self::Lock => FILE_SHARE_READ | FILE_SHARE_WRITE,
            Self::Stage => FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        }
    }
}

#[derive(Debug)]
pub(crate) struct DirectoryGuard {
    file: File,
    path: PathBuf,
    identity: FileIdentity,
    protection: Protection,
    ancestors: Vec<DirectoryGuard>,
}
impl DirectoryGuard {
    pub(crate) fn verify_binding(&self) -> io::Result<()> {
        for ancestor in &self.ancestors {
            ancestor.verify_binding()?;
        }
        prove_binding(&self.file, &self.path, self.identity, self.protection, true)
    }
}

#[derive(Debug)]
pub(crate) struct CheckedFile {
    file: File,
    path: PathBuf,
    identity: FileIdentity,
    protection: Protection,
    ancestors: Vec<DirectoryGuard>,
}
impl CheckedFile {
    pub(crate) fn file(&self) -> &File {
        &self.file
    }
    pub(crate) fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }
    /// Only for private stages whose caller independently retains parent guards.
    pub(crate) fn into_file(self) -> File {
        self.file
    }
    pub(crate) fn into_parts(self) -> (File, Vec<DirectoryGuard>) {
        (self.file, self.ancestors)
    }
    pub(crate) fn verify_binding(&self) -> io::Result<()> {
        for ancestor in &self.ancestors {
            ancestor.verify_binding()?;
        }
        prove_binding(
            &self.file,
            &self.path,
            self.identity,
            self.protection,
            false,
        )
    }
    pub(crate) fn read_bounded(mut self, max: usize) -> io::Result<Vec<u8>> {
        let ceiling = u64::try_from(max)
            .ok()
            .and_then(|v| v.checked_add(1))
            .ok_or_else(|| invalid("invalid protected read ceiling"))?;
        if inspect(&self.file, false)?.1 > max as u64 {
            return Err(invalid("protected read ceiling exceeded"));
        }
        let mut bytes = Vec::new();
        self.file.by_ref().take(ceiling).read_to_end(&mut bytes)?;
        if bytes.len() > max || inspect(&self.file, false)?.1 != bytes.len() as u64 {
            return Err(invalid("protected file size changed or exceeded ceiling"));
        }
        self.verify_binding()?;
        Ok(bytes)
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}
fn check_bool(success: i32) -> io::Result<()> {
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn handle(file: &File) -> HANDLE {
    file.as_raw_handle()
}
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
    if value.is_empty() || value.len() > 32760 || value.contains(&0) {
        return Err(invalid("invalid protected Windows path"));
    }
    value.push(0);
    Ok(value)
}

/// Reject UNC/device namespaces and lexical traversal instead of normalizing them.
fn path_chain(path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut components = path.components();
    let prefix = match components.next() {
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) =>
        {
            prefix
        }
        _ => {
            return Err(unsupported(
                "protected storage requires an absolute local drive path",
            ));
        }
    };
    if components.next() != Some(Component::RootDir) {
        return Err(invalid("protected path is not rooted"));
    }
    let mut current = PathBuf::from(prefix.as_os_str());
    current.push(Path::new("\\"));
    let root_wide = wide(&current)?;
    // DRIVE_FIXED = 3 (the symbolic constant is in WindowsProgramming).
    if unsafe { GetDriveTypeW(root_wide.as_ptr()) } != 3 {
        return Err(unsupported(
            "protected storage requires a fixed local drive",
        ));
    }
    let mut result = vec![current.clone()];
    for component in components {
        match component {
            Component::Normal(name) => {
                let encoded: Vec<u16> = name.encode_wide().collect();
                // Win32 alternate streams, wildcard and trailing normalization aliases.
                if encoded.is_empty()
                    || encoded
                        .iter()
                        .any(|c| *c < 32 || [b':' as u16, b'*' as u16, b'?' as u16].contains(c))
                    || matches!(encoded.last(), Some(32 | 46))
                {
                    return Err(invalid("ambiguous protected path component"));
                }
                current.push(name);
                result.push(current.clone());
            }
            _ => return Err(invalid("protected path contains traversal")),
        }
        if result.len() > 256 {
            return Err(invalid("protected path ancestor ceiling exceeded"));
        }
    }
    Ok(result)
}

fn open_raw(
    path: &Path,
    directory: bool,
    access: u32,
    sharing: Sharing,
    disposition: u32,
    attributes: *const SECURITY_ATTRIBUTES,
) -> io::Result<File> {
    let path = wide(path)?;
    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | if directory {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            0
        };
    let raw = unsafe {
        CreateFileW(
            path.as_ptr(),
            access | READ_CONTROL | FILE_READ_ATTRIBUTES,
            sharing.mask(),
            attributes,
            disposition,
            flags,
            null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // CreateFileW handles are non-inheritable when attributes is null or explicitly false.
    Ok(unsafe { File::from_raw_handle(raw) })
}

fn inspect(file: &File, directory: bool) -> io::Result<(FileIdentity, u64)> {
    if unsafe { GetFileType(handle(file)) } != FILE_TYPE_DISK {
        return Err(unsupported("protected handle is not a disk file"));
    }
    let mut attributes: FILE_ATTRIBUTE_TAG_INFO = unsafe { zeroed() };
    let mut info: FILE_ID_INFO = unsafe { zeroed() };
    let mut standard: FILE_STANDARD_INFO = unsafe { zeroed() };
    unsafe {
        check_bool(GetFileInformationByHandleEx(
            handle(file),
            FileAttributeTagInfo,
            (&mut attributes as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        ))?;
        check_bool(GetFileInformationByHandleEx(
            handle(file),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        ))?;
        check_bool(GetFileInformationByHandleEx(
            handle(file),
            FileStandardInfo,
            (&mut standard as *mut FILE_STANDARD_INFO).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        ))?;
    }
    if attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
        || standard.Directory != directory
        || standard.DeletePending
        || standard.EndOfFile < 0
    {
        return Err(invalid(
            "protected object type, reparse or deletion state is unsafe",
        ));
    }
    if info.FileId.Identifier == [0; 16] {
        return Err(unsupported(
            "filesystem did not supply a usable file identity",
        ));
    }
    let mut flags = 0;
    let mut fs_name = [0u16; 32];
    unsafe {
        check_bool(GetVolumeInformationByHandleW(
            handle(file),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            &mut flags,
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        ))?;
    }
    let fs_name = String::from_utf16_lossy(
        &fs_name[..fs_name
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(fs_name.len())],
    );
    if flags & FILE_PERSISTENT_ACLS == 0 || !matches!(fs_name.as_str(), "NTFS" | "ReFS") {
        return Err(unsupported(
            "filesystem lacks qualified local ACL and identity semantics",
        ));
    }
    Ok((
        FileIdentity {
            volume_serial: info.VolumeSerialNumber,
            file_id: info.FileId.Identifier,
        },
        standard.EndOfFile as u64,
    ))
}

struct Token(HANDLE);
impl Drop for Token {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn trusted_sids() -> io::Result<Vec<Vec<u8>>> {
    let mut token = null_mut();
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } != 0 {
        let _token = Token(token);
        return Err(unsupported(
            "protected storage refuses impersonated callers",
        ));
    }
    if unsafe { GetLastError() } != ERROR_NO_TOKEN {
        return Err(io::Error::last_os_error());
    }
    unsafe {
        check_bool(OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY,
            &mut token,
        ))?;
    }
    let token = Token(token);
    let mut needed = 0;
    if unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut needed) } != 0
        || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER
        || needed < size_of::<TOKEN_USER>() as u32
        || needed > 4096
    {
        return Err(invalid("invalid token information extent"));
    }
    let mut storage = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    let capacity = needed as usize;
    unsafe {
        check_bool(GetTokenInformation(
            token.0,
            TokenUser,
            storage.as_mut_ptr().cast(),
            needed,
            &mut needed,
        ))?;
    }
    if needed as usize > capacity || needed < size_of::<TOKEN_USER>() as u32 {
        return Err(invalid("token information extent changed"));
    }
    let user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
    let token_bytes =
        unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), needed as usize) };
    let user_sid = bounded_sid(user.User.Sid, token_bytes)?;
    let mut trusted = vec![user_sid.to_vec()];
    for kind in [WinLocalSystemSid, WinBuiltinAdministratorsSid] {
        // Win32 writes a DWORD-aligned SID, then comparisons use owned bytes.
        let mut sid_storage = [0u32; MAX_SID_BYTES / 4];
        let mut length = MAX_SID_BYTES as u32;
        unsafe {
            check_bool(CreateWellKnownSid(
                kind,
                null_mut(),
                sid_storage.as_mut_ptr().cast(),
                &mut length,
            ))?;
        }
        if length as usize > MAX_SID_BYTES {
            return Err(invalid("well-known SID extent exceeded"));
        }
        let sid = unsafe {
            std::slice::from_raw_parts(sid_storage.as_ptr().cast::<u8>(), length as usize)
        }
        .to_vec();
        if !valid_sid(&sid) {
            return Err(invalid("invalid well-known SID"));
        }
        trusted.push(sid);
    }
    Ok(trusted)
}

fn bounded_bytes(pointer: *const c_void, backing: &[u8], length: usize) -> io::Result<&[u8]> {
    let offset = (pointer as usize)
        .checked_sub(backing.as_ptr() as usize)
        .ok_or_else(|| invalid("security pointer outside allocation"))?;
    if pointer.is_null()
        || offset
            .checked_add(length)
            .is_none_or(|end| end > backing.len())
    {
        return Err(invalid("security extent outside allocation"));
    }
    Ok(&backing[offset..offset + length])
}
fn bounded_sid(pointer: PSID, backing: &[u8]) -> io::Result<&[u8]> {
    let header = bounded_bytes(pointer, backing, 8)?;
    let length = 8 + usize::from(header[1]) * 4;
    if length > MAX_SID_BYTES {
        return Err(invalid("SID extent exceeded"));
    }
    let sid = bounded_bytes(pointer, backing, length)?;
    if !valid_sid(sid) {
        return Err(invalid("invalid security SID"));
    }
    Ok(sid)
}
struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn validate_security(file: &File, protection: Protection, directory: bool) -> io::Result<()> {
    let trusted = trusted_sids()?;
    let mut descriptor = null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle(file),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let descriptor = Descriptor(descriptor);
    // OWNER plus one DACL is bounded by the u16 ACL extent plus bounded SIDs.
    let allocation = unsafe { LocalSize(descriptor.0) };
    if allocation == 0
        || allocation > 128 * 1024
        || unsafe { IsValidSecurityDescriptor(descriptor.0) } == 0
    {
        return Err(invalid("invalid or excessive security descriptor"));
    }
    let length = unsafe { GetSecurityDescriptorLength(descriptor.0) } as usize;
    if length > allocation || length < 20 {
        return Err(invalid("invalid security descriptor extent"));
    }
    let mut owner = null_mut();
    let mut defaulted = 0;
    let mut present = 0;
    let mut dacl = null_mut();
    unsafe {
        check_bool(GetSecurityDescriptorOwner(
            descriptor.0,
            &mut owner,
            &mut defaulted,
        ))?;
        check_bool(GetSecurityDescriptorDacl(
            descriptor.0,
            &mut present,
            &mut dacl,
            &mut defaulted,
        ))?;
    }
    // The OS returned a valid self-contained descriptor; tie every interior
    // byte slice to this borrow, so it cannot outlive the LocalFree owner.
    let descriptor_bytes = unsafe { std::slice::from_raw_parts(descriptor.0.cast::<u8>(), length) };
    let owner = bounded_sid(owner, descriptor_bytes)?;
    if present == 0 || dacl.is_null() {
        return Err(invalid("protected object lacks a non-null DACL"));
    }
    let header = bounded_bytes(dacl.cast(), descriptor_bytes, 8)?;
    let acl_length = usize::from(u16::from_le_bytes([header[2], header[3]]));
    if acl_length > MAX_ACL_BYTES {
        return Err(invalid("ACL extent exceeded"));
    }
    let acl = bounded_bytes(dacl.cast(), descriptor_bytes, acl_length)?;
    validate_acl(owner, Some(acl), &trusted, protection, directory).map_err(invalid)
}

pub(crate) fn validate_file_security(file: &File, protection: Protection) -> io::Result<()> {
    inspect(file, false)?;
    validate_security(file, protection, false)
}
fn pin(path: &Path, protection: Protection) -> io::Result<DirectoryGuard> {
    let file = open_raw(path, true, 0, Sharing::Lock, OPEN_EXISTING, null())?;
    let identity = inspect(&file, true)?.0;
    validate_security(&file, protection, true)?;
    Ok(DirectoryGuard {
        file,
        path: path.to_path_buf(),
        identity,
        protection,
        ancestors: vec![],
    })
}
fn pinned_parents(path: &Path, private_parent: bool) -> io::Result<Vec<DirectoryGuard>> {
    let mut chain = path_chain(path)?;
    if chain.len() < 2 {
        return Err(invalid("protected child has no parent"));
    }
    chain.pop();
    let last = chain.len() - 1;
    chain
        .iter()
        .enumerate()
        .map(|(index, path)| {
            pin(
                path,
                if private_parent && index == last {
                    Protection::Private
                } else {
                    Protection::IntegrityProtected
                },
            )
        })
        .collect()
}
pub(crate) fn open_pinned_directory(
    path: &Path,
    protection: Protection,
) -> io::Result<DirectoryGuard> {
    let chain = path_chain(path)?;
    let mut ancestors = Vec::new();
    for ancestor in chain.iter().take(chain.len() - 1) {
        ancestors.push(pin(ancestor, Protection::IntegrityProtected)?);
    }
    let mut guard = pin(path, protection)?;
    guard.ancestors = ancestors;
    guard.verify_binding()?;
    Ok(guard)
}

fn checked_file(
    file: File,
    path: &Path,
    protection: Protection,
    ancestors: Vec<DirectoryGuard>,
) -> io::Result<CheckedFile> {
    let identity = inspect(&file, false)?.0;
    validate_security(&file, protection, false)?;
    let checked = CheckedFile {
        file,
        path: path.to_path_buf(),
        identity,
        protection,
        ancestors,
    };
    checked.verify_binding()?;
    Ok(checked)
}
pub(crate) fn open_checked_file(
    path: &Path,
    protection: Protection,
    sharing: Sharing,
    write: bool,
) -> io::Result<CheckedFile> {
    let ancestors = pinned_parents(path, matches!(sharing, Sharing::Stage))?;
    let access = FILE_GENERIC_READ | if write { FILE_GENERIC_WRITE } else { 0 };
    let file = open_raw(path, false, access, sharing, OPEN_EXISTING, null())?;
    checked_file(file, path, protection, ancestors)
}
pub(crate) fn read_protected(
    path: &Path,
    max: usize,
    protection: Protection,
) -> io::Result<Vec<u8>> {
    open_checked_file(path, protection, Sharing::ReadOnly, false)?.read_bounded(max)
}
fn prove_binding(
    file: &File,
    path: &Path,
    expected: FileIdentity,
    protection: Protection,
    directory: bool,
) -> io::Result<()> {
    if inspect(file, directory)?.0 != expected {
        return Err(invalid("held protected identity changed"));
    }
    validate_security(file, protection, directory)?;
    // Metadata/security-only open coexists with a read-only held file. Sharing
    // admits the held writer/stage, while that original handle blocks deletion.
    let current = open_raw(path, directory, 0, Sharing::Stage, OPEN_EXISTING, null())?;
    if inspect(&current, directory)?.0 != expected {
        return Err(invalid("protected pathname identity changed"));
    }
    validate_security(&current, protection, directory)
}
pub(crate) fn validate_same_file(
    file: &File,
    path: &Path,
    protection: Protection,
) -> io::Result<()> {
    let guards = pinned_parents(path, false)?;
    prove_binding(file, path, inspect(file, false)?.0, protection, false)?;
    for guard in guards {
        guard.verify_binding()?;
    }
    Ok(())
}

struct PrivateDescriptor {
    descriptor: Box<SECURITY_DESCRIPTOR>,
    _acl: Vec<u32>,
    _trusted: Vec<Vec<u32>>,
}
impl PrivateDescriptor {
    fn new(directory: bool) -> io::Result<Self> {
        // Keep every SID DWORD-aligned for Win32 descriptor/ACE functions.
        let trusted: Vec<Vec<u32>> = trusted_sids()?
            .iter()
            .map(|sid| {
                sid.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|chunk| u32::from_ne_bytes(*chunk))
                    .collect()
            })
            .collect();
        let acl_size = 8 + trusted.iter().map(|sid| 8 + sid.len() * 4).sum::<usize>();
        let mut acl = vec![0u32; acl_size.div_ceil(4)];
        let raw_acl = acl.as_mut_ptr().cast::<ACL>();
        unsafe {
            check_bool(InitializeAcl(raw_acl, acl_size as u32, ACL_REVISION))?;
        }
        for sid in &trusted {
            unsafe {
                check_bool(AddAccessAllowedAceEx(
                    raw_acl,
                    ACL_REVISION,
                    if directory {
                        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
                    } else {
                        0
                    },
                    FILE_ALL_ACCESS,
                    sid.as_ptr() as PSID,
                ))?;
            }
        }
        let mut descriptor: Box<SECURITY_DESCRIPTOR> = Box::new(unsafe { zeroed() });
        let raw = (&mut *descriptor as *mut SECURITY_DESCRIPTOR).cast();
        unsafe {
            check_bool(InitializeSecurityDescriptor(
                raw,
                SECURITY_DESCRIPTOR_REVISION,
            ))?;
            check_bool(SetSecurityDescriptorOwner(
                raw,
                trusted[0].as_ptr() as PSID,
                0,
            ))?;
            check_bool(SetSecurityDescriptorDacl(raw, 1, raw_acl, 0))?;
            check_bool(SetSecurityDescriptorControl(
                raw,
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            ))?;
        }
        Ok(Self {
            descriptor,
            _acl: acl,
            _trusted: trusted,
        })
    }
    fn attributes(&mut self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: (&mut *self.descriptor as *mut SECURITY_DESCRIPTOR).cast(),
            bInheritHandle: 0,
        }
    }
}
fn create_private(path: &Path, sharing: Sharing, disposition: u32) -> io::Result<CheckedFile> {
    let ancestors = pinned_parents(path, true)?;
    let mut descriptor = PrivateDescriptor::new(false)?;
    let attributes = descriptor.attributes();
    let file = open_raw(
        path,
        false,
        FILE_GENERIC_READ | FILE_GENERIC_WRITE,
        sharing,
        disposition,
        &attributes,
    )?;
    checked_file(file, path, Protection::Private, ancestors).map_err(|error| {
        if disposition == CREATE_NEW {
            after_creation(error)
        } else {
            error
        }
    })
}
/// The exclusive OS creation succeeded, but its subsequent validation failed.
/// This records an observed creation, not permission to clean up through its path.
#[derive(Debug)]
struct CreatedBeforeFailure(io::Error);
impl std::fmt::Display for CreatedBeforeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for CreatedBeforeFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}
fn after_creation(error: io::Error) -> io::Error {
    io::Error::new(error.kind(), CreatedBeforeFailure(error))
}
pub(crate) fn created_before_failure(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<CreatedBeforeFailure>())
}
pub(crate) fn create_private_file(path: &Path, sharing: Sharing) -> io::Result<CheckedFile> {
    create_private(path, sharing, CREATE_NEW)
}
pub(crate) fn open_or_create_private_lock(path: &Path) -> io::Result<CheckedFile> {
    create_private(path, Sharing::Lock, OPEN_ALWAYS)
}
pub(crate) fn create_private_directory(path: &Path) -> io::Result<DirectoryGuard> {
    let ancestors = pinned_parents(path, false)?;
    let mut descriptor = PrivateDescriptor::new(true)?;
    let attributes = descriptor.attributes();
    let encoded = wide(path)?;
    unsafe {
        check_bool(CreateDirectoryW(encoded.as_ptr(), &attributes))?;
    }
    (|| {
        let mut guard = pin(path, Protection::Private)?;
        guard.ancestors = ancestors;
        guard.verify_binding()?;
        Ok(guard)
    })()
    .map_err(after_creation)
}
/// Inspection errors stay errors; Windows directory entry durability is unproven.
pub(crate) fn inspect_directory(path: &Path) -> io::Result<DirectorySync> {
    open_pinned_directory(path, Protection::IntegrityProtected)?.verify_binding()?;
    Ok(DirectorySync::Unsupported)
}
