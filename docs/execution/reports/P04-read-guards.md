# P04 retained read authorization review

2026-09-28; independent storage specialist, narrow static review. Accepted baseline supplied by root: `74cfb5838d32f1889bb8a094bc7fe0643390d85e`; source leaf implementation was in progress. No Rust changes, Cargo invocations, fault injection, or live tests performed. This report does not reopen the accepted P03 review.

## Finding

`src/sources/evidence.rs::plan_revalidate` checks the caller's expected predecessor hash, verifies its citation, and places that dependency in `EvidencePlan.dependencies`. Its draft only creates the successor. `ChangeDraft`/`ChangeManifest` have no read-precondition field. A caller passing `plan.draft` to preparation therefore loses the predecessor authorization. An intervening explanation-only edit, or structurally valid stance edit, can pass future whole-graph validation while violating the earlier `--if-match`. `src/changes/apply.rs::retain_validation` first captures graph dependencies at application, too late to reconstruct that authorization. This is a statically demonstrated contract gap, not a reproduced runtime result.

## Recommended minimal fix

Adopt root's proposed `read_preconditions: Vec<ReadDependency>` in `ChangeDraft`, `ChangePlan`, and `ChangeManifest`. Define these as **original filesystem observations**, not general projected-graph dependencies. Serialize the manifest field only when nonempty and deserialize omitted fields as empty, retaining v1 compatibility and unchanged encoding for old manifests. The immutable manifest hash binds the list, as it already binds operations. Do not rewrite existing manifests to add guards.

Preparation should validate all guards before retained allocation: bounded count, valid contained portable paths, bounded raw-byte hashes or explicit absence. Sort/deduplicate exact duplicates when constructing a draft, reject contradictory duplicates, and require strict sorted unique form when reading retained manifests. Keep strict unknown-field and duplicate-JSON-key rejection; add the optional property and strict dependency object to `schemas/change-v1.json`.

Normalize a guard overlapping a retained write only when it equals that write's `expected`/`before` state. The write then carries the authorization. Reject a different state rather than interpreting it as permission to refresh the original expectation. **After no-op writes are dropped, requested guards on those paths must remain.** Retained external guards should be disjoint from retained operation targets. These rules require no before/new authorization arrays.

Attach original-read dependencies to the draft inside each source planner before returning it, including the predecessor dependency added after `build_evidence` in `plan_revalidate`. Keeping the top-level dependency list for diagnostics/citation consumers is harmless, but it cannot be the sole correctness channel. Passing only `plan.draft` must be sufficient.

## Overlay and binary distinction

`SourceView`/`VerifiedCitation.dependencies` can describe proposed bytes, including binary originals and content snapshots. They must not be blindly copied to original-state guards. Current `SourceStore` plan methods construct a filesystem view, so direct attachment is valid there.

If a future planner uses an explicit overlay, its binder must distinguish base reads from proposed reads. Base reads become original-state guards. A proposed dependency must exactly match the same draft's proposed bytes/path (including explicit deletion), with the operation's original expectation still checked normally; retained `after`/payload verification then binds the proposed read. An unrelated overlay cannot supply authorization. If no matching write survives because it is a no-op, preserve the equivalent original-state guard. A conflicting or unclassifiable dependency must fail, never be overwritten by map insertion. Production graph-validator dependencies remain projected-graph dependencies and retain their existing separate verification channel. None of this adds an implicit staged-overlay CLI workflow.

## Application, recovery, and history

Check external guards before `Applying`, immediately before each mutation, before publication, and before final completion; re-use these checks on forward recovery. Prepared mismatch returns `CONTENT_CONFLICT` without activation. Mismatch after durable applying intent becomes durable `Conflict`, preserves unfamiliar bytes, and denies publication. A retained guard failure must not be repaired by recalculating dependencies from changed files. Normal operation old/new recovery classification remains unchanged.

Manifest validation must remain structural, without checking present dependency bytes. Terminal committed/aborted receipts and journals keep their historical exemption: later legitimate edits to a dependency cannot make completed changes unreadable or prevent unrelated recovery. Stale prepared changes remain abortable. All-old proposals without durable intent remain staged; guard failure cannot grant applying authority. An inverse is a fresh plan against current state, not a copy of obsolete original guards.

Origin reuse must return the original retained proposal and its guard set, without replacing guards, rebinding hashes, allocating new IDs, or invoking a builder to refresh authorization. Terminal origin reuse must not require historical guards to match today's files.

## Exact affected paths and regression obligations

Root: `src/changes/types.rs`, `prepare.rs`, `apply.rs`, `recover.rs` as needed for staged diagnostics, and `rollback.rs` for fresh draft initialization; `schemas/change-v1.json`; `src/sources/types.rs` documentation. Source worker: `src/sources/capture.rs`, `lifecycle.rs`, `evidence.rs` planner construction/binding; `revision.rs` only if adding an overlay-aware binder.

