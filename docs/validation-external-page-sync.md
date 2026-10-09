# External Page synchronization validation

Ordinary [`index sync`](external-page-sync.md) supports bounded external authored
Page creation, editing, deletion and restoration on normalized catalogs.
Independent Astra functional acceptance was **9.5/10**, with all seven mandatory
tasks passing on both original and retained layouts and no observed blocker.
The native companion and both declared 10K timing tasks also passed. Default
retrieval quality, the five-second no-op target, representative 25K capacity and
full release acceptance remain open.

The production executable was native macOS arm64, optimization level 3 with debug
information disabled, SHA256
`398229c96b305cc166f84cd80fb379719fbfbbfd1036bb4acafddfb114404679`.
The final native test executable was
`d997e94970a39be9ea0bd359c05be3423ef66d806870c7302a68b1e08cfa7ecc`.
All 511 frozen inputs of that native test build match the integrated source tree.
Later production-source changes were confined to tests and test-only sentinels;
the qualified production executable was reused.

## Public workflow and native companion

Seventy-five offline CLI calls exercised creation, equal-size edits with restored
mtimes, deletion/restoration, incoming links, current/historical citations,
reviewed evidence and a cited companion Page. Guarded writes, stale-author refusal,
expected invalid-support diagnostics, reconstruction and final complete integrity
checks passed. Independent checks authenticated 30 citation occurrences, ten
context spans, 18 preserved files and both final managed Pages. Distinct native
owning intervals summed to 8.269 seconds, excluding orchestration and review gaps.

All **32 durability cells** passed: eight reached cuts, two layouts, and returned
error/native SIGKILL. Twenty-five new cells were combined with seven unchanged,
source-qualified prior cells. Recovery preserved canonical/unfamiliar files,
selected the correct authority, matched complete logical catalog/FTS oracles and
remained repeatable. Copy witnesses distinguish the first of six backup steps from
completion over 1,293 SQLite pages. Retirement follows actual owned-file removal
and entry into directory sync. These checks do not simulate power loss or qualify
other platforms. See the [native tests](../src/catalog/maintenance_page_delta_tests.rs).

Dry sync/rebuild preserved bytes, mtimes and authority on both layouts. Offline
selected sync attempted no named credential/helper/provider boundary. Positive
controls reach real scan, writer, maintenance, SQLite, credential, helper,
dispatch and transport entrypoints; this is not interception of every OS syscall.
Authenticated public APPLY refused maintenance-only deletion, preserving canonical
content, staged records, publication and selection. Only writer-lock diagnostic
mtime changed, with identical bytes. Distinct native owner time was 38.025 seconds,
including qualified prior evidence and failed guard attempts once each. Earlier
failed builds/full suites remain failed; this is scoped acceptance.

Public Page reads do not expose internal link-resolution state. Actual target
absence/restoration and preserved authored links were observed separately; native
checks cover exact relations. Selected-membership and unknown-vector-cache
warnings remain disclosed. No full retained-payload audit or provider compatibility
follows from these checks.

## Separate 10K timing gate

A new disposable copy matched all 91,989 files and 32,909 directories of the
protected post-churn seed, including bytes, modes and mtimes. Source/copy/source
qualification took 100.048 seconds and preserved the seed. It contained 10,002
Sources, 10,103 revisions and 1,000 Pages: 7,981,127,123 logical file bytes, including
a 2,561,953,792-byte catalog. This generated retained corpus does not establish
representative public-import capacity.

The prospectively bound existing draft Page was **2,664 bytes**. All thirteen
offline calls passed, including deleted-ID refusal, absence/restored search, full
restored Page/SourceRefs, draft context exclusion, incoming-target checks and four
exact current/historical 1,024-byte Source citations. Named incoming, unrelated,
outgoing and Source assets preserved bytes, modes and mtimes. Post-task preservation
was sampled, not another complete corpus audit.

| Whole task, including validation and receipts | Seconds | Gate |
| --- | ---: | ---: |
| Delete and validate | 37.254 | ≤60 |
| Exact restore and validate | 28.364 | ≤60 |

Both syncs reported `build: null`, exactly one deletion/creation and zero edits.
Their disjoint native phases were:

| Phase | Delete seconds | Restore seconds |
| --- | ---: | ---: |
| Initial comparison | 15.185 | 7.483 |
| Coherent selected-catalog copy | 4.749 | 4.848 |
| Candidate sealing | 2.801 | 2.739 |
| Selected projection | 0.030 | 0.024 |
| Final input recheck | 14.072 | 12.729 |
| Publication | 0.048 | 0.045 |

The task owner measured 65.621 seconds; its external resource supervisor measured
65.865 seconds. These overlapping intervals are not added. Streams totaled 25,760
bytes and auxiliary reads 983,627 bytes. Children were reaped; no provider call or
recovery occurred. Observed host free space stayed above 126,139,801,600 bytes,
against a 40 GiB floor/margin/competing reservation. Post-run allocated charges were
8,242,970,624 bytes against a 12 GiB account. Peak allocation and RSS were not
measured; logical counters do not establish exclusive physical I/O.

A prior attempt failed because its controller's 2 MiB `RLIMIT_FSIZE` also
restricted SQLite writes. Its target remains closed, earning no product timing
credit. Concurrent bounded output pipes replaced that limit after dual-stream,
overflow, timeout and greater-than-2-MiB-file controls. A pre-copy pin-coordination
failure is also preserved. Neither result rescores the earlier 90KB Page failure
or the failed no-op target.

Independent actual acceptance SHA256:
`0688f4dba085dddf110dbba87e127c45c387ead04f9f1c8da3d06cb44a3f1e0d`.
No paired speedup, 90KB-at-10K, worst-case closure, default retrieval gain or 25K
capacity follows. Further performance work should address measured comparison and
recheck cost before adding isolated storage machinery.

## Rejected resolver speed candidate, 2026-10-09

A later experiment replaced repeated macOS destination canonicalization with an
all-component no-follow, metadata-only lookup at the same inspection boundary.
Object identity checks and the original canonicalization fallback remained;
payload authentication, nested-vault checks and final input rechecks were unchanged.
The candidate is **unpromoted**. It passed its scoped correctness checks but failed
the prospectively declared performance gate.

Both binaries were native ARM64 release builds with optimization level 3 and debug
information disabled. They alternated on the same occupied 10K vault, unchanged
generation and cache. All three no-op pairs are retained:

| Pair | Accepted binary seconds | Candidate seconds | Improvement |
| --- | ---: | ---: | ---: |
| 1 | 27.903 | 26.898 | 3.60% |
| 2 | 27.839 | 26.773 | 3.83% |
| 3 | 28.439 | 28.413 | 0.09% |

The median improvement was **3.60%, failing the required 20%**. Both binaries
failed the five-second no-op target. These are three supervised observations,
not a p95 distribution; supervisor activity remains included. A prior sample's
39.9% canonicalization occupancy did not establish that removing that operation
would yield a comparable elapsed-time gain. The complete scan's logical work
remained unchanged.

All ten offline commands passed their correctness checks. A 177-byte external
Page edit preserved its size and restored its timestamp; synchronization,
selected search and the actual returned-path read completed in **53.711 seconds**,
passing the 60-second limit. The old marker disappeared. Complete initial and
final audits preserved all 92,514 original canonical files; membership was exactly
those files plus the single edited Page. The grouped native checkpoint passed
79 tests in three parent harnesses, with one additional subprocess helper. It
covered shared maintenance and read workflows, containment races, nonblocking
special-file replacement and namespace parity; it was not a full repository suite.

Candidate executable SHA256:
`feae3e3a951dd7f1c7d39de1c4da5d78c191234596df21c0b5553fcbb39a6a91`.
Collector result SHA256:
`93bcbe84124422a2a37b7289a0c53f96c2adfc294e49fbb6dce165f00f7fe69f`.
Root seal SHA256:
`3a703d1dd43ca70f2e1726b951ad7827aab782b91da0cb5925a364732b0e4141`.
Detailed failed attempts and the candidate remain preserved locally. Default
retrieval quality, representative 25K capacity and full release remain open.

Independent Astra review accepted these scoped correctness receipts and rejected
speed promotion. The next milestone uses the accepted executable and supported
import group size eight in one bounded 25K workflow attempt. A separate group-size
benchmark and further resolver tuning are deferred. This configuration changes no
default and earns no speedup claim. Import/read/update/recovery progress will be
reported separately from full capacity acceptance, which still requires the
[corpus, quality and operational gates](testing-large-vaults.md).
