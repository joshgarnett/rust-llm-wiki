# P14 export path-authority adjudication

2026-09-28, Astra bounded independent static review. Read P14-plan, P14-review, and `src/cli/skill_export.rs`; no Cargo, tests, source changes, nested delegation, or general skill review. Only this report written. Root reports the mandatory four packaging gates and independent forward exercise passed; this review did not rerun or independently inspect those runtime artifacts.

**Disposition: fix the ancestor race before accepting the current explicit-output/no-symlink contract.** A stable, owner-controlled output tree is a reasonable residual condition for coherent concurrent edits, but it does not justify replacing the written prohibition on symlink paths with an unchecked assumption. The fix need not provide whole-tree atomicity or defend against arbitrary modifications by the same owner after export.

## Concrete source mechanism and consequence

P14-plan says: “Refuse unsafe/symlink paths” and “Every new directory/file is exclusive; never overwrite existing bytes.” It promises an explicit export project directory and honest partial output. It does not limit symlink refusal to a preflight instant.

`safe_ancestors` at `skill_export.rs:21` checks pathnames using `symlink_metadata`, then releases all observation state. `directories` later calls pathname `create_dir` at `:61`. The export loop checks ancestors at `:264`, then opens the full pathname at `:268`. A concurrent process can rename the checked `references` directory and replace its name with a symlink to another writable directory between those last two operations. The exclusive open follows that ancestor and creates a previously absent `commands.md`, `examples.json`, or `workflows.md` in the other directory. Replacing an earlier ancestor can redirect directory creation and the full discovery layout too.

This needs permission to mutate an output ancestor; merely controlling text inside a skill resource does not trigger it. It does not defeat the existing leaf's exclusive-create protection, and the exported bytes are release content rather than secrets. Its definite consequence is creating files outside the destination the command was authorized to use. A later verification error cannot undo those writes, and the reported `created_paths` can name the substituted lexical path rather than the directory object actually written.

Reuse verification has the same problem: `read_dir` at `:148`, recursive descent at `:172`, and open at `:185` reopen names. Unix `O_NOFOLLOW` at `:183` protects the final file component only. A directory swap can make verification inspect another tree and claim the original package is complete. The corrected unique component membership logic is useful and separate from this race.

Neither another `symlink_metadata` immediately before opening, canonicalizing the full path, post-write validation, nor an advisory lock on a pathname closes this mechanism. Those approaches leave another check/use window or require cooperation from the process doing the replacement.

## Minimum targeted implementation

Introduce an exporter-private directory capability, implemented per platform, with operations to open/create a single child directory, exclusively create a file, enumerate children, and open a bounded regular file for verification. Each operation takes one validated component and acts relative to the held directory, or uses an equivalently pinned Windows path. Keep package order, exclusive files, no overwrite/replacement, and manifest-last behavior unchanged.

On Unix:

1. Acquire the output directory through a component-by-component walk from a held root/current-directory descriptor. Reject `..` and symlink directory components. Open each child with `openat` plus `O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC`; keep the output/package/subdirectory descriptors needed by subsequent operations. Resolve a relative output against the initially held cwd, not a later ambient cwd.
2. Create missing directories using `mkdirat` on the held parent. Treat a racing `EEXIST` as the current implementation's conflict, then open successfully created directories without following symlinks before proceeding. If a just-created directory disappears or becomes a symlink, stop with honest partial output.
3. Create each fixed-name file with `openat(parent_fd, leaf, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, mode)`. Write and sync through that returned handle. Do not reconstruct a full pathname to perform the actual I/O.
4. Enumerate and verify relative to held directories (`fdopendir` on an independently owned descriptor or an equivalent reviewed wrapper); open children without symlink following, inspect the opened file, and retain existing byte bounds/exact membership checks. The same path applies to dry-run reuse validation.
5. Before returning a lexical path as complete/reused, compare its non-following component identities with the held objects and refuse an observed binding change. This detects observed namespace changes; it is not a claim of an immutable namespace after return. Do not clean up partial output through names that may have been substituted.

A renamed held directory on Unix remains the originally opened directory object. Continuing an operation on that object does not follow the replacement symlink. Root should explicitly retain that object-based authorization model: it is implementable without pretending that a process can freeze all future same-owner renames. Check bindings between phases and before publication, and report a changed path instead of claiming complete output at an observed wrong binding.

