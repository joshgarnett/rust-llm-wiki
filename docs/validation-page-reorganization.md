# Normalized Page reorganization validation

The normalized authored-Page workflow now completes authoring, moving a stable
identity, repairing known incoming links, immediate navigation, guarded editing,
recovery and offline cache reconstruction. Independent scoped acceptance passed
at **10/10**, with every mandatory task and no correctness blocker, against the
unchanged minimum 9/10 gate. This is separate from automatic context completeness,
query-mode parity, default activation and representative 25k qualification.

## Public workflow and exact reconstruction

The final native macOS ARM64 release executable completed **86 public CLI calls**,
five bounded logical exports and three local hash-helper calls in **16.402 seconds**.
Commands ran offline from outside disposable vaults whose paths contained spaces.
The supervisor used a monotonic clock, with limits of 128 CLI calls and 600 seconds;
these small-fixture observations are not capacity or latency qualification.

The mixed fixture exercised authored, plain and readable unadopted incoming notes,
wiki/inline/reference links, fragments, titles, aliases, self-links and code examples.
The Page kept its ID at the new path. Unrelated files retained bytes and modification
times, and captured Source/Revision bytes and exact Current source citations remained
unchanged. Reads by ID/path, complete ordered search results and document context
matched after an owned complete backup, removal of only the derived cache and public
offline reconstruction.

All **22 semantic catalog relations** matched exactly, including complete JSON facts,
identity claims, navigation and match keys, dependencies, source history, policy,
eligibility, graph rows and owner/term/column/offset search postings. The oracle
excludes physical row IDs and publication bookkeeping prospectively; it does not
exclude identity, paths, content hashes, eligibility, ranking or semantic dependencies.
Returned read objects compare in full. Search/context comparisons remove only the
declared publication-bound fields and observations; rendered accounting is checked
individually. No exclusion was added after a result.

A separate public control moved a Page with 20 authored incoming owners and a
plain policy owner, loading an actually affected external policy certificate.
Every required rewrite completed; its exact catalog and ordered search contents
also matched public reconstruction. This exceeds the ordinary 16-Page batch size
without changing that batch contract. It is not a large-vault fanout measurement.

Wrong hashes, occupied destinations, portable collisions, missing/duplicate identity,
non-Page targets, same-size externally edited incoming files and required immutable
rewrites refused without canonical mutation. Same-path requests authenticated the
author guard and reported reuse without a change/publication. Request-only previews
preserved the complete tree, including SQLite sidecars, and disclosed unchecked
admission in both JSON and human output.

The mixed fixture deliberately includes invalid notes. Full checks reported the
expected canonical diagnostics while completing the canonical/cache comparison.
Cache agreement is not a claim that those notes are valid.

## Native checks and preserved failures

The final integrated release build passed in 96.335 seconds; actual compiler
parameters establish optimization level 3, Rust 2024 and native ARM64. Five final
move tests passed: exact mixed reconstruction, affected policy fanout, guards and
budget refusal, SQL publication cuts, and filesystem cuts. Both declared filesystem
cuts actually fired, after destination replacement and before old-path removal;
ordinary `changes apply` completed recovery and repeated apply returned the same
commit. Both SQL cuts fired, after pointer update and after commit. Held readers
retained the earlier publication, new readers saw one moved identity, and recovery
preserved captured history and exact reconstructed state. This tests deterministic
native process faults, not machine power loss.

Two app lifecycle tests passed at the preceding checkpoint, including refresh,
historical/withdrawn citations and reconstruction. Seventy-two unchanged affected
tests passed at the first checkpoint and were reused: retained writes, normalized
row/fact deltas, policy, Page batches, capture and withdrawal. Only the new rename
planner and its tests changed between that checkpoint and the final build. These
are named checkpoint results, not a claim that the latest entire suite passed.

Earlier native failures remain recorded: unsupported/incorrect Entity fixture
support, a stale moved-page direct path, stale registry-probe cache across the
move transition, a fault fixture's error category, and one test-only compile error.
The fixes preserve existing identity/resolver/eligibility rules; the proper Entity
fixture now has Assertion/Evidence support.

Three public supervisors remain failed under their original assertions: a body-only
expectation compared with a complete Page envelope, invented human stdout wording
instead of the actual stderr warning, and an empty-stdout assumption for JSON
argument errors. Their successful prefixes are preserved. Separate prospective
reviews corrected only those serialization expectations; product sources, fixture,
semantic comparisons, limits and acceptance threshold stayed fixed for the final
successful public run.

## Reproduction and limits

Follow [the Page guide](page-reorganization.md) and [0.2.0 walkthrough](testing-0.2.0.md)
on disposable data. After the [native release build](builds.md), the focused tests are:

```sh
bazel-bin/unit_tests catalog::rename_projection::tests::mixed_page_move_matches_exact_fresh_rebuild --exact --test-threads=1
bazel-bin/unit_tests catalog::rename_projection::tests::page_move_ --test-threads=1
bazel-bin/unit_tests app::page_rename::tests:: --test-threads=1
```

The accepted binary SHA256 is
`0b8c129bf6a54b9153669764119610f2de06187439f2c6b774aaa7f15cfa9a30`;
the final native test binary is
`76e6912460dc7a72331fdeff1916746665f10bb6780b3bdbe20f46f90964bac0`.
Detailed frozen protocols, source/compiler pins, transcripts and failed attempts
remain local execution evidence. The candidate package records source provenance.

Normalized storage remains opt-in and the move target must be an authored Page.
Discovery uses published incoming membership; external edits need explicit sync.
Admission remains finite and may refuse genuine large affected closures. No new
provider calls, local model, service database or general repair subsystem was added.
Latest broad suites/strict Clippy, other platforms, live providers, signing and
power-loss safety remain unqualified. Native HIGH, automatic answer completeness,
full command parity/default activation and 25k remain open.