Tests in `tests/sources_evidence.rs`, `tests/changes_prepare_journal.rs`, `tests/changes_recovery.rs`, and `tests/contracts.rs` should establish:

1. Revalidate, mutate predecessor explanation/stance, then prepare only `.draft`: conflict and no retained allocation. Repeat mutation after preparation: apply refuses with predecessor and successor bytes preserved as applicable.
2. No-op-drop retains requested guard; matching write overlap normalizes; differing overlap rejects; unguarded empty legacy drafts remain compatible.
3. Existing binary dependency tampering conflicts despite absence from canonical Markdown scan. Proposed binary payload dependencies bind to exact same-draft operations; unrelated overlay or wrong proposed hash rejects.
4. Interrupt after a target write, change untouched guard, recover: durable conflict and zero publication. Untouched guards plus old/new target combinations still resume normally.
5. A mutation-time injected dependency edit fails before the next write/publication. Guard-check failures after applying intent remain durable conflicts.
6. Old manifests lacking the optional field load unchanged. Unknown/duplicate fields, duplicate paths, invalid expected state, malformed paths, excessive lists, and manifest guard tampering reject.
7. Completed/aborted changes remain inspectable/recoverable after dependency edits; stale prepared changes remain abortable; origin retry retains original guards/IDs; inverse planning does not inherit obsolete guards.

No new CLI flag is needed: this makes existing `--if-match` survive staging. Record the new retained-field semantics in the root decision log and storage contract when integrating.

## Bounded implementation review

Reviewed the subsequently implemented original-state channel in `src/changes/{types,prepare,apply,rollback}.rs`, `schemas/change-v1.json`, `tests/change_read_preconditions.rs`, and source-planner guard attachment sites. Read-only inspection; no Cargo or reproduction commands run. Root reports the five targeted guard tests passing in 0.81 seconds and thirteen source tests passing; those are supplied results, not independently executed checks. Full P04 review and final all-target checks remain pending.

The implementation correctly keeps the optional empty field out of legacy serialization, carries the source dependencies inside `.draft` (including the late predecessor merge in revalidation), normalizes matching before-state write overlap, retains requested no-op guards, and verifies live guards before activation and around mutations/publication/completion. Mismatch becomes a durable conflict after applying intent; existing terminal-return paths preserve historical exemption, and inverse drafts do not inherit original guards. Rejecting duplicate input guards rather than deduplicating them is a safe stricter policy. Current public source planning uses original filesystem views, so the projected-overlay distinction is respected without an additional binder API.

### RG1 — Cross-set path compatibility is not validated (blocking)

`prepare.rs::plan` validates read paths and write paths separately. `validate_manifest` likewise validates them separately, then prohibits only exact cross-set equality. Both miss ancestor overlaps and case-folded collisions between the two sets.

Static example: initially absent read guard `branch/missing.bin` plus a create-file operation at `branch`. Each set passes independently. Application creates `branch`, after which the guard cannot resolve because its parent is a file; the post-mutation guard becomes a durable conflict. The plan was predictably incompatible before any retained allocation. The reverse shape (absent guard `branch`, write `branch/new.md`) similarly makes directory creation invalidate the guard. A second case, absent guard `New.md` and write `new.md`, aliases on case-insensitive filesystems and conflicts after mutation there.

After normalizing exact matching write overlap, validate the **union** of retained operation targets and remaining read guard paths using the existing portable/ancestor/case-fold structural rules. Perform the same structural union check in retained manifest validation without consulting current dependency bytes, preserving terminal exemption. Add plan/preparation regressions for an ancestor overlap and cross-set case-fold collision; reject before retained allocation. A retained-manifest union regression should also reject a structurally incompatible list even with a correctly recomputed fence hash. This finding is static; examples were not executed by this reviewer.

No other blocker identified in the bounded channel review. The additional dedicated origin/strict-wire tests remain useful integration coverage; no separate defect was found in those unchanged mechanisms.

### RG1 resolution — statically resolved

Re-inspected the root correction: `plan` now validates the union of retained write paths and remaining guard paths after matching overlap normalization, using `validate_targets`; `validate_manifest` validates the same union with `validate_retained_targets`, preserving structural-only historical loading. These checks cover cross-set ancestor relationships and portable case-fold collisions. The added `read_write_ancestor_and_portable_case_collisions_reject_before_allocation` test includes both reported shapes and asserts plan/prepare rejection plus no retained allocation or target creation.

RG1 is resolved by static inspection. No further blocker remains from this narrow review. The reviewer did not run Cargo; root's expanded eight-test guard suite and full package checks are still pending at this checkpoint. Root additionally reports tests added for original-guard origin reuse and post-publication edits; their runtime results are not claimed here.

## Root final gate evidence

P04-checks.json records96 parenttests passed onthe correctedfrozen tree, including15 source,8 originalreadguard,250boundary/500faults andenabledSIGKILLwrappers. All-targetlint/fmt/debugbuild/diff/seed checks passed. All findings resolved; acceptance doesnotclaim reviewer-run tests orP05SQL/externalqualification.
