# Authenticated Source citations in saved draft Pages

The typed Page workflow accepts exact SourceRefs returned by verified context or
captured-content reads, authenticates them during the guarded write, and derives
links from the allocated Page destination and actual Source/Revision metadata.
This closes a measured navigation gap in the earlier host-assisted pilot, which
saved four broken hrefs across three Pages despite 37/37 exact machine references.
That pilot remains failed evidence; it has not been repaired or rescored.

`page init` and `page put` accept `--source-refs FILE`. Inspect
`schema page-source-refs`: the strict version 1 request permits at most 16 submitted
Source references and 64 KiB, with exact duplicate removal in first-occurrence order.
The generated block contains Source, immutable Revision and captured-content links,
exact JSON refs and states observed during that write. The command does not certify
prose support, complete answers or continuing freshness. Ordinary Page rename
rebases these generated hrefs while retaining exact refs and authored bytes.

Full-file `--if-match` continues to guard edits. Explicit empty references remove
only the generated block; omission retains ordinary authoring. Malformed/ambiguous
owned blocks refuse without repair. The 16 MiB Page ceiling includes rendered output.
Dry-run bounds/parses inputs but performs no Source proof or link rendering.
Normalized proofs touch selected dependencies; legacy retains its bounded SourceView
scan and a separate selected-payload allowance. The limits are not one aggregate 64 MiB.

Verified captured-content `read` exposes exact SourceRefs in both layouts, including
bounded Unicode ranges. Legacy reuses the same citation constructor after matching
the read to its authenticated snapshot; cached, dry-preview, empty and authored
note reads remain uncited. The library's low-level body API is still uncited.
The [cited Page recipe](../skills/llm-wiki/references/cited-page.md) explains obtaining
refs, saving, inspecting and reconciling a draft with fresh author guards.

## Validation and limits

Native release-mode code was built with optimization 3/debug 0 for Apple Silicon,
minimum macOS 26.5. Eighty-six distinct affected native checks pass across the
integrated checkpoints; two existing tests remain ignored. Passing unchanged
cases were reused. One staged-publication fixture assumption failed, was traced
to actual staging semantics and corrected; the actual refresh/stale-apply check
then passed on both layouts. No unchanged broad HTML/whole-suite gate was replayed.

The initial independent public-command assessment failed 8/10. Its 128 calls found a
real legacy verified-read omission and left a correctly constructed legacy UTF-8
negative unrun after a harness-control mistake. Saved refs/navigation, author
preservation, Source refresh/withdrawal, ordinary moves, cache-loss reconstruction,
previews and real staged Source guard refusals passed on both layouts. Runner,
timing, source-read allowance and reporting deviations remain recorded; the
initial protocol is not claimed compliant.

A fresh independent review passed the functional composite at **10/10**: all five
groups, every required task and both layouts, with zero demonstrated correctness
blockers. Twenty-five new public calls verified the repaired read-to-ref-to-Page
path, exact spans/hashes/ownership, Unicode paths, navigation, guarded updates,
rename and UTF-8 refusal. Unchanged earlier functional cases were reused against
the complete production delta. The cached-read oracle was corrected to inspect
its actual `index_snapshot` metadata; a separately declared single dry-read check
completed the last unrun task without changing the fixture. Failed harness
attempts and incomplete assessments remain preserved. This composite does not
claim compliance with the original timing protocol or a continuous combined
180-second interval; no performance result is inferred.

This is a scoped draft authoring/navigation result. Historical deterministic
context completeness remains 9/28 facts and 2/10 positive tasks; the newer pilot is a
different dataset/workflow. Native HIGH, semantic acceptance, practical 25K and
full 0.2.0 release qualification remain open. A local trial package is separate
from tagging, publishing and full release acceptance.
