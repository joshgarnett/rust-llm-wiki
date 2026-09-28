# P12 final independent invariant review

Independent Sol fallback: a fresh Astra review spawn was rejected by the runtime thread limit. Root retains separate invariant review and all gates. Accepted baseline is P15 `44f3da7368e52e070abc37a2871eb2af8da9fb8d`; P12 remains unaccepted. This reviewer writes only this report and runs no Cargo, source/test/Git mutation, provider/network or nested delegation.

Status: forward R1–R3 changed-source rereview closed conditional on actual tests. Core GraphDecide rollback extension remains a separate implementation requirement below. Authored tests are not runtime evidence.

## Reads and inspected invariants

Read execution README/STATE, selected P12 package, retrieval identity paragraphs, schema identity/supersession paragraphs, storage recoverable changes, D36–D38 and existing P12 review. Inspected changes types/apply/prepare, Catalog eligibility integration, graph decisions/remap/import/mention_state/resolution integration and P12 leaf/engine/CLI tests.

The engine-only RetainedGraphInput has private construction fields and no deserializer or Clone. Before/proposed payload path, declared byte length and BLAKE3 are checked; the witness imposes128MiB aggregate before allocating each payload. Initial and final validation retain actual ValidationInput.documents. Declared old states are reconstructed in a separate logical view; actual current reference membership is checked separately. Physical operation observations must be exact before or after states, and final exact membership/hash snapshots are rechecked around publication.

Exact authorized entity/assertion/extraction changes preserve author bodies and every unlisted frontmatter field. Assertion endpoint transitions bind invariant directed predicate/literal/negation/modality/unit/date fields; all changed endpoint fields require proofs. Accepted assertions reset to proposed; no factual acceptance follows remap. Existing acceptance is superseded only for its entire nonempty authoritative union changed by one governing operation, with explicit predecessor hashes/edges and status-only edits. Shared mention predecessor scopes require complete explicit carry-forward, including unchanged entity assignments.

The pure canonical policy is below projection/import restoration. Bounded binding/endpoint chains reject missing, contradictory/forked and cyclic successors; complete artifact maps, immutable raw/window/reserved allocations and materialized subsets remain checked. Full public receipt restoration separately invokes bounded extraction/source/quote proof. Current same-target merge families admit only disjoint absorbed scopes and exact independently current scoped mention assignments; alias retention is historical label authority, not current binding. Existing tests cover alias→merge/split, sequential merges, hidden endpoint edits with forged matching hash claims, real partial replacement/recovery with a newly introduced reference, shared predecessor partition and canonical-only acknowledgment after rename/prose/later status edits. These are source/test-coverage observations only.

## R1 — exact Decision write membership is missing at engine apply

At initial review, remap::verify_remap_overlay filters every proposed Decision out of retained operation equality (~1648–1665). verify_exact_changes proves requested predecessor edits, but does not reject additional unrelated Decision writes. decisions::verify_retained rejects such writes on acknowledgment, which is later than initial/final engine apply.

Minimal fixture: take a valid staged GraphDecide draft, add an unrelated valid Decision creation/edit that does not conflict, retain matching otherwise-valid allocated receipt copies, then prepare and apply directly through CatalogGraphValidator. The unrelated write is excluded from witness comparison. Require exact membership: all allocated main/replacement creations plus precisely declared main/mention predecessor status-only writes, exact paths/before/after bytes, no other Decision writes or deletions. Regression must exercise engine apply, not merely loader/reack. Sent to root/worker; pending hardening.

## R2 — unmatched retained origin silently grants an empty proof

The witness relevant-receipt filter requires GraphDecide task/request hash equality, but no matched receipt currently returns Some(empty authorization, canonical policy). An alias-only decision has no proposition change to trigger the later assertion-difference refusal.

Minimal fixture: valid alias receipt/changeset, but alter the GraphDecide origin task or request hash before preparation. The policy is otherwise valid; activation must reject the wrong origin rather than leave an unrestorable retained origin. Require exactly one matching receipt bound to operation/task/request hash and allocation map. Exercise direct initial/final engine apply for alias-only mismatched origin. Sent to root/worker; pending hardening.

