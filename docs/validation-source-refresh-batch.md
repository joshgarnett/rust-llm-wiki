# Source refresh batch functional validation

The [guarded batch workflow](source-refresh-batch.md) passed independent Astra
functional acceptance at **10/10**, with all nine mandatory groups satisfied and
zero observed correctness blockers. The candidate was a native macOS aarch64
release executable, optimization level 3 and debug information disabled. Its
SHA256 was `c8dbd2d995bf2860764dab06a23a387c0b2132b4c0c7f14043a3c4810821b9ff`.
This is a functional checkpoint; throughput, default retrieval quality, 25K
capacity and full-release acceptance remain open.

The complete public run issued 332 offline CLI calls in 55.096 seconds: 286
successful calls and 46 expected refusals. Both original and managed storage
layouts passed. Commands covered one and sixteen members, reverse caller order,
shared input bytes under distinct owners, no-op/historical/title-only refresh,
staging and exact apply after input deletion, stale/malformed refusals, old-binary
safe refusal, immutable history, authored Pages, full checks and complete cache
loss followed by rebuild. All full streams were retained; no provider calls or
unreaped children occurred.

Independent checks authenticated 44 returned Source citation hashes and spans
(40 current and four historical at their respective observation points). Both
cache-loss rebuilds reproduced all 23 logical catalog tables, stable metadata
and complete logical FTS terms. The inventory also checked all 36 schema tables
and 181 ordered columns; unexpected tables or columns failed. Only the five
predeclared physical row IDs were excluded from logical row equality. The four
compact-unit relations were empty in these CLI fixtures, so their equality does
not qualify populated embedding-unit behavior.

Focused native evidence contains 133 latest passing checks and one intentionally
ignored self-spawn crash child. Forty-five historical failed results remain
preserved, including renamed-test supersession and invalidated wrong-route
coverage. Relevant tests live in [batch projection tests](../src/catalog/source_refresh_batch_projection_tests.rs),
[application tests](../src/app/source_refresh_batch_tests.rs) and
[retained replay tests](../src/changes/indexed_refresh_tests.rs).

All 32 recovery cells passed: eight reached cuts, two storage layouts and both
returned errors and native SIGKILL. Cuts cover retained evidence, active intent,
first fresh asset, between Source pointers, FILES_APPLIED, SQL before/after
commit, and the durable terminal receipt before authority cleanup. Ordinary
`changes apply` through `OfflineApp::changes_apply` converged to the original
allocations and one intended epoch; repeat preserved bytes/mtimes and idle
authority. These macOS process-kill checks do not establish power-loss or native
safety on other platforms. The separate large-vault general-recover timeout
remains failed evidence.

The default 4096-row boundary was exercised jointly. Logical byte admission,
nonzero reduced deadline tests and the post-writer pre-prepare guard passed.
Lowered private encoding limits test defensive admission at adjacent boundaries;
they do not measure maximum usable 64/256 MiB payload capacity or allocator peaks.
Deadline checks are cooperative. Compatibility includes accepted-old-binary
emitted v3 inputs and independently specified deployed-v2 wire over a compatible
base, with exact encoding/checksums and public replay. The latter is not a
historical v2-produced cache. [Fixture provenance](../tests/fixtures/old-wire-provenance.md)
records immutable inputs, including exact old producer WAL/SHM materialization.

The next performance gate is 1,000 genuine changed captures within 600 seconds
and at least 1.5× improvement against a complete, prospectively pinned scalar
pair. The original scalar run acknowledged 272 updates before its 600-second
failure; it supplies no complete speed ratio. The first paired experiment passed
its fresh-copy qualification, then stopped after one successful scalar refresh
because its verifier expected the batch `capture_state` field in the scalar
response, which uses `extraction_status`. Its publication comparison also confused
the snapshot's version/hash authority with the operations record's epoch
authority. These are benchmark adapter faults, not demonstrated product failures.
A complete paired batch arm and speed ratio remain unmeasured; the changed scalar copy
and all failed receipts are preserved. A new pair must use corrected, independently
reviewed adapters and fresh qualified initial states. No native quality gain
follows from this functional milestone; the [retrieval evaluation](validation-native-phrase-locality.md)
records the separate budget profiles and their limits.

