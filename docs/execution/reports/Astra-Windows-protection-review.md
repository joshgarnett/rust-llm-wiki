# Independent Windows protection helper review

2026-09-28. Bounded Astra static review of the approved protection plan, Sol's Windows-protection report, `windows_security.rs`, `acl_policy.rs`, their portable tests, and the recorded standalone check harness/logs. No source changes, Cargo, tests, native Windows execution, or nested delegation. Only this report edited.

**Disposition: no concrete unsafe-memory, forbidden-grant false acceptance, or blocking helper-interface defect found in the reviewed candidate.** The helper is suitable for root integration under its explicit conservative policy. This is not acceptance of as-yet-unintegrated provider/spool/lock/export call sites or native Windows behavior. Guard lifetimes and unavailable-capability handling remain necessary integration conditions.

## Unsafe and policy boundaries

- `windows_security.rs:323` rejects impersonation rather than silently interpreting the process SID as the effective user. It only falls back after `ERROR_NO_TOKEN`; other errors refuse. The process token is closed through RAII. Two-pass `TokenUser` storage is bounded to4096bytes, pointer-aligned via `Vec<usize>`, and checked again against the returned length before interpretation. SID bytes are copied after checking their complete range inside that backing allocation. Well-known SID output uses DWORD-aligned bounded arrays. This matches the token-query buffer/access contract. [GetTokenInformation](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-gettokeninformation)
- `:431` queries owner and DACL through the actual file handle, whose open requested `READ_CONTROL`. The descriptor has one RAII `LocalFree` owner; returned interior byte slices remain within its borrow. `LocalSize`, descriptor length, SID extent, ACL extent, and per-ACE bounds are checked before the Rust policy parser slices them. The initial Windows descriptor-validation functions rely on the successful OS API's returned descriptor contract; this is not a safe parser for caller-supplied arbitrary raw pointers. The128KiB limit is a post-query acceptance cap, not a promise that no OS allocation occurred before the check. [GetSecurityInfo](https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-getsecurityinfo), [LocalSize](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-localsize)
- `acl_policy.rs:57` validates the exact owner SID against the fixed trusted set, rejects absent/null/empty DACLs, and checks ACL version, lengths, bounded ACE count, flags, alignment-sized records, complete SID bytes, and unused tails. Parsing uses checked slice access; bounded ACL size prevents its small offset additions from overflowing. It accepts only ordinary allow ACEs, so denies, object/callback/conditional entries and unknown types cannot be misinterpreted as subtractive permission evidence.
- Generic masks map to file rights before policy comparison. The mutation mask includes data/append/EA/attributes, delete-child, delete, DACL control and ownership. Private rejects all outside nonzero effective grants; IntegrityProtected permits outside reads but rejects mutation. Inherited effective grants receive identical checks. Directory inherit-only grants are also checked because they can affect descendants. File inherit-only grants do not grant access to the present file. Some safe but unsupported ACLs deliberately refuse; the code does not claim to emulate AccessCheck.
- `PrivateDescriptor::new` at `windows_security.rs:622` keeps descriptor, ACL and SID allocations separately owned and stable. SID/ACL storage has DWORD alignment; the descriptor is boxed with its native alignment. Their allocations remain alive across `CreateFileW`/`CreateDirectoryW`. The explicit owner is the process user, the DACL grants only the three trusted principals, `SE_DACL_PROTECTED` is set, and handles are non-inheritable. New files use `CREATE_NEW`; existing locks opened with `OPEN_ALWAYS` are validated without being repaired. Thus the intended private ACL exists at creation, rather than after sensitive bytes are written.

No claim is made against the current user, SYSTEM or Administrators, preexisting mappings, or the OS. The source explicitly excludes those principals/mechanisms from its boundary. Newly conflicting writer/deleter opens are constrained by the selected sharing modes.

## Paths, sharing and identity

`path_chain` at `:162` limits paths to rooted local Disk/VerbatimDisk forms, rejects traversal/ambiguous stream and normalization components, caps the chain at256, and requires a fixed local drive. `inspect` at `:239` rejects non-disk files, mismatched directory types, all reparse attributes, deletion-pending handles and invalid sizes; it requires NTFS/ReFS with persistent ACL support and a nonzero128-bit file ID. Binding compares volume serial plus full file ID, never a timestamp/size substitute.