## R3 — supersession depth cap depends on ID order

At initial review, verify_decision_policy's supersession DFS (~1430–1450) returns immediately for colors==2 without retaining the longest remaining suffix. A chain exceeding64 hops can pass if its successor was visited first. Minimal structural fixture: edges d66→d65→…→d00, with ID iteration d00,d01,…; each new visit sees an already-completed suffix at depth1. Entity-only merge histories with no assertion/mention references avoid the separately bounded endpoint/binding walks.

Cycle detection must preserve ID-independent maximum chain length, for example by memoizing longest suffix length and checking it at every root. Add a descending-ID over64-hop regression and a legal64-hop boundary. Sent to root/worker; source fix/runtime pending.

## Evidence and limits

No reviewer tests were run. Worker isolated P15+P12 compiler/target gates are ongoing;16 worker, two engine witness factory and three CLI cases were initially authored but unrun. No runtime/native recovery claim follows this review. P13 must later test the actual review CLI→entity-decision path; P12 manual accepted-status fixtures do not establish that future integration. Final changed-source hashes and supplied actual outcomes will be recorded after the source stabilizes.


## Forward R1–R3 changed-source disposition

R1 source-closed: verify_exact_decision_writes checks the complete write target set, including allocated Decision creations and declared main/mention predecessor writes. Creations require absent original ID/path and exact generated envelope/body bytes; predecessors require their exact original path and active→superseded status-only bytes. No unrelated Decision operation survives initial/final retained validation. The new regression invokes production engine apply with an otherwise valid unrelated alias Decision creation, checks the exact membership diagnostic and preserves canonical bytes.

R2 source-closed: retained GraphDecide requires exactly one canonical policy receipt matching operation/task/request hash and the complete allocated-ID map. No/mismatched receipt refuses before authority. The new production apply regression exercises alias-only wrong origin both with and without a durable receipt.

R3 source-closed: DFS retains proven longest suffix lengths and checks depth+suffix when reusing a completed node. Both cycle and hop ceiling are independent of ID order. The production canonical-policy regression constructs real entity-only receipts in reverse chain/ID order, permits64 edges and refuses an overlong chain.

Reviewed forward SHA256s (runtime pending):

```text
b6acb56f01d29960db709538806fc092e9e0762bd6f34859066ca19d11a37e48 src/graph/remap.rs
4e49e9a207a8d810aa80b0bd30c8d650fa13cd7cf10ae2ce0d8810b6273ea509 src/graph/decisions.rs
da5d8aaa519dc1ee62a6dc523f18b088344ea46dcbed2799f04932dbab7567f6 tests/entity_decisions.rs
```

## GraphDecide rollback: required sealed inverse extension

Actual source limitation: changes::rollback::prepare_inverse calls ordinary validator.validate, then emits origin=None/inverse_of=original. Ordinary Catalog proposition checks therefore reject the reversed endpoints; deleting new forward Decision receipts also removes their forward exception. This is core rollback behavior under CLI/storage contracts, not an external qualification deferral.

Proposed minimal extension, sent to root: engine-only opaque RetainedGraphInverse (no unchecked construction/deserialization), plus GraphValidator::validate_inverse defaulting to ordinary validation. At preparation and initial/final apply/recovery authenticate inverse_of's exact original manifest hash and Committed terminal proof, verify bounded original before/proposed payloads, and require the exact mutable-operation reversal (original after→before, inverse deletion of creations, reversed dependency ordering). No extra writes, origin or allocations; immutable assets stay retained. Current target observations must be exact inverse-before or inverse-after during recovery. Missing payloads, unfamiliar edits or noncommitted originals refuse.

Preserve actual input.documents. Reconstruct the original complete after-view separately from authenticated retained bytes for strict forward receipt/origin/allocation/semantic proof; never replace the actual scan. The Catalog exception authorizes only those authenticated reversed assertion endpoints, then uses complete ordinary reference, duplicate, decision and current source/evidence validation on the actual proposed inverse. Enumerate new inbound references to split-created identities, including resolved artifact bindings, and refuse their deletion. Later decision/author changes to original write targets fail exact original proposed-hash guards.

