# Selected identity and lifecycle validation

The 0.2.0 candidate preserves ordinary verified discovery when an Entity has a
Current identity and an unsupported or invalid description. Its authenticated
identity can appear as empty, uncited navigation. This does not grant authority
to the description or permit unsupported body excerpts. Ranking, excerpts,
filters and cursor rules are unchanged.

Independent scoped acceptance: **9.5/10, no observed correctness blocker**.
This qualifies the repair and the tested update, withdrawal and recovery workflow
on a disposable native macOS ARM64 1k vault. It does not pass the original failed
[capacity query trial](validation-capacity-workflow.md), broader context quality
or the 25k capacity gate.

## Checks and actual behavior

One grouped release checkpoint passed all sixteen focused native tests: thirteen
selected-search coordinator cases and three CLI adapter cases. New adversarial
checks reject forged identity/body authority, secondary excerpts, wrong record
types, unsupported identity and selected edits. Existing unsupported-body and
final-recheck behavior remains covered. Compilation and tests completed in
108.103 seconds; test execution took 5.52 seconds. No unchanged checks were rerun.

A prospective finite public replay completed 54 observations, including one
preserved cached baseline and 53 new commands. Sixteen cached/verified discovery
pairs and same-publication pagination preserved the returned content. The
previously failing question returned all ten original hits, including the empty
Entity navigation hit. Independent byte/span/hash inspection verified 49 search
citations, 20 context citations and four exact reads. Its initial context includes
the requested approved deployment lead; the short search/read excerpt alone does
not. The four other missing-fact tasks were not replayed.

Refreshing one Source captured exactly the frozen 80,497-byte replacement in a
new immutable Revision while retaining Source identity. Immediate search, context
and read used that revision without an intervening sync. Withdrawal removed it
from Current search and context; its old citation still read the retained bytes.
The supervised refresh and withdrawal took 2.763 and 1.423 seconds respectively.

After idle recovery and a complete backup, removal of only the owned cache was
followed by reconstruction and public full check. Rebuild took 11.631 seconds;
check took 15.564 seconds and returned no diagnostics with canonical/cache
agreement. Protected canonical and retained bytes and mtimes matched their
inventories. Rebuilt discovery matched under prospectively declared publication
normalization; the old cursor refused with `CURSOR_STALE`. An actual selected
Entity edit refused with `FRESHNESS_CONFLICT`; its exact bytes and mtime were
restored. Both refusals returned exit 4 and null data.

The independent critic recomputed eighteen discovery comparisons and preservation
over the actual original, backup, rebuilt and restored-copy inventories. Its
audit checked 395 executable, source, protocol, compiler and test pins without
rerunning the product. The wider failed trial and all unrun tasks remain recorded.

## Measurement and reproduction limits

The pinned executable SHA256 is
`19350962d3342bb6372397ffe986b6c772635523956e0154566006a1f6c6b699`.
The compiler response files establish Rust edition 2024, optimization level 3 and
native `aarch64-apple-darwin`. The owning replay interval was 115.844 seconds,
including copies and 8.573 GB of repeated SHA256 observations. These observations
are not unique vault size or shipping I/O. Native main-process RSS peaked at
196,870,144 bytes; sampled tree RSS is approximate and its exact peak unavailable.
This one-off replay establishes neither warm latency distributions nor capacity.

Follow the [disposable candidate walkthrough](testing-0.2.0.md) and
[selected-search contract](indexed-context.md#selected-search-citations). Local
raw reports and pinned fixtures are retained outside Git; this summary describes
the tested source and scope rather than promising a portable benchmark fixture.
Reconstruction exceptions were limited to derived cache, operation publication
authority and the writer-lock diagnostic PID; complete backup preserved them.
Full check explicitly leaves unused retained payloads unchecked. No separate SQL
equivalence, power-loss, other-platform or live-provider qualification is claimed.

Retrieval completeness, HIGH/unseen evaluation, import throughput, 25k scale and
strict Clippy remain open. The [earlier verified-search gate](validation-verified-search.md)
retains its separate broader public command coverage.