On Windows, final-component `FILE_FLAG_OPEN_REPARSE_POINT` alone is insufficient. Acquire the directory chain incrementally with `FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT`, reject all reparse-point attributes, and hold ancestor handles without delete sharing through descendant operations. Non-delete sharing prevents the rename/delete substitution described here; use held identity checks and revalidate reparse state. New files remain `CREATE_NEW`, handles non-inheritable. Paths reopened beneath the pinned chain must refer to those same held ancestors. Directory reparse mutation and permissive/untrusted parents require the existing Windows protection module's policy, not an unqualified claim that one flag freezes every object mutation. [Microsoft CreateFileW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew)

Do not route Windows back to the current static checks and label it safe. A platform adapter that cannot establish the necessary path authority should fail explicitly before writes; that is an honest unavailable capability, not completed cross-platform export support. Root can integrate the separately leased Windows helper where its actual contracts suffice. Directory crash durability remains outside this P14 race fix, since P14 does not promise it.

## Definite regression gates

Use a private deterministic hook/barrier at the boundary between directory acquisition and first child operation; avoid probabilistic rename stress as the only evidence. Hooks must not become public runtime authority.

- **Creation redirection:** after acquiring `references`, rename it and install a symlink at its old name to an outside sentinel directory. Assert that no file or directory is created through the replacement symlink, existing sentinel bytes/mtime remain unchanged, and an observed binding change cannot return `complete:true`. Writes, if any, may exist only in the originally held directory and must be reported as partial.
- **Missing-directory redirection:** pause before creating a nested package directory, swap its checked ancestor, and assert no redirected directory/file appears. This exercises `mkdirat`/pinned-parent creation, not just file opening.
- **Reuse verification:** replace an acquired ancestor with a symlink to a complete matching package during verification. Assert it cannot validate the substituted tree and return reused success at the changed binding. Include the dry-run branch and verify no writes.
- **Windows equivalent:** where native execution is available, directory replacement should fail with the handles held or the exporter should refuse; a preexisting junction/reparse component must refuse. A cross-build only verifies cfg/API integration, not these native outcomes.

Rerun the already required packaging gates after the targeted source change. Existing layout, host preservation, alias regression, and workflow successes establish their tested behavior; they do not exercise these deterministic namespace transitions. No additional whole-tree transaction, recursive cleanup, global host installation, or unrelated storage redesign is requested.

Reviewed SHA-256 snapshots:

```text
src/cli/skill_export.rs 5bca7ae5344987a1e605bafbf763ce2b477aded6d59b60d090df675cc810892c
docs/execution/reports/P14-plan.md 463e7fe6ed7841848567fb5f1b45b2256f2b56f70e2a4ad4ae9116f8e1a5b167
docs/execution/reports/P14-review.md ab208094141a9a838fedfcb19050aed631c594245110cc3befea602ce16ae064
```

P14 storage acceptance remains blocked on this narrowly identified mechanism. Root owns implementation and final acceptance. Report lease returned.

## Anchored implementation rereview — static candidate

2026-09-28. Reviewed only the new backend, common verification/partial-result routing, six private deterministic tests, and unchanged Windows helper call contracts. No Cargo, source edits, test execution or maintained skill-content review. **The original Unix ancestor-redirection mechanism is closed in source. One Windows creation-error reporting issue remains before whole-backend acceptance.** Targeted runtime gates were still in progress when this static disposition was sent to root.

On macOS/Linux, `skill_export.rs:445` opens children relative to retained parent descriptors and always adds `O_NOFOLLOW | O_CLOEXEC`; directory opens also require `O_DIRECTORY`. Missing directory creation at `:602` uses `mkdirat`; file creation at `:625` uses descriptor-relative `O_CREAT | O_EXCL`. The creation is recorded before post-creation checks, so an observed moved parent leaves an honestly reported partial in the originally held object. No path-based cleanup is attempted. Enumeration at `:701` opens `.` on the held descriptor with an independent enumeration offset, transfers its descriptor to `fdopendir`, and closes it exactly once; names are copied before the next `readdir`, checked, and count-bounded.

`acquire` at `:487` captures a relative destination's cwd handle and compares it with the component-wise pinned path before use. Every directory retains its parent/name/identity chain. `verify_binding` at `:558` checks the held identities and each non-following parent-relative name. Verification reads at `:659` compare the opened regular file's identity and size, read only the expected bounded bytes, and retain the file plus its parent. `FileGuard::verify_binding` at `:373` compares current identity and the original size/mtime/ctime stamp. Common return routing at `:277` and `:312` checks retained directory/file guards after the final hook. Thus reuse cannot silently validate a substituted matching symlink tree, and an observed final file/directory binding change refuses success. This does not assert atomicity against arbitrary concurrent content changes after the last check.