Freshness pitfall: do not reuse obsolete original source read_preconditions or claim unavailable historical unmodified bytes. Committed operation proofs authenticate the reversal; current SourceView/dependency checks authenticate present source support. Every restored accepted assertion (original before accepted, original after proposed) must have post-inverse Current eligibility on both initial/final/recovery checks, **independent of actual baseline status**. During partial recovery the accepted bytes may already be restored, so ordinary baseline-difference acceptance logic would otherwise skip this check. Stale/withdrawn support must never be promoted through rollback.

Recovery must rebuild the seal from inverse_of plus retained original/inverse manifests, not require forward receipts still present in current files. Required tests: actual merge/split inverse and FilesApplied interruption/recovery; new split inbound references; original-after author edit; forged inverse_of/noncommitted original/missing payload; hidden extra inverse writes; stale accepted restoration; immutable captured bytes remain retained. This section is a design recommendation, not implemented or runtime-validated behavior.


## D40 implementation source review

Status: new inverse implementation inspected read-only; runtime pending. Forward R1–R3 worker reports19/19 target4 passed (4.34s compile/55.53s runtime, `/tmp/lwiki-p12-worker-target4.log`); changed test fixture hash is `3f2fafd91afba74d526b874ed84c5627a05893d4c5430f7ee993bf39134e3a63`. This forward evidence does not certify D40.

The new opaque RetainedGraphInverseInput is engine-only, noncloneable and non-deserializable; ordinary default validate_inverse grants no exception. The engine authenticates each parent through exact manifest binding plus durable terminal receipt/complete Committed journal, rather than editable note status. Ancestry rejects repeated IDs and >64 hops. Exact inverse matching covers every mutable target/before/after/absence, requires empty origin/allocations/read guards and MutableRecord roles, and reverses dependency edges. Immutable assets are excluded from deletion. Immediate-parent and GraphDecide anchor retained bytes are hash/length/path verified before sealing.

The graph verifier preserves actual documents and reconstructs anchor before/after separately. It rechecks generated Decision bytes, original receipt/origin/allocation and exact field/body/artifact/predecessor transitions without requiring obsolete unmodified guards. Actual current inverse targets must be exact parent-before or parent-after bytes; the overlay is the exact reversal with no extra paths. Deleted split-created entities require no extra actual or final assertion/resolved-artifact references. The sealed authorized assertion set is still subject to complete final Catalog policy. The independent restored-accepted set derives from authenticated parent bytes; Catalog explicitly requires each member's final accepted status and Current support even if partial recovery's baseline is already accepted. Undo-of-inverse uses authenticated exact ancestry rather than a new forward exception.

### D40-R1 — retained payload allocation precedes aggregate proof meter

Blocking source evidence at this pass: rollback::inverse_plan loads the original via ordinary load_manifest and calls per-file verify_payload into its draft before retained_inverse_input establishes the256MiB meter. apply::validation_input likewise reads each proposed child payload before validate_graph invokes the ancestry factory. Several64MiB payloads can therefore allocate beyond the declared aggregate ceiling before subsequent semantic/lineage rejection. Per-file bounds do not fulfill the aggregate-before-allocation contract.

Require aggregate claimed-length checks before the first retained payload read in inverse_plan, and bounded aggregate proposed overlay reads for inverse manifests before ValidationInput construction; keep draft/preflight paths on that same bounded route. Hostile sparse payload/count fixture should return BudgetExceeded before later payload reads, with no staging/SQLite. Sent to root. No reviewer fixture execution claimed.

No other concrete D40 source blocker found in the bounded paths inspected. This is conditional architectural/source review, not runtime acceptance. Inverse/new-reference/author-edit/forged ancestry/missing payload/stale acceptance/partial recovery/undo-of-inverse tests remain required.

Inspected D40 candidate SHA256s (before pending budget fix):