## Separate single-arm 10K checkpoint

A subsequent, prospectively reviewed experiment qualified one disposable copy of
the complete post-churn 10K seed. All 91,989 files and 32,909 directories, including
the root, matched the frozen manifest; source preservation and quiescence checks
also passed. Qualification took 108.199 seconds. The seed contained 7,981,127,123
logical file bytes. The system copy allowed opportunistic APFS cloning with a
funded full-copy fallback; allocated-block counts do not establish exclusive
physical ownership of shared extents.

The earlier approximately 129 GiB free-space requirement funded an entire scalar
and batch pair, its floor and margin. It was not the size of one seed copy. The
single-arm protocol reserved 80 GiB for the complete experiment, plus a 32 GiB free
floor, 2 GiB margin and 6 GiB competing reservation. This admission does not change
the original resource gate or qualify 25K capacity.

Independent Astra review admitted exactly 1,000 genuine changes in 63 batch
commands, followed by one complete integrity check and 21 current, historical,
withdrawn and authored-Page reads. The 600-second phase included actual input and
Source-guard hashing, request creation, returned-item validation, output and child
reaping. Old assets, retained Changes, Pages and caller inputs remained mandatory
preservation checks; acceptance required at least 9/10 with every gate satisfied.

The attempt **failed at its first command** because the sandbox denied the existing
process-memory observer. The controller killed and reaped the child; neither the
product nor verifier acknowledged an update, and both output streams were empty.
The owning interval was 16.429 seconds, including preflight. All remaining updates,
reads and the integrity check were unrun. A subsequent bounded control-file read
found unchanged idle publication authority; it did not prove the entire target
unchanged. The failed target and receipts are preserved without retry or recovery.

This is an execution-environment failure, not a demonstrated product throughput
defect. The complete 1,000-update timing, scalar comparison, 10K lifecycle and 25K
gates remain open. No Rust rebuild or provider call was needed for this checkpoint.

One separately approved, read-only permission probe then ran the unchanged observer
against its own live Python process. It returned positive RSS in 0.031 seconds in
the escalated context. That qualifies this invocation context only; it does not
authorize retrying or rescoring the failed benchmark. Future native measurements
need prospectively admitted fresh state and qualified observer permissions.

## Integrated 10K workflow throughput checkpoint

A later independent review admitted a complete managed-update-to-cited-Page
workflow on a separate fresh copy of the same 10K post-churn seed. Preparation
matched all 91,989 files and 32,909 directories and preserved the protected seed;
it took 97.006 seconds. The accepted release executable and its optimization
settings remained unchanged. No Rust build or provider call occurred.

The native run **failed the update-throughput gate**. Twenty-five 16-member batch
commands acknowledged and locally verified 400 fresh captures. Their median
launch-to-reap time was 20.982 seconds. At 543.607 seconds, the controller refused
to launch another command because its complete 60-second window plus five-second
cleanup reserve would exceed the frozen 600-second phase. The remaining 600
members were unattempted. The native owning interval was 557.794 seconds; summed
with preparation, the separately measured owning intervals were 654.800 seconds.
External review and launch gaps are excluded from that sum.

The observer recorded 2,243 samples without resource errors; all children were
reaped. The final integrity checks, citation controls, retrieval and cited-Page
steps were unrun: 73 of the 98 planned calls received no acceptance credit. The
changed target and complete receipts remain preserved without retry or recovery.
This partial batch result supplies no complete scalar comparison, speedup ratio,
whole-workflow score, default-quality improvement or 25K qualification.

The next milestone is one narrowly instrumented release invocation on another
qualified initial state, with the same 16 caller inputs and guards. It will
attribute selected-proof checks, file/journal durability, SQL changes, checkpoint
and finalization costs. Code inspection found repeated selected-dependency checks,
but neither those checks nor the approximately 2.6 GB catalog size establish a
bottleneck. Optimization remains contingent on measured stage costs and preserved
freshness, immutable-history and crash-replay invariants. The time and correctness
gates remain unchanged.