`pinned_parents` at `:511` acquires ancestors in order and retains prior handles while opening descendants. Those handles omit delete sharing and each ancestor's IntegrityProtected policy excludes outside mutation/reparse-control grants. `open_pinned_directory` additionally checks the complete binding before returning. File construction verifies held security and identity against a fresh current-path handle before exposing the result; bounded reads repeat binding/security checks after reading from the same file handle.

`Sharing::ReadOnly` permits only read sharing. Lock handles permit read/write sharing for OS lock contention while excluding delete sharing. Stage handles permit delete sharing for the guarded rename and require a Private immediate parent. The proof for stages therefore includes that retained private parent and the stated trusted-principal boundary; it is not a claim that the stage handle itself prevents renaming.

The policy intentionally validates every ancestor to the drive root. An outside create-child grant on any ancestor, or an owner outside the three trusted SIDs, refuses even if the ultimate file would otherwise be usable. This can reject ordinary usable Windows paths. Report such refusal truthfully; do not work around it by skipping ancestors, repairing shared ancestors, or describing the helper as supporting every local Windows layout. No native filesystem sample was examined here.

## Required root integration conditions

1. Retain `CheckedFile`/`DirectoryGuard`, or the complete guards returned by `into_parts`, through the authority-bearing operation. `into_file` at `:97` intentionally drops its guards; use it only for a stage whose owner already holds the same private parent chain. A temporary validation call does not pin a later independent pathname open.
2. Route actual protected reads through `read_protected` or the held checked file. Route private creation through the creation-time descriptor helpers within the existing injected-I/O boundary. Keep public WIKI reads distinct. Existing arbitrary user files must not be silently repaired.
3. Keep persistent run/writer lock handles and their ancestor guards alive while using their permits, and recheck binding when required by the caller contract. `validate_same_file` checks a momentary binding but drops its temporary guards on return.
4. Keep `inspect_directory` at `:722`'s result meaningful: inspection can fail; success returns `Unsupported`, never `Supported`. Paid authority requiring directory-entry durability must remain unavailable before send. P14 can separately honor its weaker, honest-partial export contract without inventing a crash-durability claim.

The helper's module-level allowance of unused entry points in the standalone harness does not exempt the integrated crate from its own lint or call-site checks. No helper-only review closes these integration conditions.

## Evidence and limits

Inspected `/private/tmp/lwiki-windows-security-checks.json`, all listed final logs, and the harness's actual manifest/source. Current source and harness SHA256s match that record, including its equal pre/post source hashes. The harness includes the real helper/policy by path and stubs only the two-case `DirectorySync` enum; it does not exercise root routing.

- Portable actual-policy tests: **7 passed**; malformed extents/SIDs/flags, all255 unsupported ACE types, null/empty DACL, foreign owner, inherited grants, outside rights and generic masks are covered.
- Actual cached `windows-sys0.61.2` source checks: **x86_64-pc-windows-msvc and x86_64-pc-windows-gnu passed** under Rust1.98.0.
- Standalone strict Clippy and formatting checks passed. Harness `allow(dead_code)` is explicit because there are no production callers in it.

No additional local policy regression is required by a defect from this review, because none was found. Focus later native fixtures on creation-time ACL inspection, impersonation refusal, existing insecure object refusal without repair, outside-principal grants, read/write/delete sharing conflicts, two lock contenders, reparse/ancestor replacement, and stage rename while parent guards remain held. Cross-checks establish API/cfg compatibility; portable tests establish parsed policy decisions. Neither establishes these native effects or crash safety.

```text
src/vault/windows_security.rs 2746d5ce193aee01c3af95db227b3a2b5a168bbf6a924d189aa1a4fed3a57a2c
src/vault/acl_policy.rs f6599a05ee449eeddf91ab41764cc9084e9b2ba27db1d73fa75cd1f46ba30875
tests/windows_acl_policy.rs dbbae84d4bebf5f9d8ef8b8ef1cafc830c55d7604723b4956b1be48b7646399e
```

Root retains integration and package acceptance. Report lease returned.
