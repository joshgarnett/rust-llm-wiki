# P03 independent storage review

2026-09-28. Reviewer: GPT-6-astra. Pass A only; full P03 acceptance remains pending pass B and integration checks. Review scope: P03 contracts/invariant analysis, retained preparation, protected journal, shared changes types, manifest schema, and preparation/journal tests. Accepted P00–P02 history was not reopened. No Cargo commands, runtime reproductions, code changes, or external qualification were performed by this reviewer.

## Pass A finding

### A1 — P2: interrupted pre-manifest preparation poisons future origin lookup

- **Paths:** `src/changes/prepare.rs:165` (payloads precede manifest), `:224` (unconditional inspection of every directory), `:304` (directory enumeration), `:329` (missing manifest is an error).
- **Trigger/minimal reproduction:** inject a preparation failure or terminate after creating `changes/<new-id>/proposed/00000.md`, before persisting `change.md`; restart the engine and invoke `prepare_or_reuse` for any origin. A fixture with that leftover directory/file reproduces the same retained state without changing a canonical target.
- **Expected:** distinguish recoverable incomplete preparation from a retained, potentially applied change; preserve orphan bytes, do not infer apply intent, and allow a safe unrelated preparation/retry through the documented recovery policy.
- **Actual (static trace):** `change_ids` returns the orphan ID; `inspect` immediately returns `missing retained manifest`; `prepare_or_reuse` propagates it before reaching `build_once`. Subsequent originated preparations remain blocked indefinitely. The normal pre-manifest durability failure path produces this state, without corruption or external interference.
- **Required resolution:** establish an explicit interrupted-preparation representation/recovery policy that cannot accidentally ignore missing retained state belonging to an applied change. Extend the injected preparation matrix to restart and exercise recovery plus origin lookup, preserving targets/payloads. Do not simply skip every missing manifest, since deletion of a real manifest must still fail closed.
- **Evidence limit:** static finding, not a run reproduction. Existing `actual_prepare_durable_io_failure_matrix_never_mutates_targets` checks target preservation and error propagation but does not restart or retry origin lookup.
- **Root response:** confirmed; correction pending. Root selected preservation/diagnostic exclusion only for directories lacking `change.md`, operational journal, terminal receipt, and validation receipt. Missing manifest with any such authority/baseline artifact must return RecoveryRequired. Existing malformed manifests remain errors. Permanent restart/origin-retry coverage is requested; no pruning is authorized by this fix.

## Pass A checks and pending boundaries

Static inspection found the exact payload/fence hashing, owned generated payload paths, duplicate-key rejection, bounds, dependency ordering, no-op dropping, read-only planning, terminal historical target exemption, protected framing, contiguous sequence/binding, D22 recovery epochs, and truncate-plus-sync-before-append consistent with the intended design. Independent header checksum prevents ordinary damaged length bytes from masquerading as a torn body; complete corrupt frames fail closed.

Prepare/journal/types/tests SHA256s exactly matched P03-A handoff (`acd0309e…`, `659a7b56…`, `0e78e664…`, `a3c1dd46…`). Recorded worker results are 15 preparation/journal tests and library Clippy before later changes; the sixteenth test and receipt/topological/reserved-case edits await root's affected rerun. These are worker claims, not reviewer-run checks.

Pass B must verify sealed-tree ownership at application/recovery (A only checks tree absence at initial planning), retained terminal receipt proof/precedence, immutable baseline/read dependencies, application durability boundaries, rollback, and the publication capability. Exact-case revision-path recognition also merits checking against B's canonical path policy. No native Linux/Windows, power-loss, SQLite isolation, or live-provider claim follows from this review.

## Pass A1 correction review

Disposition: **resolved**. `classify_change_directories` now separates preserved incomplete preparations only when the note and every operational-journal/outcome/validation artifact are absent. An authority/baseline artifact with a missing manifest produces RecoveryRequired; malformed existing notes, symlinks, and failed bounded reads still refuse. Read-only `incomplete_preparations` exposes diagnostic IDs. No cleanup or mutation is hidden in this classification.

