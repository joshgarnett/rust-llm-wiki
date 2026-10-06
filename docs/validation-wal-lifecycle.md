# Bounded WAL lifecycle during ordinary publication

Updated 2026-10-05. Independent acceptance is **PASS_SCOPED_WAL_LIFECYCLE**:
all six frozen mandatory conditions pass, with no scoped correctness blockers.
The release candidate completes the public 1K workflow and the finite native
mechanics gate. This categorical acceptance does not establish 10K/25K capacity,
native HIGH or retrieval completeness.

## Behavior and recovery

Ordinary normalized Source and Page publication releases its owned planning
reader, commits the row/FTS transaction, verifies publication, then offers one
zero-timeout SQLite `TRUNCATE` checkpoint before acknowledgment. The existing
connection, selector lease and checked database/sidecar identities remain held.
FULL synchronization and persistent sidecars remain enabled.

An external reader can defer reclamation while keeping its old snapshot.
The checkpoint does not wait or retry. Other SQLite/identity errors retain the
committed publication and intended snapshot in their diagnostics; ordinary
recovery verifies and finalizes that publication once without retrying optional
maintenance or rewriting immutable revisions. This adds no schema, canonical
format, database service or periodic rebuild.

## Frozen experiment and observed change

The [baseline public lifecycle](validation-public-1k-lifecycle.md) and this
candidate use the same 1,000 Sources, 100,302,609 source bytes, 1,000 Page drafts,
100 development tasks, query ordering, release optimization and evidence budgets.
The single candidate runs 3,424 public CLI calls and all 700 query workflows:
initial queries, refresh/withdrawal, historical reads, pending-publication
SIGKILL and recovery, backups, complete cache loss, offline rebuild and query
replay. No provider call or live vault is involved.

Tasks use frozen keyword queries, indexed-document context limited to 6,000
UTF-8 bytes/1,500 estimated tokens/65,536 inspected entries, then bounded verified
Source reads; at most eight CLI calls per task. This compares native context and
the separate reading recipe, without generating answers.

The prospective gate requires WAL logical and allocated size ≤64 MiB at every
applicable existing quiescent checkpoint, active import ≤449.215 seconds, and
all earlier correctness, citation, reconstruction and resource checks. It does
not cap an arbitrarily long externally pinned reader or observe active peaks.

| Observation | Preserved baseline | WAL candidate |
| --- | ---: | ---: |
| Active import | 359.372 s | 326.133 s |
| Initial whole-task p95, 300 executions | 1.278 s | 0.517 s |
| Post-update whole-task p95, 300 executions | 1.292 s | 0.514 s |
| Rebuilt whole-task p95, 100 executions | Separate mixed-order tail | 0.514 s |
| Worst 128-item window allocated growth | 171,667,456 B | 104,640,512 B |
| Candidate maximum observed selected WAL | — | 4,152 logical / 8,192 allocated B |

All 34 applicable candidate checkpoints fit the WAL limit; four checkpoints
without a selected normalized database are explicitly not applicable. The WAL
is zero at ordinary completed import/Page/update boundaries, but one-frame
observations mean it is not zero at every checkpoint. The baseline post-update
WAL was 470,635,872 logical bytes.

Observed import time falls 9.25% and post-update task p95 falls 60.23%. These are
paired development observations, with operating-system cache state uncontrolled;
they are not isolated causal measurements or shipping latency guarantees.
Candidate owner time is 794.699 seconds, including 222.966 seconds of resource
observation. The failed baseline owner and separate completion have different
boundaries, so their totals are not a controlled total-workflow comparison.

The critic's completeness comparison finds unchanged grades: native context
supplies all required evidence in 550/700 executions and the reading recipe in
627/700. There are no question-level grade changes. Faster commands and correct
citations preserve the existing missing-fact gap; native HIGH remains open.