The six private tests at `:1140`–`:1302` use actual private callbacks at operation boundaries, not timing-dependent stress. They cover a file-parent swap, a missing-directory ancestor swap, reuse and dry-run matching-subtree swaps, final complete-package swaps, static symlink/FIFO/exclusive-collision refusal, and zero-write dry-run/manifest-last controls. Outside tree snapshots include bytes and modification times; callbacks assert they fired. The file-parent swap verifies the anchored empty file exists only in the renamed original directory, while the missing-directory swap verifies creation only in the held original parent. These are meaningful regressions for the mechanism previously reported. They do not supply Windows runtime evidence.

The Windows bridge retains `Arc<DirectoryGuard>` plus parent chains and retains `CheckedFile` for final verified-file checks. Its helper validates the complete ancestor chain and excludes delete sharing, so the remaining pathname-based enumeration/opening is performed under actual pinned ancestors, not the earlier stat-only preflight. Existing reparse/unsupported roots fail through the conservative helper; new objects use creation-time private descriptors. Native Windows behavior and full integration still require their stated separate checks.

### W1 — medium: Windows post-creation errors can hide a partial export

At `skill_export.rs:925` and `:945`, `created_paths` is updated only after `create_private_directory`/`create_private_file` returns `Ok`. However, the reviewed helper performs successful physical creation and then fallible validation before returning:

- `windows_security.rs:714` calls `CreateDirectoryW`; `:716` pins/inspects the result and `:718` verifies its binding.
- `windows_security.rs:692` opens a new file; `:700` calls `checked_file`, which performs several fallible identity/security/binding queries.

A query/resource failure after the first successful `CreateDirectoryW` leaves that new directory behind but leaves `created` empty. Common error output at `skill_export.rs:330` consequently reports `partial_export:false`. Later post-creation failures can omit real created objects from its claimed definite creation list. This violates the required honest-partial contract, although the anchored authority protection remains intact.

Root should preserve physical creation outcome across the helper's post-creation failure boundary: for example, an internal structured error/outcome carrying the successfully created object's requested name, or a creation callback invoked immediately after success before validation. An alternative is explicitly conservative `possibly_created_paths` and an error message saying partial output may remain; do not label speculative paths as definite creations. Do not attempt cleanup through a substituted pathname. This is a root-owned helper/caller integration change, not a newly identified unsafe-memory defect in the helper itself.

Narrow regression: inject failure immediately after successful physical directory creation, before its first inspection, with no earlier export creations. Require an error, `complete:false`, an honest partial/may-remain indication and a retained created directory; require no removal/overwrite/outside redirection. Repeat for a new file after earlier valid directory setup. A platform-neutral outcome/reporting seam can test this locally; only a native Windows fixture can exercise the actual API-side failure boundary. Current six Unix tests do not cover it.

Static candidate SHA-256s (private race tests are inside the exporter source):

```text
src/cli/skill_export.rs 57827082609d0b9c61e35d9dbad472dd1172be8277f66e0621906e1951c06e93
tests/skill_export.rs b8adb08a0207c7877918296a3a4c784708ac5b5fc6148e485ace4a6462eb3184
src/vault/windows_security.rs 2746d5ce193aee01c3af95db227b3a2b5a168bbf6a924d189aa1a4fed3a57a2c
src/vault/acl_policy.rs f6599a05ee449eeddf91ab41764cc9084e9b2ba27db1d73fa75cd1f46ba30875
```

Source closure is supported for the original Unix race; runtime attribution remains pending the worker's matching logs. W1 remains open. Root owns final acceptance.

## Closing rereview of W1 and runtime evidence

Root and worker corrected W1 during this review. **W1 is now closed in source, and no remaining concrete defect was found within the requested anchored-backend scope.** This supersedes the open W1 disposition above.

`windows_security.rs:700` wraps a failed post-open validation only when the physical disposition was successful `CREATE_NEW`. A private `CreatedBeforeFailure` error preserves the original error kind and source; its public(crate) predicate exposes only the observed creation fact. Pre-creation errors bypass that wrapper, and ambiguous `OPEN_ALWAYS` does not claim a new object. Directory creation wraps only the validation closure after successful `CreateDirectoryW`. The exporter at `skill_export.rs:929` and `:955` records the definite created path before translating/propagating a marked error; successful paths still record once. No cleanup or speculative creation assertion was added. This establishes truthful partial reporting for the identified boundary. The actual Windows post-creation fault branch remains statically reviewed, not natively fault-injected.

