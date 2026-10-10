# Compact retained Change review

`changes show CHANGE_ID --summary` now provides a readable metadata overview
before exact operation inspection and guarded apply. It addresses full inspection
responses that grow with retained file contents, while preserving the existing
full-show and `--operation` JSON byte-array contracts.

Independent review accepted this functional slice at **10/10**, with all ten
prospectively frozen mandatory tasks passing and zero observed correctness
blockers. This does not qualify default retrieval, answer completeness, update
throughput, representative 25K capacity or the full 0.2.0 release. Previous failed
Page and retrieval campaigns remain failed and were not replayed or rescored.

## Behavior and authority

The separate metadata inspector checks the vault/Change binding, structural
manifest, journal and retained terminal proof when present. Recorded status takes
precedence over editable note status, which remains diagnostic. Every operation
includes its zero-based index, target, role, create/update/delete kind, expected
before and proposed after states, retained references with hashes and declared
byte counts, and application dependency indices. Read guards are included.

It does not read retained payload bodies or current targets. Both human and JSON
output explicitly mark payload availability, payload integrity and current
target freshness **not checked**. Summary does not establish apply readiness or
undo availability and cannot authorize application or an inverse. Existing
selected/full inspection verifies retained bytes separately; unresolved Changes
still require their broader body verification. Apply and recovery are unchanged.
The new flag conflicts with `--operation`. See the
[stage/review/apply recipe](source-refresh-batch.md) for actual commands.

## Validation and limits

One locked, offline release checkpoint compiled the library and relevant CLI,
manifest/journal and read-guard test executables. The recorded configuration was
Rust 1.98.0 on native macOS ARM64, optimization level 3, debug information disabled.
Compilation took 121.886 seconds in its owning process. All **30 focused tests**
passed in 33.059 seconds, including staged missing/corrupt payloads, terminal
status precedence, metadata corruption, exact retained bytes, read-only CLI
inspection, stale guards and historical apply behavior. Engine body-independence
tests covered both physical storage layouts; public sequences covered legacy
and normalized catalogs. Unchanged durable-write failure matrices were not rerun.

The independently authored replay then ran **120 public commands** once:
96 expected successes and 24 expected refusals, with no unexpected failures or
retries. All fifteen before/after inventories retained identical file hashes,
sizes and modification times. Exact selected bytes remained compatible with full
inspection, including historical output. Missing/corrupt retained bodies remained
inspectable as unchecked metadata; explicit byte inspection refused them.
Malformed manifest bindings, journals and terminal proofs refused. Help, flag
conflicts, escaped terminal controls and continuation from outside a wiki path
containing spaces and an apostrophe were checked.

| Declared disposable fixture | Actual JSON stdout |
|---|---:|
| Single Source, 64 KiB input | 3,494 bytes |
| Same shape, 1 MiB input | 3,498 bytes |
| Sixteen-member normalized refresh, 64 operations and 49 read guards | 52,340 bytes |

These satisfy the frozen fixture thresholds of 16 KiB per single summary, less
than 2 KiB difference, and 128 KiB for the batch. They are not universal output or
performance guarantees: manifest, journal, operation, guard and path metadata
still determine inspection cost. The owning replay interval was 26.016 seconds;
its observed subprocess intervals totaled 24.907 seconds. Neither is model
inference time or a shipping capacity measurement. Replay retained 453,708 output
bytes and 12,311,267 bytes across 500 disposable fixture files. No provider calls,
production vault access or holdout questions were used.

The tested production binary SHA256 is
`480291171e63b47fff90ce5793c5f5defe5e8a92345d3da8740df41beeb332b8`.
Detailed frozen protocol, source pins, raw command streams and critic reports are
optional local evidence under the ignored execution artifacts. This maintained
summary and the linked recipe remain usable in a fresh checkout.
