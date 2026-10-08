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
failure; it supplies no complete speed ratio. The paired experiment and its
resource admission remain unrun. Default native quality remains 9/28 facts and
2/10 positive tasks; no gain follows from this functional milestone.
