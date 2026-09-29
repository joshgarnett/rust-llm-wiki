# Windows held-handle protection implementation

Sol bounded helper lease, 2026-09-28. Implemented source: `src/vault/windows_security.rs`, `src/vault/acl_policy.rs`, and `tests/windows_acl_policy.rs`. Root owns integration, Cargo/module exports, all existing call sites, fault boundaries, and acceptance. No shared Cargo target, native Windows execution, arbitrary ACL repair, real vault, credentials, or live provider was used.

## Behavior and API

`Protection::{Private,IntegrityProtected}` is the portable conservative policy. Owner must equal the process user SID, LocalSystem, or Builtin Administrators. Thread impersonation is explicitly rejected: a present thread token refuses the operation; only `ERROR_NO_TOKEN` permits process-token lookup. Other token errors are preserved. Token storage is aligned and bounded to 4096 bytes; SIDs have revision/count/exact-extent validation and a 68-byte ceiling.

The held handle supplies `GetSecurityInfo` owner/DACL evidence. Its LocalAlloc descriptor is always freed, allocation is capped at 128 KiB, interior extents are bounded, ACL extent is at most 65535 bytes, and ACE iteration is bounded by both count and remaining byte extent. Missing/null/empty DACLs refuse. Only ordinary allow ACEs are supported; every deny/object/callback/unknown ACE refuses. Generic read/write/execute/all are explicitly mapped to file rights; unknown rights refuse. Outside grants all refuse for Private. IntegrityProtected permits outside read/execute but refuses mutation, ownership/DACL control, deletion and delete-child rights. Effective inherited grants are evaluated identically to explicit grants; directory inherit-only grants also receive policy checks. Unsupported flags, malformed lengths/SIDs, unknown ACL revision and nonzero unused tails refuse. This deliberately accepts fewer ACLs than Windows AccessCheck.

Root-facing `io::Result` helpers:

- `read_protected(path,max,protection)` and `open_checked_file(path,protection,sharing,write)` hold the ancestor chain and one file handle. Bounded reads reject oversized initial/final size, consume at most max+1, then reprove held ACL and pathname identity before returning bytes.
- `CheckedFile::{file,file_mut,identity,verify_binding,into_parts}` exposes the held File and permits retaining its complete guard chain in authority structs. `into_file` is reserved for stages whose caller independently retains the pinned private parent.
- `open_pinned_directory(path,protection)` returns a `DirectoryGuard` owning its ancestor chain; `verify_binding` repeats held-handle and current-path proof.
- `validate_file_security(file,protection)` queries the actual open file. `validate_same_file(file,path,protection)` pins ancestors and compares volume serial plus 128-bit FileIdInfo identity; it also validates held and current object security.
- `create_private_file(path,sharing)` uses CREATE_NEW; `open_or_create_private_lock(path)` uses OPEN_ALWAYS. Both pass an explicit protected owner/DACL SECURITY_ATTRIBUTES to CreateFileW. Existing lock files are validated without ACL mutation. The immediate parent must already be Private.
- `create_private_directory(path)` passes an explicit protected inheritable trusted-principal DACL to CreateDirectoryW. Parents must be IntegrityProtected, so a private directory can be bootstrapped under an existing protected vault. The resulting directory and binding are checked.
- `inspect_directory(path)` opens with backup/reparse flags, proves directory attributes, ACL, identity and ancestor bindings, then returns the existing `DirectorySync::Unsupported`. Inspection and access failures remain errors. No unsupported barrier is represented as supported.

`Sharing::ReadOnly` permits only read sharing. `Sharing::Lock` permits read/write sharing for OS lock contention and excludes deletion sharing. `Sharing::Stage` permits deletion sharing for the caller's guarded rename and requires a Private immediate parent. All handles are non-inheritable. Read-only handles exclude newly conflicting writers/deleters; no proof is made against previously established mappings or the explicitly trusted principals.

Paths must be rooted Disk/VerbatimDisk paths on a fixed local drive; UNC/device paths and ambiguous/traversal components refuse. Ancestor count is capped at 256. Every ancestor is opened, validated, and retained without deletion sharing before descendant opens. All reparse attributes refuse, not only symbolic links. The conservative filesystem set is local NTFS/ReFS with FILE_PERSISTENT_ACLS; unusable/unsupported FileIdInfo refuses instead of weakening identity to timestamps/size. Shared parents with outside mutation grants or foreign owners will be refused, intentionally.

