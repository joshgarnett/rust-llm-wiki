# Request-only dry-read preview

CLI `--dry-run read` now returns a request plan before catalog or target access.
It completes the dry-read control that timed out on an occupied 10k vault while
preserving ordinary reads, citations and stale-edit refusal. The correction passed its frozen public replay and independent scoped acceptance
at **10.0/10 with no blockers** (threshold at least 9.0).
The [earlier diagnostic](validation-10k-workflow.md) remains **FAILED**.

## Public behavior and compatibility

JSON reports the parsed ID/path, requested byte range, effective maximum bytes
and requested verified/cached mode. `body` and `source_citation` are null;
`target_resolution_performed`, `utf8_range_validation_performed` and
`verification_performed` are false. It claims no snapshot or freshness.
Human output describes the planned selector, mode and bound, with body and
freshness unknown. Run without `--dry-run` to obtain source text and citations.

This explicitly changes prior CLI dry-run reads that returned source bytes after
projecting the vault. The behavior applies with and without `--no-sync`, and to
normalized and legacy catalogs. Valid nonexistent targets and ordered endpoints
inside UTF-8 characters or beyond EOF can produce plans because content checks
are unperformed. Malformed selectors, traversal paths, incomplete/reversed
ranges and invalid byte limits still refuse. Ordinary CLI/library reads and
source-citation formats retain their existing behavior. See the
[reading contract](indexed-context.md#continuing-a-captured-source-read).

## Frozen acceptance and actual replay

An independent Astra critic froze a gate of at least 9/10 with no correctness
blockers before candidate runtime outcomes. The release build overlapped
protocol preparation; the chronology correction is retained with the protocol.
The gate requires 40 previews, 16 ordinary-read controls, at most 24 setup calls
and 80 total calls, no retries, and 15 seconds per preview/read. Whole-tree
preview inventories include files, directories, root mtimes, WAL and SHM.

One owner completed **67 calls**: 11 setup, 40 previews and 16 ordinary reads.
The three disposable fixtures were a small normalized vault, a small legacy
vault and a new complete copy of the failed diagnostic's occupied 10k vault.
No occupied rebuild/full check or unchanged retrieval task recipes were rerun.
The matrix covers ID/path, verified/cached mode, JSON/human output, missing
targets, invalid requests and deliberately unperformed content endpoints.

Ordinary controls cover Unicode prefix/continuation, Current/historical/
withdrawn citations, cached bytes, authored notes, Source metadata, empty ranges,
human output and quoted continuation. The final selected same-size/same-mtime
external edit returned `FRESHNESS_CONFLICT` without evidence, before guarded
restoration of the disposable copy. Original evidence was retained.

| Measurement | Observed result |
| --- | ---: |
| Preview calls, p95 / maximum | 0.0637 / 0.0667 seconds |
| Ordinary controls, maximum | 0.0653 seconds |
| CLI intervals summed | 6.496 seconds |
| Full owner interval | 138.958 seconds |
| Ordinary returned body bytes | 33,124 |
| Complete endpoint inventories | 15 |

Owner-monotonic intervals include process supervision. Inspection consumed
76.672 seconds, including 62.214 seconds hashing approximately 26.726 GB;
inspection and hash times overlap and must not be added. Periodic process-tree
samples reached 17,530,880 bytes; exact/native peak RSS is unavailable. Accounted
allocation reached 22,696,116,224 bytes with minimum observed free space
73,984,331,776 bytes. No logical-work scaling claim follows.

Two harness issues were corrected together before any runtime: the required
inventories exceeded an arbitrary 16 GiB hash ceiling, so the bounded inspection
allowance became 32 GiB; ordinary legacy reads legitimately update the writer
lock's PID diagnostic, so that exact transition is validated and recorded.
Every preview still requires strict whole-tree equality, including the lock.
The owner also checks combined final output bounds before decoding. These
pre-outcome corrections do not waive a failed command or increase the unchanged
15-second command deadline.

Independent audit verified all 404 runtime pins, 386 compiled source/assets,
67 receipts, 134 raw-output commitments and all fifteen inventories. Eight exact
citations passed: four Current, two historical and two withdrawn, including
human metadata. Seven successful uncited controls remained uncited. Every
preview inventory pair matched exactly, as did original/copy/final preservation;
the sole ordinary legacy lock diagnostic transition was separately validated.
The audit used eleven bounded pinned hash-helper calls and no product replay.
This score applies only to the changed preview contract and related read tail.

## Build and limits

One combined native macOS ARM64 release build took 102.618 seconds. Actual CLI
and library compiler parameters show optimization level 3 and Rust edition 2024.
All 386 compiled source/config/assets remained pinned. Five affected read test
groups passed in a 3.232-second supervised checkpoint, including a 26-request
matrix with deliberately corrupted catalog data. No additional build followed
individual edits, and unchanged dense/semantic checks were reused.

Executable SHA-256:
`c344abb60daced17dbb621fb4be4367ae999e307bf4f939f5d9f5aed7818a52b`.
Endpoint equality establishes observed preservation; static routing review
supports the absence of catalog/target work. It cannot detect hypothetical
transient writes restored before observation. This scoped read correction does
not qualify representative 25k import/lifecycle, automatic HIGH, generated
answers, other platforms, live providers or power-loss safety. The latest broad
suite and strict Clippy are unqualified. Detailed evidence remains local; this
summary works without ignored artifacts.