The preparation fault matrix now restarts with NativeIo after each injected failure, recovers without permitting validation/publication, retries a new origin exactly once, and verifies that prior file bytes and orphan IDs survive. Separate cases cover all three authority artifacts, malformed notes, and receipt symlink escape. This directly addresses A1; tests were inspected, not run by the reviewer. Reviewed correction hashes: prepare `078f6a098351eab3f1f575a52cf69bc3ae8fb08722c751bd79c8b3ebd6717d77`; tests `2ba56ffdbad14eaba3c32ef6d658b0b04517790ee8ed9b73a3b485360e5e1767`.

Root's matching-fingerprint `P03-A-checks.json` records `cargo test --locked --offline --test changes_prepare_journal`: **17/17 passed**, 24.79s on macOS 26.5.2 arm64. This supersedes the pending-A-test qualification above. The reviewer inspected that evidence record; this remains a root-run result.

## Pass B findings

### B1 — P1: application can extend an independently sealed revision tree

- **Paths:** `src/changes/apply.rs:58` and `:80` (target-only preflight), `src/changes/prepare.rs:659` (directory absence enforced only while initially preparing).
- **Trigger/minimal reproduction:** prepare two changes while `sources/source_s/revisions/revision_r/` is absent, one creating `original.bin`, the other creating `content.md`. Apply the first, then the second. Both target expectations are individually satisfied; the second proposal's target remains absent. An unrelated actor creating an unplanned binary member after preparation creates the same gap.
- **Expected:** engine policy must prevent extension of an already sealed revision, independently of graph callers. Trusted interrupted application may resume only its own manifest's proposed asset set, with member/hash checks and no competing sealed owner. Initial preparation alone cannot establish this at application time.
- **Actual (static trace):** manifest loading calls `infer_role` with `preparing=false`; `continue_apply` observes only operation targets, scans canonical Markdown, and never checks complete revision-directory membership or ownership. It writes the second asset and publishes if the injected validator accepts the graph. Binary extra members are outside the canonical scan. Recovery has the same gap.
- **Required resolution:** engine-owned revision namespace/ownership/member preflight for apply and recovery, allowing exact interrupted own assets while rejecting extra/unplanned/third-hash members and competing sealed manifests. Include portable namespace casing. Add sequential independently prepared proposal and interrupted-recovery regressions; preserve all existing bytes on refusal.
- **Root response:** confirmed blocker; bounded correction planned after this frozen review. No runtime reproduction was run by the reviewer.

### B2 — P2: known Prepared proposals still require readable obsolete targets to abort

- **Path:** `src/changes/rollback.rs:34`.
- **Trigger/minimal reproduction:** prepare a create at `future/page.md`; then create `future/WIKI.md`, making the target belong to a nested vault. Call `abort` with the original Prepared handle. A target replaced by a directory or symlink also triggers this.
- **Expected:** the authoritative Prepared journal proves no apply started; abort can finalize only its retained bookkeeping without consulting stale target paths. Missing-journal abort must retain its all-old/containment checks.
- **Actual (static trace):** unconditional `observe` fails before the `state.status.is_none()` guard, so a safe known-Prepared abort cannot complete. The existing stale-abort test covers changed file hashes, not changed target kinds or scope.
- **Required resolution:** observe targets only in the missing-journal branch; add a stale target kind/nested-vault regression and keep missing-journal refusal coverage.
- **Root response:** confirmed; correction and regression requested before acceptance.

## Pass B evidence and limits

Reviewed all four B leaves, shared interfaces, and recovery tests; their SHA256s match the frozen P03-B handoff. No reviewer Cargo commands were run. Static checks support retained whole-projected-scan/read-dependency baseline before Applying, proof resync, full old/new/third target preflight, completion-independent recovery epochs, guarded production I/O, already-new target sync, private publication capability after revalidation, unavailable-backend refusal, snapshot fingerprint binding, exact outcome-note guards, complete terminal receipt transcript validation plus strict surviving journal prefix, historical terminal exemptions, and guarded inverse operations retaining immutable captures.

The sole ignored test is the explicit subprocess child harness. Its enabled wrapper invokes `--exact crash_child --ignored`, kills after a real canonical replacement, then recovers under a fresh permit. Required fault coverage is enabled. B's matrix reaches the mock publisher's actual VaultFs stage/replace boundary, but the mock catalog is not SQLite and does not prove transaction isolation. Existing reported results remain 14 quick tests passed with the required full matrix filtered; an earlier 400-case matrix passed before subsequent B edits. The final frozen matrix is still pending root, and neither static review nor mocks establish power-loss, disk-full, other-platform, or live-provider qualification.