Independent inspection verifies 5,801 structured Source citations and 2,604
rendered citations without byte/hash/locator/eligibility errors. All 100 complete
post-update/rebuilt sequences match after the frozen narrow normalization of
publication-bound fields and strict cursor validation. Backups, 1,010 immutable
revisions, 1,000 Pages and the reached pending-publication recovery also pass.

The largest recorded allocation of the entire experiment account is
4,443,504,640 bytes, including preserved earlier evidence; minimum recorded free
space is 63,175,348,224 bytes. Exact active allocation peaks, physical SQLite I/O
and the complete logical import-work ledger remain unavailable.

The subsequent independent resource review retains **10K HOLD**. The unchanged
guard uses the maximum growth per item across every window: the last 102-item
window controls at 877,226.67 bytes/item. Its 1.5× margin plus two future-vault
equivalents and 1 GiB needs 40,555,382,784 additional bytes for a fresh 10K
control, before new input/setup; saved headroom above the 32 GiB floor is only
28,815,609,856 bytes. Removing replay duplication alone does not establish a
whole-lifecycle 25K storage route. No larger run is admitted or guard relaxed.

## Native checks and evidence boundary

Twenty-six unique affected native checks pass: 23 existing regressions and
three finite WAL cases. They cover 256 Sources in 32 groups, coherent held-reader
views, real busy deferral, next-publication reclamation, checkpoint I/O-error
diagnostics/recovery, ordinary Source/Page callers, immutable bytes/mtimes and
full-body Current/historical/withdrawn citations. Fault injection tests control
flow; they do not qualify real disk failure or power loss.

The first native group has 24 passes and two failures in a shared test helper
that called an uncited lower-level read. That failed result remains preserved.
The helper now invokes the supported verified CLI route. Only those two cases
were replayed after one unit-only rebuild; the other 24 passes and unchanged
shipping CLI were reused. One integrated release build and this justified
unit-only rebuild are the complete native build sequence for this milestone.

Native macOS ARM64 release, optimization level 3, Rust edition 2024. Candidate
executable SHA-256:
`eba9457aa850bb2c97dec1ebc01584b73a564ef8758874c2e10f885a09de28b4`.
Optional local evidence is under
`.artifacts/representative-25k-public-lifecycle-001/`; this guide remains usable
without those ignored files.

| Retained evidence | SHA-256 |
| --- | --- |
| Actual candidate owner result | `92620b62cce35b7589a5e968ae8df4f2e3105e1c3d5893650450f637e17e4652` |
| Merged native check receipt | `5239e3e03b203139d538795fd0828eba4298bef1488fd32f60cd597c27059d41` |
| Independent native mechanics review JSON | `b806f824fcfb58c78fd502fc7d49aa5daea91455b88ec278d914c84032de06df` |
| Independent actual public audit JSON | `b08e006ec1274b120459ae38d4d667f6f7ad2100bf7c38d9330526a742251e85` |
| Subsequent resource review JSON | `db0eb17b95d45cb950b7a862d1e55c915448c329b4540a01d6ccb7669878ba3f` |

The local **0.2.0 candidate009** archive uses source commit
`1503fd9dd3fcb26f9f08b29ee21c09dacbff8806` and the unchanged verified shipping
CLI above. Optional local archive:
`.artifacts/releases/0.2.0/lwiki-0.2.0-macos-arm64-candidate-009.tar.gz`, SHA-256
`ef36f6e031d779b85899981fe5a049a704779a87ce9ef31f2d2d703fde000855`.
Packaging verifies all 396 current source inputs against that commit, compiler
and acceptance seals, staged version, and all 50 files through a safe archive
roundtrip with exact bytes/permissions. It rebuilds or replays no product test.
The bundle includes both build receipts and explicitly records the test-only
helper delta and shipping CLI reuse. Native macOS ARM64, minimum macOS 26.5;
unsigned/unnotarized local candidate, with no public tag or release.

Native evidence completeness, useful cited Page synthesis, strict/historical and
semantic/hybrid modes, native HIGH, 10K/25K qualification, broad checks, other
platforms and power-loss/live-provider qualification remain separate open gates.