The Unix C variadic `openat` mode argument now explicitly uses the promoted `libc::c_uint`. Final-binding tests additionally cover substitutions of the complete package, a verified subdirectory, and an already verified leaf in both reuse and dry-run branches. The retained file guard, not merely a final directory check, detects the leaf replacement.

Completed logs independently read:

- `/private/tmp/lwiki-p14-anchored-private-third.log`: **6 passed**, compile2.64s/run0.52s; actual macOS descriptor-relative race/control tests.
- `/private/tmp/lwiki-p14-anchored-public-first.log`: **4 passed**, compile11.24s/run8.25s; the four mandatory packaging parents, including the executable workflow.

Both logs identify isolated snapshot `lwiki-p14-acceptance-1hnkvjrp`. The following current workspace files match that actual isolated snapshot byte-for-byte. The worker's final freeze inventory and any additional platform cross-checks were still pending at this observation; the older `lwiki-p14-final-checked-hashes.json` names the pre-anchor exporter and must not be used as the new candidate's inventory.

```text
src/cli/skill_export.rs e6ac605c3115a952663663b09dbd66dee7a0d8dd59519bf093a5efb8fdb4ee21
tests/skill_export.rs b8adb08a0207c7877918296a3a4c784708ac5b5fc6148e485ace4a6462eb3184
src/vault/windows_security.rs 93f0df13d9fd48c178e05796233dc1e9ac6dd21ec3c419112b9b39799b26d686
src/vault/acl_policy.rs f6599a05ee449eeddf91ab41764cc9084e9b2ba27db1d73fa75cd1f46ba30875
```

The previous helper ACL/unsafe review remains applicable; only its creation-error marker and the exporter consumption were rereviewed here. Native Windows path sharing/reparse behavior remains unqualified. Root retains final inventory reconciliation, platform gates and package acceptance. Report lease returned.

## Final source-delta reconciliation

Reviewed the subsequent narrow source deltas; the prior closure stands. Private `Event::Before*` names became `Event::*` at the same acquisition/operation/return boundaries, and the callback alias remains test-only. The scoped Clippy cast allowances preserve libc device/inode width conversion and the required variadic mode promotion. Windows `io::ErrorKind::Unsupported` now maps to `CapabilityUnavailable` without a pathname fallback. Removing the unused Windows identity getter changes no held-handle, guard, creation-marker or binding behavior. No new blocker found.

Final source observations, matching the actual isolated snapshot byte-for-byte:

```text
src/cli/skill_export.rs 59ae93f8cb34df19ea1a9367a3539e3583b6bfc2821abf2b59fa55b8536da838
tests/skill_export.rs b8adb08a0207c7877918296a3a4c784708ac5b5fc6148e485ace4a6462eb3184
src/vault/windows_security.rs 785baab120bf4b89594231f73b9fb0da458a061bd093d563e948291efb36f274
src/vault/acl_policy.rs f6599a05ee449eeddf91ab41764cc9084e9b2ba27db1d73fa75cd1f46ba30875
```

The worker's `/private/tmp/lwiki-p14-anchored-checked-hashes.json` records its preceding exporter `7eac1eb9...`; the root lint deltas produce the final `59ae93f8...` above. Root must retain the final inventory under that final identity rather than relabel the older worker manifest.

Inspected completed root logs: `/private/tmp/lwiki-p14-root-anchored-final.log` has **6 passed**, run0.50s; `/private/tmp/lwiki-p14-root-public-final.log` has **4 required packaging parents passed**, run8.31s, plus **7 ACL policy tests passed**. Root formatting, strict all-target Clippy and build logs also finish successfully, with command/exit metadata in `/private/tmp/lwiki-p14-root-checks.json`. That JSON's earlier `cli::skill_export::tests` filter is not evidence for the six anchored tests; the separate correctly targeted anchored log supplies it. Earlier worker final-batch/integration logs likewise distinguish six matching private tests from filtered-zero integration targets and the separate actual4+7 integration execution.

This final reconciliation neither runs tests nor expands the previous native/cross-build qualification limits. No outstanding finding remains from this bounded review. Root owns the final inventory, platform gates and package acceptance. Report lease returned.
