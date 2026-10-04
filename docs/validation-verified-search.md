# Selected search validation

The 0.2.0 candidate adds `search --verify-selected` for normalized lexical
indexes. It keeps cached discovery order and excerpts, authenticates displayed
documents and their supporting dependencies, and emits exact citations for
nonempty captured excerpts. Authored and empty excerpts remain uncited.
`--no-sync` is compatible; global dry-run validates only the request.

Independent scoped acceptance: **9.5/10, all ten public tasks exercised, no
observed correctness blocker**. This qualifies the explicit selected-search
workflow on the tested native platform. The original plain-search 1k assessment
remains failed, and answer completeness and 25k capacity remain unqualified.

## Evidence

The candidate was built for native macOS ARM64 with Rust optimization level 3,
edition 2024 and minimum deployment target 26.5. Thirteen focused native tests
passed: ten proof/lifecycle/cursor/corruption/budget cases and three CLI adapter
cases covering search-only parsing, no-sync compatibility, pure preview and
unsupported routes. A missing test import during extraction was corrected; its
failed compilation remains recorded. Bazel successfully compiled and ran the
passing tests, then exited 37 because the sandbox blocked Java process inspection
at shutdown. The passing tests were not repeated to mask that wrapper failure.

A prospective prototype gate compared 120 observations on identical published
inputs. Sixty result pairs matched in every non-proof field; 300 verified
citations matched canonical source bytes and ownership. Its independent score
was 9.5/10, with an older preservation endpoint and unavailable exact process-tree
peak recorded as measurement limits. This test-only experiment did not qualify
the public CLI.

The subsequent public gate used the same 20 development searches, three rounds,
identical generation/publication and fixed limits: 80 candidates, five hits and
1,024-byte excerpts. All 120 commands succeeded; all 60 pairs preserved discovery
fields and all 300 verified citations passed. Cached results remained uncited.
Both arms completed 54/57 positive observations and all three negative
observations. The same missing-fact query failed in every round; verification
did not improve its returned content.

The gate also exercised actual human/JSON citation navigation, mixed authored
and captured results, empty/identity excerpts, source refresh and withdrawal,
historical citations, filters/pagination/stale cursors, selected content/policy
refusals, unrelated edits, legacy/unsupported modes, budget refusal, preview on
normal/corrupt caches and unchanged context parsing/help. There were 161 actual
CLI invocations: 120 paired and 41 supplementary. The original finite envelope
was prospectively extended by one call to correct an invalid authored fixture;
criteria and candidate stayed unchanged. Two checker/fixture errors and a later
policy-edit helper assertion stopped the original attempts. Their outputs remain
failed evidence; corrected continuation ran only affected or previously unrun
commands. No paired searches, completed navigation or help checks were repeated.

Warm supervised CLI p95 was 43.75 ms cached and 54.52 ms verified. These intervals
include launch and exit; they are not internal proof timing. The first cached
call took 705 ms. The supplementary large-document budget refusal reached the
proof byte limit, returned no data and supplied an actionable hint. Its peak
main-process RSS was 639,434,752 bytes; exact process-tree peak and shipping proof
counters are unavailable. Native proof counters and fixed defaults were checked
separately. No model/provider calls or new holdout questions were used.

Across public execution, all 8,907 vault paths retained identical contents; all
8,379 canonical/retained files also retained their mtimes. Only a SQLite shared
memory sidecar mtime changed. Controlled accounts occupied about 3.795 GB of
recorded file blocks, with about 107.877 GB free. Those figures count file blocks,
not unique APFS extents, and exclude the build cache. Preservation and inventory
ran outside query intervals.

## Reproduction and limits

Use the [candidate walkthrough](testing-0.2.0.md) and
[selected search contract](indexed-context.md#selected-search-citations) against
a disposable vault. Add `--verify-selected` to normalized lexical searches;
follow each captured hit's payload path and exact byte span with `read --path`.
The immutable Revision record's ID navigates to metadata rather than the payload.

The shared coordinator bounds selected verification with 64 MiB, 4,096 files,
16,384 entries and a two-second deadline. It does not synchronize or establish
global membership, uniqueness, unselected freshness, truth or completeness.
These 1k development controls do not establish realistic/unseen semantic quality,
25k capacity, live-provider compatibility, other native platforms or power-loss
safety. The [original 1k control](validation-1k-collection.md) and
[scale protocol](testing-large-vaults.md) retain their separate limits and gates.
