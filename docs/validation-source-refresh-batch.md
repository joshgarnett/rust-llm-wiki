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

## Measured update-stage diagnosis

One instrumented native release invocation subsequently completed the same first
16 changed inputs on a separately qualified 10K copy. Both library and executable
compiler arguments specified optimization level 3 and debug information level 0.
The one build took 74.388 seconds; copy qualification took 96.901 seconds and
matched the protected seed before and after copying.

The original diagnostic controller failed during setup before any native command
or vault read/write. Its failed receipt remains preserved. Independent review
prospectively admitted a corrected experiment on the proven unmodified copy;
the corrected controller exercised its complete preparation path without launching
the CLI before receiving execution approval. That preparation took 1.895 seconds.
No additional build or copy occurred.

The corrected invocation exited successfully and returned 16 captures, each
correlated with its caller input and locally verified against the selected Source
note and publication authority. Independent Astra review reconciled all 16
results, exact command, output streams and diagnostic counters. The native
process's own monotonic interval was 38.193 seconds; the external launch-to-reap
interval was 38.783 seconds. These are separate measurements, not an overhead or
speedup estimate.

| Exclusive native stage | Seconds |
|---|---:|
| Canonical file application | 27.399 |
| Preparation and sealing | 9.329 |
| Final verification and acknowledgement | 0.946 |
| Joint projection | 0.324 |
| Selected SQL changes, commit and checkpoint combined | 0.128 |
| Public input and planning | 0.053 |
| Startup and dispatch | 0.015 |

File application and preparation consumed 96.16% of this invocation. Nested
counters recorded 193 dependency-guard passes, 22,967 precondition checks and
32,457 bounded canonical reads delivering 1,129,794,177 logical bytes. Target
hashing itself took 0.393 seconds, compared with 16.691 seconds for the enclosing
target-state checks. Payload retention took 8.253 seconds inclusively; its path,
layout and durability sub-costs were not isolated. Nested durations and logical
byte categories overlap and must not be added together. They do not measure
physical I/O or all SQLite synchronization.

This evidence directs the next experiment toward redundant filesystem/path work,
while preserving fresh byte guards around every mutation and publication. It
does not justify an SQL/index rewrite or reducing guard frequency. The first
candidate tested removing redundant per-prefix canonicalization on Unix under the
existing canonical-root, validated-relative-path and fresh symlink-check
invariants. Its paired result is recorded below. Other platforms retained their
existing resolution path in that isolated candidate.

The diagnostic did not audit all retained payloads, immutable history or unselected
content, and did not run the complete integrity, retrieval or cited-Page workflow.
It supplies no paired speedup, 1,000-update acceptance, default-quality improvement,
25K qualification or full-release credit. All original gates remain unchanged.

## Unix path-resolution candidate: paired gate failed

The isolated candidate replaced repeated child-prefix canonicalization on Unix
with fresh root inspection and canonicalization before each resolution. It kept
fresh child symlink checks, nested-vault checks, byte guards and durability order.
Both compiled artifacts used native Darwin arm64 release settings with
optimization level 3 and debug information level 0.

The comparison used two newly qualified copies of the same unchanged 10K seed,
identical 16 caller inputs and guards, and a fixed baseline-first order. Each arm
received one ordinary offline `source refresh-batch` invocation with the same
60-second command limit. Acceptance required a candidate launch-to-reap interval
at most 80% of baseline, no regression in the Rust owning interval, reductions in
file application and preparation, and unchanged correctness and resource gates.

| Measurement | Baseline seconds | Candidate seconds |
|---|---:|---:|
| External native launch to reap | 37.564 | 36.496 |
| Rust owning interval | 37.525 | 35.880 |
| Exclusive canonical file application | 26.892 | 25.529 |
| Exclusive preparation and sealing | 9.167 | 8.992 |

Both commands acknowledged and locally verified 16 updates, retained 193
dependency-guard passes and 64 canonical operations, and produced valid traces
without overflow or invalid stage events. Independent Astra review reconciled
the actual commands, returned captures, traces and cumulative accounting. The
external ratio was 0.97157: an
observed 2.84% reduction, below the frozen 20% threshold. This single pair supplies
no statistical speedup claim. The candidate remains isolated and is not promoted
as a performance improvement.

Seventy focused correctness checks completed successfully. The original complete
37-test recovery invocation timed out during its crash matrix and remains
incomplete; later targeted checks do not turn that suite into a pass. Two earlier
controller setup failures were preserved, each before native execution; independent
review admitted the bounded remaining preparation and pair without rebuilding,
recopying or changing the threshold.

The complete 1,000-update-to-search-to-cited-Page gate was not launched after this
failed mechanism screen. A fresh Astra architecture review found no evidence
justifying another isolated storage patch as a solution to the unchanged
600-second workflow. The selected next priorities are the separately frozen
retrieval-quality experiment, subject to its exact outbound-payload approval,
and completing the external-editor Page workflow. Default retrieval quality,
the complete 10K lifecycle, 25K capacity and full-release acceptance remain open.

## Guard-locality architecture review: implementation rejected

A subsequent independent Astra source review reconciled the measured repeated
checks: 64 canonical operations, three complete guard passes per operation and
one final pass produce 193 passes; sweeping 119 retained preconditions explains
all 22,967 checks. This is arithmetic and source inspection, not a new native
measurement or proof of each traced path's meaning.