## Root integration constraints

Compile both modules under `cfg(windows)`; the portable integration test imports actual policy by path on macOS. Add exact existing windows-sys features Foundation, Security, Security_Authorization, Storage_FileSystem, System_SystemServices, System_Memory and System_Threading. Root has already added the Memory feature. Avoid ordinary-create-then-protect on Windows: DurableIo private creation must route through the explicit creation helpers, and adapters unable to do so must refuse. Create/validate private managed parents before private files. Existing arbitrary configuration and shared ancestors are never repaired.

Keep returned guards in WriterPermit, RunStore and RunLedgerGuard through authority work. Stage creation may release helper-owned temporary guards through `into_file` only when RunStore already holds the same private parent chain. Ordinary WIKI.md reads must stay distinct from private job reads. The helper is not yet evidence that root call sites satisfy these conditions. Unused optional helpers can require scoped handling during root's full strict lint gate; the standalone harness allows dead code because it intentionally contains no call sites.

## Exact checks and limits

Dedicated harness `/private/tmp/lwiki-windows-security-check` depends only on exact cached windows-sys 0.61.2/windows-link 0.2.1, includes the actual helper/policy source by absolute path, and stubs only the existing two-case DirectorySync enum. Target directory is `/private/tmp/lwiki-windows-security-check-target`. Offline lock generation passed. Initial unqualified Cargo invocation selected the temp directory's default toolchain and failed with missing Windows `core`; explicit `+1.98.0` selects the installed repository toolchain/targets. This was setup failure, not source qualification. The first correct-toolchain check exposed imports/Win32 bool mismatches; they were fixed. A later strict lint exposed elidable lifetimes and constant chunk conversion; those were fixed, with prior failure retained in `/private/tmp/lwiki-windows-security-pre-final-{clippy.log,checks.json}`.

Final unchanged-source checks all passed; command arrays, elapsed times, cwd, exact source/harness hashes and logs are in `/private/tmp/lwiki-windows-security-checks.json`:

```text
rustfmt --edition 2024 --check src/vault/windows_security.rs src/vault/acl_policy.rs tests/windows_acl_policy.rs
rustc +1.98.0 --edition 2024 -D warnings --test tests/windows_acl_policy.rs -o /private/tmp/lwiki-windows-acl-policy-tests
/private/tmp/lwiki-windows-acl-policy-tests                         # 7 passed, zero failures
env CARGO_TARGET_DIR=/private/tmp/lwiki-windows-security-check-target cargo +1.98.0 check --offline --locked --target x86_64-pc-windows-msvc
env CARGO_TARGET_DIR=/private/tmp/lwiki-windows-security-check-target cargo +1.98.0 check --offline --locked --target x86_64-pc-windows-gnu
env CARGO_TARGET_DIR=/private/tmp/lwiki-windows-security-check-target cargo +1.98.0 clippy --offline --locked --target x86_64-pc-windows-msvc -- -D warnings
```

Seven portable parent tests include Private/Integrity distinctions, each outside mutation bit, all 255 unsupported ACE types, foreign owner, null/empty DACL, inherited and directory inherit-only behavior, malformed/truncated lengths/flags/SIDs and generic/unknown access masks. They validate the actual policy, not a duplicated model.

Final SHA-256 source fingerprints:

```text
src/vault/windows_security.rs 2746d5ce193aee01c3af95db227b3a2b5a168bbf6a924d189aa1a4fed3a57a2c
src/vault/acl_policy.rs f6599a05ee449eeddf91ab41764cc9084e9b2ba27db1d73fa75cd1f46ba30875
tests/windows_acl_policy.rs dbbae84d4bebf5f9d8ef8b8ef1cafc830c55d7604723b4956b1be48b7646399e
```

Cross-checks prove API type/feature compatibility for this helper only; portable tests prove conservative parsed policy decisions only. They do not qualify native DACL creation/inheritance, real handle sharing/lock contention, reparse resistance, actual filesystem support, full-crate Windows routing, native crash safety, or live providers. Directory durability remains honestly unsupported and required paid work must remain blocked before send. Independent Astra review and root integration gates remain required.