```text
1dd4a95bdf7b7f2c2b7648b5ff8c32c0a8b7ec34cf60ade45addc8cb48f25d95 src/graph/inverse.rs
91c30b424f72825c93017ad5922aed4cee78ccc338ae9d692b8230d64299d6d2 src/graph/remap.rs
12c91b95019c3b44ae82b22ab6d07332c54c46c1286dd17c52c07abca067f9a2 src/changes/types.rs
62d699f7515e11b48854177336d9cb06b3ddd98ca9cfb5037d62c9378bdc37e6 src/changes/apply.rs
ee98d4cdab388b0940e8a6de9e9cba9df22397355758a559ec78bfa746ecf15b src/changes/prepare.rs
7bd442b2102bc067fd4735943e96bbf286c72e9ddffcf9f69f754bbf92965593 src/changes/rollback.rs
36c2a86443a754a3833f30898fd26e67fccd3fba0f852b72d99c51397cee7519 src/catalog/eligibility.rs
e317a45f6f86591193b22d7e37b75a0531018302dc75456035a8b22d19ce4122 src/app/offline.rs
```


### D40-R1 changed-entry-path disposition

The budget finding is source-closed conditional on the hostile/runtime gate. General ChangeEngine::plan checks inverse proposed-byte totals before cloning operations and meters actual before-image reads. inverse_plan charges the declared original aggregate before any retained before-payload reads, then uses bounded remaining reads. load_manifest now imposes intrinsic GraphDecide128MiB/inverse256MiB aggregate limits, protecting inspect and later retained-draft reads. validation_input checks declared child after-payload totals before reading/cloning overlays and charges actual reads. New inverse scan construction caps canonical files4096/bytes64MiB before each allocation. Draft validation and prepared/apply paths share the engine factories. No general caller receives an unchecked inverse seal.

These counters bound their declared payload/capture domains, not every operational journal/terminal-receipt read or whole invocation OS I/O. Existing bounded terminal proof reads remain separate. A minor error-channel observation sent to root: inverse_plan's depleted per-read limit can surface InvalidData from read_bounded rather than BudgetExceeded; the read remains bounded before allocation. No additional concrete D40 correctness blocker found in this bounded source review. Hostile declared-length and real inverse/recovery gates remain pending; no reviewer Cargo run.

Updated inspected hashes, superseding changed engine files above:

```text
fd0e016a8d7e13b93b86cccc782177c745296159101c95cc9714f96ef8bd309c src/changes/apply.rs
fc3692e0e2be842c220171ae437d5b74e160458e4398086e356699238f89a97a src/changes/prepare.rs
6da1c9d77c383410c40ee6a64baf3ebe4531aa7649b9211e713a3df64e0b4e35 src/changes/rollback.rs
7a55c2eff9fde0b6879ac2e27c396df38ea490ebdbb97e0aad624a9f0c236dc9 tests/entity_decisions_inverse.rs
```

Authored inverse tests cover merge/split exact undo and redo, retained immutable captures, source withdrawal while reversing proposal-only assertions, alias no-op historical proof, added assertion/resolved mention references, author edits/missing/corrupt/noncommitted parents, extra writes/forged lineage, partial inverse with deleted forward receipt, and accepted restoration before writes plus Applying/FilesApplied recovery. They exercise production factories and real interruption paths, but their actual results must be supplied before acceptance.

## Root runtime disposition and acceptance

Root independently matched final30 overlay bytes and read passing logs: forward19, inverse10, CLI3, sparse preread1, retained factories2, full native changes28/one explicitly invoked ignored helper and shared145 parents. R1-R3 direct retained forgeries and chain gates passed; R4 aggregate-before-allocation sparse gate passed. Actual partial recovery/new-reference/stale accepted restoration/undo cases passed. Strict Clippy/fmt/build/schema/capabilities28/seed34 passed. P12-checks.json records final hashes and attribution; P12 is locally accepted, while P13 real review integration remains mandatory. Native interruption evidence is not power-loss/other-platform/live qualification.