## B1/B2 bounded correction re-review

**B1: resolved in reviewed implementation, subject to the final integration gate.** The private immutable module establishes a checksummed manifest-bound inventory after durable validation and before Applying. Its claim binds the exact original validation receipt bytes. Existing ownership requires matching proof and resync; missing baseline cannot be silently recreated when the claim survives. Prepared proposals refuse existing trees. Trusted interrupted applications and explicitly authorized journal-loss application accept only their verified inventory, planned directories, and reserved UUIDv7 stage artifacts containing prefixes of retained proposed bytes; these artifacts do not add canonical members. Competing active/sealed owners remain blockers, including retained terminal proof after journal loss.

Complete tree checks now surround operation mutation and completion and precede FilesApplied/publication authority; the post-publication check detects new interference. Extra members and third hashes remain intact and produce durable Conflict for active applications. Root's aligned preparation-role, source-dependency, scanner, and projected-scan changes consistently fold only namespace words, preserving exact source/revision IDs. The orphan classifier now also treats `revision-trees.json` as authority evidence. No accepted-history re-review was performed.

**B2: resolved in reviewed implementation, subject to the final integration gate.** `abort` only observes canonical targets when the journal is missing. Known Prepared aborts operate on retained bookkeeping. The new regressions separately exercise directory, symlink, and nested-vault targets for successful known-Prepared abort and refusal with missing intent.

No new implementation blocker was identified in this bounded pass. Tests inspected cover independent same-tree proposals, folded namespaces, own partial replay, extra members before apply and during final validation, original-baseline loss, retained committed owner after journal loss, and abrupt termination after empty native revision-stage creation. The expanded required fault matrix now includes immutable binary creation, exercising ownership-claim I/O as well as ordinary changes; it remains **unrun on this final tree**. Worker reports 23 quick tests passed, one explicitly invoked child helper ignored, and the required expensive matrix filtered (11.75s), plus library Clippy and leased-file rustfmt. These are recorded worker results, not reviewer-run checks.

Evidence refinement sent to root: the current tests explicitly delete `validation.json`, but do not directly delete, corrupt, or misbind an existing `revision-trees.json`. Those refusal paths were inspected statically; no runtime qualification of those particular claim failures is asserted. A focused claim-integrity regression is recommended before the final gate.

Reviewed final fingerprints:

```text
e0714cc9158f2fa5b49954db9338ab584773700a9a4d2776b6596822d978cbc9  src/changes/apply.rs
05dbfb5d42bfcd6c54a1b152179891a9432a4aec1fa3a0b9dc92eb48caafc6f0  src/changes/immutable.rs
6da421ed075e37d5f2a62e0cf348c54a38540aa38120574a23e0ea6c02e4f4a3  src/changes/rollback.rs
914c0fa43b3d3bf54a9d752d41e3db592763f1388b25bd0f15eb615f7c4a1ac7  src/changes/prepare.rs
c77c110fa3d6a833ed44507f6fb19ea049d8dfcc2b687c810012905962ed6996  src/vault/paths.rs
cdda54334f622b6145f61f08216dd695b636907b8cbac31955682a6b9da412c9  tests/changes_recovery.rs
d140d8aa11bbfc8675913fc0cd9bc8d76d7a50fb9e9e106c6aa7394105e1bab1  tests/changes_prepare_journal.rs
130d4661dba2dd7963596dd8590be672a9c3c4fc0366bfb6af0be700a5e83a9e  tests/vault_fs.rs
```

Earlier A integration evidence predates these namespace/authority-artifact changes; root's final all-targets gate must cover them. Full P03 acceptance still requires that gate and the expanded fault matrix. Actual SQLite publication/isolation, power-loss, disk-full, and native other-OS qualification remain unproven here.

## Root final gate disposition

After handoff, root added and executed the direct missing/corrupt/misbound ownership-claim regression, then froze the code. All73 parent tests passed, including17 preparation tests,25 recovery tests, both enabled native SIGKILL wrappers, and the expanded250-boundary/500-case matrix. Full all-target Clippy -D warnings, fmt, debug build, diff check and seed fixture check passed. Exact tested hashes/results are in P03-checks.json. A1/B1/B2 are accepted as resolved. This is root-run evidence; it does not change the reviewer static/runtime distinction above. P05 SQL and external qualifications remain pending.
