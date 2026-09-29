# Windows protected storage implementation plan

Independent Astra static review, 2026-09-28. Scope: execution STATE, provider protected reads, operational spool/run locks, vault paths/durability, the Windows credential-helper handle boundary, and cached `windows-sys 0.61.2` declarations. No source changes, Cargo, tests, native Windows execution, or live provider calls. This report proposes implementation; it does not accept Windows protection or durability.

## Ranked source gaps

1. **High — permissions are silently accepted on Windows.** `src/config/providers.rs:572` discards the permission arguments outside Unix; `src/vault/operational.rs:63` and `:72` return success without restricting files/directories. Provider configuration, credentials, custom CA material, requests, and receipts need actual Windows protection. Implementing ACL checks remains core work, as STATE requires.
2. **High — path identity is not established.** Provider reads compare inode identities only on Unix (`src/config/providers.rs:542`, `:563`). `src/vault/operational.rs:96` discards the opened metadata, although run-lock validation depends on `same_inode`. `src/vault/lock.rs:40` opens a pathname and `:84` subsequently validates only the vault root. A replacement lock can therefore defeat the intended one-file lock boundary. Rust's Windows open defaults permit deletion sharing; select explicit sharing for protected handles. [Rust OpenOptionsExt](https://doc.rust-lang.org/std/os/windows/fs/trait.OpenOptionsExt.html)
3. **High — securing after creation leaves an exposure window.** Operational creation calls ordinary create, then `private_file`/`private_dir` (`:223`, `:236`, `:483`, `:577`). An inherited permissive ACL can allow another principal to open a new file before restriction, or insert a child into a new directory. Changing its DACL later does not revoke already granted handle access. Private objects must be private when created.
4. **Capability gap — directory durability is unavailable, honestly.** `src/vault/fs.rs:85` returns `Unsupported` outside Unix; `RunStore::sync_dir` at `src/vault/operational.rs:277` correctly turns that into `CapabilityUnavailable`. `WriterPermit::acquire` at `src/vault/lock.rs:26` ignores the returned enum. Audit that caller's promised durability explicitly; do not mistake successful `Result` for a supported barrier.

## Root-owned interfaces and bounded lease

Add one internal Windows storage module; keep existing Unix branches unchanged. Suggested interfaces are descriptive, not a required public API:

```text
Protection = Private | IntegrityProtected
FileIdentity = { volume_serial: u64, file_id: [u8; 16] }
open_checked_file(path, protection, sharing) -> CheckedFile
CheckedFile::{identity, validate_security, read_bounded, verify_binding}
create_private_file(path, disposition, access, sharing) -> CheckedFile
create_private_directory(path, parent_guard) -> DirectoryGuard
open_pinned_directory(path, protection) -> DirectoryGuard
validate_same_file(held_file, current_path, parent_guard) -> Result
sync_directory_native(held_directory) -> Result<DirectorySync>
```

These should remain private helpers, not caller-supplied proofs. Hold real owned handles and derive identity/security from them. Root must route both config reads and operational reads through them and add private creation operations to the `DurableIo` boundary, so injected failures still observe creation/write/flush/rename. Do not bypass `DurableIo` for private writes. A default private-creation method must fail unsupported if an adapter cannot fulfill its contract; ordinary creation followed by an ACL change is insufficient on Windows.

Use distinct ordinary and private read entry points: operational `read` also reads public `WIKI.md` at `:130`, so adding a universal private ACL requirement there would reject valid vaults. Reopening existing app-owned files validates protection; do not silently repair arbitrary user configuration or shared ancestors.

## Conservative ACL policy

Query the actual held handle with `GetSecurityInfo(SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION)`, requesting `READ_CONTROL` when opening it. Free its returned descriptor using `LocalFree`; all returned interior pointers share that descriptor's lifetime. Do not use a later pathname-only ACL query as evidence for bytes read from an earlier handle. [GetSecurityInfo](https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-getsecurityinfo)

The trust set should be explicit: the effective current user SID, LocalSystem, and Builtin Administrators. These elevated principals are outside the confidentiality boundary; reject other owners because ownership can confer DACL-control authority. Do not trust arbitrary groups merely because the current user belongs to them. Resolve impersonation deliberately: use a thread token when present, otherwise a process token, or reject impersonated callers explicitly. Query `TokenUser` with bounded two-pass allocation and `TOKEN_QUERY`. [GetTokenInformation](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-gettokeninformation)

For the first conservative validator:

- Require a valid, present, non-null DACL. A null DACL grants unrestricted access; an empty DACL grants none and must not be confused with private usable storage. Require the actual needed open/access to succeed. [Null and empty DACLs](https://learn.microsoft.com/en-us/windows/win32/secauthz/null-dacls-and-empty-dacls)
- For **Private**, reject every effective allow grant to a principal outside the trust set. This intentionally rejects some harmless metadata-only ACLs and complicated otherwise-safe ACLs.
- For **IntegrityProtected** CA files, allow outside read/execute grants, but reject write-data, append, write-EA, write-attributes, delete, `WRITE_DAC`, and `WRITE_OWNER`. Parent directories additionally must not give outside principals child creation/replacement or `FILE_DELETE_CHILD` rights. Map generic rights with the file generic mapping; reject unresolved/unknown masks.
- Safest initial implementation accepts ordinary allow ACEs only and rejects deny/object/callback/conditional or otherwise unsupported ACEs. This yields conservative false rejections. A later basic-deny implementation may ignore denies when computing an upper bound on grants; it must never subtract arbitrary deny masks from allow masks and claim to reproduce Windows access checks. ACE order and inheritance affect effective access. [ACE ordering](https://learn.microsoft.com/en-us/windows/win32/secauthz/order-of-aces-in-a-dacl)
- Evaluate inherited effective grants exactly like explicit grants. An inheritance-only ACE does not grant present-object access, but still matters on a directory whose children must stay private. Require safe inheritable policy on private directories. Bound and validate ACE sizes, SID extents, and iteration before dereferencing; unknown formats fail closed.

Create new app-owned objects using an explicit protected DACL in `SECURITY_ATTRIBUTES`, with `bInheritHandle = FALSE`. Grant the trusted SIDs the required rights; private directories can use object/container inheritance for those same trusted principals. `CreateFileW(CREATE_NEW)` and `CreateDirectoryW` apply that descriptor at creation. Verify the filesystem actually supports persistent ACLs and validate the resulting handle. Do not recursively protect a shared ancestor as a side effect. `SetSecurityInfo` can propagate inheritable ACEs to existing children and does not canonicalize arbitrary ACE order. [CreateDirectoryW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createdirectoryw), [SetSecurityInfo](https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-setsecurityinfo)

Both child and parent protections matter: a parent can authorize replacement/deletion; conversely, private parent permissions alone do not ensure children cannot be read, because traverse checking is commonly bypassed. [File security and access rights](https://learn.microsoft.com/en-us/windows/win32/fileio/file-security-and-access-rights)

## Held handles, reparse points, and lock identity

For bounded provider reads: open once with `OPEN_EXISTING`, `FILE_FLAG_OPEN_REPARSE_POINT`, read/attribute/security-query access, and read sharing only. Check disk-file type, non-directory attributes, no `FILE_ATTRIBUTE_REPARSE_POINT`, bounded size, owner/DACL, and `FileIdInfo` on that handle. Read at most `max + 1` from that same handle. Recheck security/size and the path binding before returning. No sharing with writers or deleters prevents new conflicting data-write/delete opens while held; it is not protection against a trusted owner changing security or preexisting mappings.

Identity uses `GetFileInformationByHandleEx(FileIdInfo)`'s volume serial plus 128-bit file ID, not size/time. Reject unsupported or unusable identity rather than substituting those weaker fields. A conservative first implementation can explicitly reject remote and non-ACL filesystems instead of asserting their handle/identity semantics are equivalent. [FILE_ID_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_info)

Reject all reparse-point attributes, not just the recognized symbolic-link forms checked in `src/vault/paths.rs:55`, `:111`, and `:216`. `OPEN_REPARSE_POINT` addresses the final component only. Validate and hold the managed ancestor directory chain without delete sharing while opening descendants; reject unsafe ancestor permissions/reparse points and check final containment/identity. Checking strings and then reopening through an unpinned mutable ancestor leaves a race. The policy excludes attacks by the same user or elevated trusted principals; it must not claim protection against them. [CreateFile sharing and reparse semantics](https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-createfilea)

Persistent run/writer lock handles need read/write sharing so contenders can reach the OS lock, but **no delete sharing**. Recheck the same held identity against the current managed pathname before authority-bearing work; hold ancestor guards through the operation. Apply this to both `RunLedgerGuard::check` and `WriterPermit::require_root`. Stage files differ: `replace_path` retains the stage handle through rename (`src/vault/operational.rs:558`). Removing delete sharing from every file would break self-renames. Either use an appropriately shared stage handle under a pinned private parent, close it at an explicitly validated boundary, or use a handle-based rename with the necessary rights; root must freeze that choice.

The Windows credential helper at `src/providers/credentials.rs:824` already manages explicit inherited pipe handles and suspended-process Job Object assignment. New sensitive storage handles must stay non-inheritable and outside its inherited handle list; no helper redesign is required by this review.

## Directory durability: truthful capability

Implement a Windows-specific directory-open/inspection adapter, using `CreateFileW(OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)` with an appropriate held-directory identity. Microsoft documents opening directory handles this way. [Directory handles](https://learn.microsoft.com/en-us/windows/win32/fileio/obtaining-a-handle-to-a-directory)

`FlushFileBuffers` requires write access and documents flushing a file's buffered information. The documentation inspected does **not** establish a portable guarantee that flushing a directory handle durably orders every child create/rename/delete. Therefore merely obtaining a directory handle, calling `File::sync_all`, or observing one successful native flush is not enough evidence to label the current transaction contract supported. This is a limit of the documented guarantee, not a claim that every Windows filesystem necessarily fails the operation. [FlushFileBuffers](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers)

The smallest safe initial implementation returns `Unsupported` where this required barrier is unestablished. An optional native flush attempt must preserve errors: recognized unsupported-operation failures can produce `Unsupported`; permission, I/O, identity, and malformed-path failures remain errors. Never convert access denied into successful durability. Only expose `Supported` for a concretely justified platform/filesystem barrier. `MoveFileExW(MOVEFILE_WRITE_THROUGH)` is not a universal substitute for directory creation/removal barriers; its documented copy/delete flushing guarantee does not prove all current transaction operations. [MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw)

Keep paid jobs blocked before send if required durable private storage is unavailable. ACL implementation can and must complete independently. A remaining barrier capability restriction must be stated as an unsupported operation, not hidden under a blanket native-qualification deferral or called fully functional Windows persistence.

## Features, checks, and acceptance limits

Cached `windows-sys = 0.61.2` exposes all proposed core functions. Add `Win32_Security_Authorization` for `GetSecurityInfo`/`SetSecurityInfo`, and `Win32_Storage_FileSystem` for creation, handle attributes/identity, and flush. Existing `Win32_Security`, `Win32_Foundation`, and `Win32_System_Threading` cover SID/token basics, `LocalFree`, and token opening. If using the symbolic volume flag `FILE_PERSISTENT_ACLS`, also add `Win32_System_SystemServices` (the equivalent `FS_PERSISTENT_ACLS` is in `Win32_System_WindowsProgramming`); select one, not both. Cargo remains root-owned.

Suggested separate bounded implementation lease: one Windows helper module plus explicitly enumerated call sites; root owns `DurableIo` changes and Cargo. Extract a platform-neutral parsed ACL decision model for local tests covering outside allows, generic masks, null/empty DACL, owners, inherited/inherit-only entries, unsupported ACEs, and malformed lengths. Cross-check Windows compilation for feature names, signatures, ownership, and cfg routing. These local checks do not prove live ACL behavior, lock contention, inheritance, reparse resistance, or crash durability. Disposable native Windows fixtures should later exercise those behaviors, including adversarial pathname replacement, preexisting insecure objects, two lock contenders, inherited-handle exclusion, and truthful unsupported directory barriers. No such checks were run during this review.

Reviewed SHA-256 snapshots:

```text
src/config/providers.rs a943a62f99d6738d9f2516c1b0ef0cceda42ab4be90a7023c76d691871e6606b
src/vault/fs.rs 75e468e0b3665e90684607aeb7574217ee807a60ddf42f31d92706bcbc236204
src/vault/operational.rs 0f0ece6e0ccbab0054f3b930df40aca65d09dbd385ed158f9e6f5aa9e087cab9
src/vault/lock.rs 554b5b9c7174384fd177d3ae97747e73a61df6c7da5d3c3df90edfdf9be14792
src/vault/paths.rs a22b6506e2277acfecfa3ba638866bde42448416df58da5860cd40cd87357988
src/providers/credentials.rs 4dcd9ad68f662cac473c758af80e2689f14adf4f1bb07155de340062248c4a9e
Cargo.toml 2ba74d7f3d7c02928a527e66a69190fa34b0ad7c0d300fdfe6a61b749924ed5f
```