The proposed optimization would check each operation's own Source/Revision guards
around its mutation and check shared semantic/policy guards only at the beginning
and final publication. The review rejected implementation under the unchanged
freshness/recovery contract. An ordinary external edit to another selected or
unchanged member must still be detected at the next existing guard before further
canonical writes. Final publication refusal cannot restore that earlier refusal
or prevent the additional partial writes. Distinct identities prevent ownership
overlap; they do not establish byte freshness for other members or shared policy.

The retained flat read set contains joint semantic authorization, including
unchanged witnesses. It has no authenticated partition proving which checks can
be omitted at a checkpoint. A path-prefix heuristic, cached hash or reduced
guard frequency is not an accepted substitute. A coherent phase redesign would
require a prospectively reviewed contract and exact compatibility/recovery
semantics; this review authorizes neither that redesign nor another native run.
The only admitted next diagnostic is a bounded static public-plan dependency
matrix, if needed to decide that architecture. The complete 1,000-update gate and
all correctness thresholds remain unchanged; no storage candidate was promoted.

## Static dependency matrix: closed without another optimization run

On 2026-10-09, the bounded matrix and independent Astra review confirmed that
the public plan retains the same 64 operations and 193 full guard sweeps. A
prepared operation program preserving their existing order and refusal boundaries
does not remove that dominant work. Two duplicate computations were identified
within individual evaluations, but their exclusive costs were unavailable; the
inclusive phase timings do not establish enough savings for the 600-second gate.
No duplicate-calculation patch, new instrumentation or further resolver tuning
was admitted. This conclusion uses source inspection and the existing measurements,
with no new native performance claim.

The 1,000-change threshold remains mandatory for full capacity. Its failed 10K
observation does not prohibit a separately frozen 25K interactive functional
attempt after profile-matched 1K/10K controls and safe resource admission. Such
an attempt must retain every applicable import, query, individual-update,
citation and recovery threshold, and report unfinished bulk/churn/rebuild and
release gates separately. Passing a subset cannot qualify full capacity or
default retrieval quality. The [large-vault protocol](testing-large-vaults.md)
continues to define those gates.

A fresh whole-workflow review on 2026-10-10 again rejects another throughput
patch or experiment on current evidence. Guard counts are implementation-derived,
but the next-guard refusal before additional canonical writes is observable.
Stronger identity allocation cannot authenticate external byte edits; duplicate
work within individual guard evaluations still lacks an exclusive cost case.
A staged-publication redesign would need an explicit canonical/recovery contract,
compatibility and restoration rules, reached recovery cuts, and a benefit case
covering preparation as well as application. No such redesign was adopted.
The useful functional 0.2.0 checkpoint remains separate from the failed bulk
performance gate; no new build, benchmark or performance claim follows.


## Maintenance cost instrumentation checkpoint

Developer instrumentation now covers ordinary update, search/read and normalized
rebuild phases, filesystem observations and joined worker accounting. It is
disabled in the shipping build; enabling it requires both the private Cargo
feature `maintenance-diagnostic029` and runtime
`LWIKI_MAINTENANCE_DIAGNOSTIC029=1`. Fixed-size summaries record attempted work,
errors and overflow without changing freshness decisions or mutation order.

One release-profile checkpoint passed 41 affected Rust tests and a public
default-off CLI smoke. Independent review authenticated the binaries, actual
streams and source identities. Both layouts retained the exact 193-checkpoint,
119-precondition guard fixture, with reached filesystem/activation races and
error/panic accounting. The shipping smoke remained silent with the diagnostic
environment variable set. An initial feature-build visibility error was fixed;
the passing shipping build was reused.

The subsequent update→search→read→rebuild→search→read diagnostic preserved two
orchestration failures: its first read requested 64 MiB above the public 16 MiB
ceiling, and the continuation's catalog oracle incorrectly required an empty
SQLite WAL. Neither failure is a product speedup or a quality result. The actual
16-source refresh and search succeeded; corrected read and normalized rebuild
also succeeded. Rebuild took 9.435 seconds on the 1K fixture. Independent read-only
inspection authenticated complete publication 317 and the returned read's exact
95,730 bytes and current citation. A WAL-aware continuation completed the last
search/read in 0.329/0.033 seconds. Full current membership (1,000 documents),
16 historical captures, all 1,001 Pages and 4,048 Source files remained exact.
Independent review also compared the actual pre/post-rebuild hits, read body,
source identities, byte spans and quote hashes. Composite mechanics are accepted
as six successful operations across seven attempts; both interrupted campaigns
remain failed. This is an instrumented 1K fixture observation, not shipping
performance or 25K qualification.

The refresh diagnostic closes the proposed path-buffer reuse optimization:
optimistically eligible construction was only 6.060 ms, or 0.04987% of the
12.152-second command-owner interval. Fresh filesystem canonicalization occupied
3.194 seconds of that interval and cannot be treated as reusable validation.
An independent architecture review instead motivated a descriptor-relative route
and I/O candidate, preserving every current namespace and freshness check. Its
performance remains unmeasured. The earlier failed threading comparison remains
failed; the canonical 1,000-update/600-second product gate remains open.

Separate interrupted campaigns do not supply a combined workflow timing. These
observations qualify neither retrieval quality, representative capacity nor the
release. All final workflow gates remain mandatory.
