# Large-vault acceptance protocol

This prospective protocol accompanies the [architecture plan](large-vault-design.md).
Its thresholds are project targets, not measured capabilities or universal RAG
standards. Adopted protocol version 1 precedes the scale experiments; changes need
an explicit new version and must preserve earlier failures.

## Corpus and scope

The mandatory tier is **100,000 distinct current captured sources with one
immutable current revision each**, totaling 10,000,000,000–10,100,000,000 UTF-8
content bytes. Original copies, history, control files and indexes do not count
toward that text total. Add 1,000 authored pages and a bounded graph overlay;
report them separately. Controls are 1,000 sources / 100 MB and 10,000 / 1 GB.

Use seeded, varied synthetic text with frozen size histograms, per-source hashes,
non-ASCII content, distractors and facts at different positions. Write actual
payloads; sparse files, repeated padding and a preloaded SQL fixture cannot stand
in for end-user import. Source facts belong in the documents; expected answers,
facet labels and query metadata stay outside the index. Include a fixed licensed
public development overlay for realistic retrieval, with separate quality scores.

Measure both initial state and churn: refresh 1% of sources, withdraw 0.1%, retain
old revisions. Count every eligible retrieval unit under a frozen segmentation
policy. Mock vectors can test full-unit mechanical capacity, but cannot establish
semantic relevance. A lexical scale pass does not qualify semantic/hybrid modes.
Full-tier paid embedding acquisition is not part of this protocol.
All scale runs are offline with zero live provider calls.

## Environment and resource envelope

Initial native host: Apple M1 Max, ten CPU cores, 32 GiB RAM, APFS. Record current
OS, free space, power/background conditions and binary hash for each run. Leave
at least 32 GiB free; the entire disposable experiment may occupy at most 100 GiB
physical storage, including input, canonical copies, history, indexes, WAL,
temporary files and controlled backups. Bulk operations allow at most 8 GiB peak
process-tree RSS. The full run allows 24 hours; no command may exceed four hours.

Run 1k and 10k controls before allocating the full tier. Stop safely if projected
or observed disk, memory or time exceeds the envelope. Such a stop is an
incomplete/failing attempt. Preserve evidence, immutable history and unknown
accounting; only known disposable fixtures may be removed.

## Mandatory workflow gates

| Task | Required result |
| --- | --- |
| Initial documented batch import | Every intended source is discoverable and byte-exact; full tier completes within four hours and 8 GiB RSS, including publication/sync. Logical read/decode/publish work grows at most 15× from 10k to 100k. |
| Fast generation-scoped queries | Warm p95 ≤5 seconds; every query and first query after publication ≤15 seconds; process-tree RSS ≤1 GiB. No whole-corpus parse/hash/reconstruction or exhaustive vector scan per query. |
| Managed incremental changes | No-op ≤5 seconds; one approximately 100 KiB add/refresh/delete/withdraw ≤60 seconds each; 1,000 changed documents ≤10 minutes. Fixed-fanout logical rows, payload bytes and filesystem visits grow at most 2× from 10k to 100k. |
| Strict reconciliation plus cited query | Unchanged valid full tier succeeds within 30 minutes and 8 GiB RSS. Insufficient-budget and adversarial cases refuse false verification. |
| Offline rebuild after derived-index removal | Completes within four hours and 8 GiB RSS; canonical identities, eligibility and exact-search results agree. Ignore only declared generation/timestamp bookkeeping in logical comparisons. Zero provider calls. |
| Interrupted publication and resume | Existing native fault suite plus a representative full-tier interrupted batch; only complete generations visible, no duplicate commit, lost revision or accounting loss. Recovery ≤30 minutes, excluding remaining initial import, which still counts toward its four-hour active-runtime limit. |
| Independent user walkthrough | Published import/query/update/reconcile/recovery commands work from outside a vault with spaces in its path. JSON/text labels and continuation instructions are accurate; dry-run changes no bytes/mtimes; offline makes no network calls. |

Queries use 100 frozen tasks, with three deterministic interleaved repetitions at
both 10k and 100k, before and after churn. Use 20 tasks each for exact IDs,
multi-term distractors, distant evidence, two-source evidence, and restrictive
filters/absent facts. Freeze candidate/output limits across tiers. Every exact-ID
task must succeed; every citation and freshness label must be correct. Report
fact completeness separately. All expected-success requests must avoid
operational errors; every negative case must produce its specified safe outcome.
Before rebuild comparisons, enumerate the exact bookkeeping fields excluded
from equality. Preserve every substantive projection field, source/revision ID,
eligibility, dependency and deterministic ranking result.

## Freshness, corruption and recovery

Strict mode retains its existing global verification meaning. A distinct fast
workflow may claim eligibility/discovery at generation G and verify selected
canonical bytes now. It must expose G, its observation boundary, proof scope,
omissions and the absence of a global-currentness/completeness claim. It cannot
reuse `verified_snapshot` or silently weaken the default.

Test same-size/same-mtime edits, selected and unselected control changes, new
duplicate IDs, unpublished sources, refresh/withdrawal, changed dependencies,
deletion/rename, edits after selection, traversal errors, selected row corruption,
unselected cache omissions and interrupted publication. Test watcher continuity
loss if a watcher is used. Replay representative duplicate/edit/omission attacks
at full tier. Selected evidence changes must fail the fast proof; unrelated
changes cannot be claimed globally absent. Strict reconciliation must detect
them or refuse. No observed-filesystem check implies atomicity against arbitrary
concurrent editors.

If approximate vector retrieval is introduced, compare at least 100 frozen
queries with exhaustive exact neighbors at both tiers. Mean recall@20 must be
≥0.95 overall and ≥0.90 in every declared filter/churn stratum. Eligibility,
space isolation and citation correctness permit no stale or mismatched evidence.
Neighbor recall is separate from complete-answer coverage.

## Evidence and critic

### Development diagnostic for validation memory

[The scaling example](../examples/catalog_scaling_probe.rs) directly generates
synthetic canonical files and measures the public graph validator in a fresh
process. Its `audit` command checks exact fixture membership, deterministic
bytes, manifests and source ownership. A separate `project` command checks full
graph expectations and reports retained retrieval text and dependency duplication.
This diagnostic bypasses managed import, publication and recovery; it does not
qualify the acceptance workloads above.

On macOS, build `//:catalog_scaling_probe` with the repository's Bazel wrapper,
then run the [supervisor](../scripts/catalog_scaling_probe.py) from the repository
root:

```sh
mkdir -p .artifacts/catalog-scale
python3 scripts/catalog_scaling_probe.py \
  --binary "$PWD/bazel-bin/catalog_scaling_probe" \
  --account-root "$PWD/.artifacts/catalog-scale" \
  --output "$PWD/.artifacts/catalog-scale/smoke-001" --stage smoke
```

Every output directory must be new. After smoke passes, use `--stage 1000` with
a new output directory. For `--stage 10000`, also pass `--baseline` pointing to
the successful 1k directory; keep all runs beneath the same account root. The
supervisor refuses 10k unless the frozen time, memory and disk admission margins
fit. It never runs full projection at 10k. Failed and refused runs remain on disk.

To retest an unchanged fixture after a fix, add `--reuse-fixture-from` pointing
to its previous run directory and choose a new output directory. This separate
inspection protocol verifies the retained inventory against the previous report
and performs no generation. Establish a fresh 1k baseline with the new binary
before admitting 10k. Existing fixture allocation counts toward the same disk
ceiling, with an additional 256 MiB reserved for reports; this replaces the
new-fixture allocation forecast, not the time, memory or disk limits. A prior
validation failure is preserved and does not prevent reuse of an audited,
unchanged fixture.

Each source has exactly 100,000 UTF-8 content bytes plus a separate original
copy. All tiers retain 1,000 pages and one fixed small graph, so this measures
source-count growth, not graph-fanout growth. Caps are 900 seconds per child
command, 8 GiB child-process-tree RSS, 4 GiB allocated diagnostic disk and at
least 32 GiB free space. Polling can overshoot; native RSS also checks short-lived
peaks. Python supervisor memory is outside the child RSS measurement. Pre/post
hash passes warm the filesystem; Rust phase time, loading time and externally
observed child duration are separate fields, and the latter includes polling
delay. A validator call performs both proposed and baseline projections, so its
time is not directly comparable to one full projection as an optimization ratio.

### Actual refresh command diagnostic

[benchmark_source_refresh.py](../scripts/benchmark_source_refresh.py) runs the
public CLI on disposable captures, then measures no-op, title-only and changed
refreshes followed immediately by indexed context. Supply an explicit binary and
a new output directory; `--baseline-binary` compares two binaries on copies of
the same seeded canonical state.

```sh
python3 scripts/benchmark_source_refresh.py --binary /absolute/path/to/lwiki \
  --workdir /absolute/path/to/new-run --tier tiny --trials 1 --history-revisions 1
```

The two-document, 100 KiB smoke run passed all three refresh/context checks on
the legacy public workflow: refresh took 0.170 seconds unchanged, 0.857 seconds
for a title edit and 1.494 seconds for changed content; subsequent context took
0.072–0.093 seconds. These single observations validate the harness and expose
remaining refresh cost. They do not establish tail latency. The normalized update
path is now connected for an already selected normalized catalog; the measurements
above describe the earlier legacy path.

Setup uses real `source add` commands and is timed separately; its current cost
can grow quadratically, so the larger tiers may hit the explicit run ceiling.
The harness preserves raw commands, outputs, binary hashes, resource observations
and failures. It checks selected revision identity, immutable bytes and exact
cited spans. Independent quote-hash recomputation requires Python's optional
`blake3` module and is explicitly unqualified when unavailable. Held-reader and
logical filesystem/SQL work measurements still require the separate integration
tests. This diagnostic does not replace the full acceptance matrix below.

For the normalized path, [export_refresh_fixture.py](../scripts/export_refresh_fixture.py)
supervises an explicitly supplied, SHA-256-pinned unit-test executable. Its ignored
export test formats real capture records, builds and publishes the normalized
catalog once, closes its handles, then hashes every vault file including SQLite
sidecars. This avoids running legacy import repeatedly just to prepare a diagnostic;
it does not measure public import throughput. Build `//:unit_tests` and `//:lwiki`
from the same source, copy both executables into a new disposable account directory,
and retain their hashes and build provenance.

```sh
python3 scripts/export_refresh_fixture.py \
  --unit-test-binary /absolute/account/bin/unit_tests \
  --binary-sha256 ACTUAL_SHA256 \
  --workdir /absolute/account/export-1k --source-count 1000
python3 scripts/benchmark_source_refresh.py \
  --binary /absolute/account/bin/lwiki \
  --preseed /absolute/account/export-1k/fixture \
  --account-root /absolute/account --workdir /absolute/account/run-1k \
  --tier 1000 --bytes-per-source 100000 --trials 5 \
  --command-seconds 120 --run-seconds 1800 --max-disk-gib 8
```

The preseed route requires Python `blake3` (the recorded run used 1.0.8), verifies
every pinned file before use, checks the clone before opening SQLite, and rechecks
the untouched original afterward. Account-root allocation includes seeds, clones,
binaries and reports. Output directories must be new and separate from the seed.
Run a two-source export and `--tier tiny --trials 1` first. Admit 10k only after
measuring 1k time, memory and allocation with headroom; use new directories and an
explicit disk ceiling. Export supervision bounds its own workdir, while benchmark
supervision accounts for the entire account root. Both preserve failures and
enforce a 32 GiB free-space floor. Resource sampling can overshoot its limits.

The 1,000-source normalized diagnostic (100,000 content bytes per source, zero
initial history and no authored fanout) passed all 15 measured refresh/context
pairs on the development M1 Max. Exact current and retained bytes, revision reuse,
publication epochs, cited spans and Blake3 quote hashes passed. Five warm trials
per case produced these whole-command times:

| Command | Median | Maximum / sample p95 |
|---|---:|---:|
| Unchanged refresh | 36 ms | 39 ms |
| Title-only refresh | 403 ms | 430 ms |
| Changed-content refresh | 844 ms | 891 ms |
| Following context, across cases | 47–48 ms | 51–52 ms |

Fixture export took 37.4 seconds with 150 MB native peak RSS and 533 MB summed
allocated blocks. The complete benchmark took 35.0 seconds, including cloning and
verification, and left 1.25 GB allocated across the account. Measured refresh and
context peaks were 35.5 MB and 28.0 MB respectively. No timed inventory sweep
overlapped these commands. Five samples provide no tail-confidence claim; changed
trials accumulate retained history. Filesystem metadata and index pages are warm.
These results meet the prospective one-second update target at 1k, but do not
qualify 100k capacity, logical-work scaling, all query modes or semantic quality.

The same pinned binary and protocol subsequently passed all 15 pairs at 10,000
sources (1 GB current content). Unchanged refresh had median/max 41/43 ms,
title-only 692/699 ms, and changed content 1.604/1.722 seconds. Following context
had medians 49–51 ms and maximum 54 ms. **Changed refresh missed the prospective
one-second target**, despite meeting the frozen acceptance ceiling. This warrants
investigating update-path work before extrapolating to 100k; a passing diagnostic
status does not mean every UX target passed.

The 10k export took 415.3 seconds, with 781 MB native peak RSS and 5.18 GB allocated;
the benchmark took 319.2 seconds including setup and verification. The complete
account, including earlier fixtures, used 11.64 GB. Export time grew 11.1× for 10×
content. These fixture and warm-command measurements remain development evidence,
with the same exclusions as the 1k result.

The test-only [path profiler](../scripts/profile_source_refresh.py) isolates the
logical filesystem work behind this result. It copies and verifies a closed
export, runs one no-op, title-only and content update through the real application,
and verifies immediate citations. Supply the matching native unit-test executable
and its SHA-256, rather than the production CLI binary:

```sh
python3 scripts/profile_source_refresh.py \
  --binary /absolute/account/bin/unit_tests --sha256 ACTUAL_SHA256 \
  --preseed /absolute/account/export-1k/fixture \
  --account-root /absolute/account --workdir /absolute/account/profile-1k \
  --tier 1000
```

Use a two-source export with `--tier tiny` first, then independent new output
directories for 1k and 10k. The runner requires Python `blake3`, caps the combined
account at 20 GiB by default (`--max-disk-gib` can explicitly raise it, up to the
100 GiB experiment ceiling), retains a 32 GiB free-space floor, and bounds setup plus execution
to 30 minutes and the child to 120 seconds/8 GiB RSS. It preserves all failures and
qualifies phase timings when its account inventories overlap the child. This
direct application diagnostic excludes CLI argument/configuration overhead and
does not replace the actual-command benchmark.

The measured source-directory work was:

| Update | Directory enumerations | Entries at 1k | Entries at 10k |
|---|---:|---:|---:|
| No-op | 0 | 0 | 0 |
| Title-only | 12 | 12,000 | 120,000 |
| Changed content | 30 | 30,000 | 300,000 |

All three cases at each tier passed revision, epoch, payload and citation checks.
The counts establish corpus-linear work in ordinary updates. Both larger profiling
runs overlapped supervisor inventory scans, so their phase times are not accepted
as clean attribution evidence. The independent critic accepted the counts as
sufficient evidence to remove the unnecessary source-directory enumeration; the
post-change CLI benchmark must establish the resulting latency. This profiling
checkpoint itself does not change production path validation.

The subsequent published-path optimization removes that source-root census for
admitted source refreshes while preserving checks for new namespace components
and selected files. Both storage layouts passed immediate and staged-apply tests.
The regression gate passed 475 unit and 105 filesystem, recovery, offline CLI and
source-evidence integration tests: 580 passed, nine ignored, with the two unchanged
ledger matrices excluded. Invalid-UTF-8 filename coverage is Linux-only; native
macOS coverage uses representable folded spelling and real collision cases.

The candidate CLI (SHA-256
`a8e9399968397e0f8a0136cf5e0b5b2c560bae85d06b9a6774d0f04a8b314362`)
passed all 15 refresh/query pairs at each tier using the original closed fixtures
and five-trial protocol. The baseline below is the earlier recorded run, not a
contemporaneous randomized comparison. Values are median / maximum milliseconds:

| Sources | Update | Baseline | Candidate |
|---:|---|---:|---:|
| 1,000 | No-op | 36 / 39 | 40 / 49 |
| 1,000 | Title-only | 403 / 430 | 364 / 388 |
| 1,000 | Changed content | 844 / 891 | 785 / 917 |
| 10,000 | No-op | 41 / 43 | 83 / 90 |
| 10,000 | Title-only | 692 / 699 | 410 / 420 |
| 10,000 | Changed content | 1,604 / 1,722 | 879 / 909 |

All candidate prospective targets passed, including changed-content p95 at most
one second; with five samples p95 equals the maximum. Context maxima increased
from 52 to 59 ms at 1k and from 54 to 106 ms at 10k. No inventory scan overlapped
the timed CLI commands. Candidate native refresh/context RSS peaked at
35.6/28.0 MB for 1k and 37.3/28.2 MB for 10k.

To investigate the no-op/context increase, a separate 20-command check alternated
both pinned binaries on the same completed 10k candidate vault, with identical
input, epoch and exact returned context. Five paired samples per mode, without
concurrent process polling or inventory, gave context medians of 46.19/46.20 ms
and no-op medians of 37.44/35.74 ms (baseline/candidate). Context maxima were
98.66/83.90 ms. Selected canonical and authority bytes remained unchanged. This
did not reproduce a sustained binary regression; it does not establish the cause
of the earlier difference or erase those measurements. Its different monitoring
protocol is suitable for that paired check, not substitution into the table.

Independent candidate counter runs at tiny, 1k and 10k all passed revision,
payload, epoch and citation checks with **zero source-root directory opens or
entries** for all three update cases. The larger profiler runs overlapped account
inventories and remain count evidence only. The complete retained experiment
account used 29.01 GB, within an explicitly raised 32 GiB ceiling and the unchanged
32 GiB free-space floor. Whole CLI benchmark runs took 423 and 633 seconds,
including untimed fixture verification and account inventories; these durations
are not document-update latency.

Selected-source revision allocation and generic operational-directory checks can
still grow with their respective histories. These results do not qualify 100k
capacity, authored fanout, all query modes, cold-cache tails or unseen semantic
completeness. See the [published-path storage contract](technical/storage.md).

### General lexical and snapshot-context diagnostic

The [general-query runner](../scripts/benchmark_general_queries.py) copies only
canonical files from a verified closed seed into a new owned fixture, then uses
the real normalized builder and public CLI. It adds 133 fixed authored records
and executes 14 query cases five times each: identity/title/alias, Unicode,
popular and rare terms, selective filters, long metadata, stale identity, and
single- and multisource document context. A changed-source workflow then checks
immediate discovery, snapshot context and exact indexed-evidence citations.
Fixture construction is not public import/rebuild qualification.

The implementation at `19e8eb4` returned all expected results in **70/70 trials
at each tier**. Both refresh workflows passed, as did canonical read-only
bindings, original-seed verification and binary/script pins. The frozen
per-case nearest-rank p95 target was five seconds; with five samples this is
the maximum. The **1k diagnostic passed; the 10k diagnostic failed latency**.

| Sources | Worst query | Changed refresh | First search after refresh | Snapshot / indexed context |
| --- | ---: | ---: | ---: | ---: |
| 1,000 | 1,615 ms | 893 ms | 27 ms | 30 / 45 ms |
| 10,000 | **7,367 ms (failed)** | 979 ms | 29 ms | 30 / 46 ms |

At 10k, the first Unicode-alias query caused the failure; its five-call median
was 726 ms. The query also contains a term present in every captured source,
so the FTS candidate leg scores a broad match population. The failure is retained;
subsequent faster calls do not turn it into a pass. Query native peak RSS was
51.7 MiB at 1k and 51.4 MiB at 10k. Runs used fresh CLI processes after fixture
construction and hashing, with uncontrolled OS cache state and all first calls
included. They do not establish cold-cache performance or semantic completeness.

Run the finite diagnostic with Python `blake3` installed and explicitly pinned
CLI and unit-test binaries built from the same source:

```sh
python3 scripts/benchmark_general_queries.py \
  --binary /absolute/account/bin/lwiki \
  --binary-sha256 CLI_SHA256 \
  --unit-binary /absolute/account/bin/unit_tests \
  --unit-sha256 UNIT_SHA256 \
  --account /absolute/account \
  --preseed /absolute/account/export/fixture \
  --workdir /absolute/account/new-general-query-run \
  --tier 1000
```

Use a new work directory for each run. The runner retains raw output and failed
reports, caps combined account allocation at 40 GiB, requires 32 GiB free, and
limits each query to 15 seconds and 1 GiB native RSS. Query timing excludes
inventory scans. The two measured runs used runner SHA-256
`cb93e29d76fb6056a2bb15fac10c1e0e68cdbda15a6db1e6298287cfc23de67d`.
The 10k latency failure remains failed; a fresh fixed-version test is required
before this package can receive performance approval.

A subsequent read-only mechanism probe isolated eligibility access from FTS
matching and early metadata access. It used the existing owned fixtures after
refresh (epoch 2), the same bundled SQLite with mmap disabled and an 8 MiB pager
cache, fresh connections, and two counterbalanced query orders. All scalar
match/rank checks and production candidate-order comparisons agreed.

| Matching captured revisions | Early metadata pager misses | With eligibility | Additional misses per capture |
| --- | ---: | ---: | ---: |
| 1,001 | 1,062 | 49,112 | 48.002 |
| 10,001 | 10,162 | 490,217 | 48.001 |

Both orders produced the same counts. The eligibility column follows large body
and raw-text values in the SQLite record, so reading that small field traversed
payload overflow pages for every matching capture. The production FTS candidate
leg showed comparable demand: 490,223 misses before selected payload consumption
at 10k. This establishes avoidable pager work, not physical disk reads or the
precise contribution to the original 7.37-second delay. The experiment's first
10k eligibility scan took 7.31 seconds and its reverse-order scan 0.57 seconds,
despite identical pager counts; OS cache and waiting remain relevant.

The critic approved a candidate metadata index for implementation, followed by
focused no-tag/tag-filter pager tests and the unchanged public CLI benchmark.
It excludes document bodies, raw text and tag arrays. Ranking, eligibility and
pre-limit filtering must remain identical. This approval is a design decision,
not a passing performance result.

The implemented candidate passed a 644-test correctness gate (505 unit and 139
integration tests; eight ignored and the same two unchanged ledger stress
matrices excluded). A focused real-builder regression reduced pager misses from
4,062 to 20 without a tag filter, and from 4,062 to 33 with a shared 4 KiB tag.
Selected keys, scores and ordering stayed identical. SQLite bytecode confirmed
that tag filtering reads only the table's tag column while eligibility and kind
come from the index. VM steps increased slightly; this is page-access reduction,
not uniformly reduced SQL work. Public title-only and new-revision refresh tests
also passed.

Fresh fixtures tested the fixed implementation at `2fc6731` with the same pinned
runner, questions, expectations and budgets. **Both the 1k and 10k diagnostics
passed all 70 query trials and the refresh/citation workflow.** Original-seed,
read-only binding and binary/script checks passed. The original failed run is
retained separately.

| 10k query case | Before median / maximum | Fixed median / maximum |
| --- | ---: | ---: |
| Unicode alias with broad FTS matches | 726 / **7,367 ms** | 195 / 344 ms |
| Popular term | 785 / 977 ms | 268 / 444 ms |
| Selective filter with no result | 651 / 728 ms | 217 / **1,369 ms** |

The selective-filter maximum increased even though its median fell; all five
samples remain included. It was the slowest fixed 10k query and stayed within
the unchanged five-second per-case target. At 1k, the worst call was the first
exact-ID query at 1,745 ms; the Unicode case peaked at 204 ms. These are fresh
processes with uncontrolled OS cache, not an OS-cold benchmark.

The single changed-document workflow took 879 ms at 1k and 960 ms at 10k, followed
by immediate searches in 26 and 28 ms. At 10k, post-refresh snapshot/indexed context
took 31/47 ms. Native query RSS peaked at 44.4/47.3 MiB for 1k/10k. The final account
held 35.77 GB allocated; an older completed disposable profiling vault was retired
before these runs, with its reports, hashes and original seed preserved. No
inventory scans overlapped timed queries.

Independent review accepted the scoped general lexical/snapshot package at
**9.2/10 with zero blockers**, verifying raw outputs, case statistics, native RSS,
binary/script pins, publication bindings and selected canonical spans. These
diagnostic results do not
qualify public normalized rebuild/sync/check, all retrieval modes, the 100k/10 GB
workload or unseen semantic completeness.

### Public reconstruction checkpoint

The opt-in `index rebuild --normalized` route now uses shared parsed notes,
streamed retrieval rows and metered named source/layout reads. After activation,
plain `index rebuild` keeps that layout and explicit `index sync` preserves an
unchanged current publication. Compatibility checks include proof/ownership
versions and exact bounded schema definitions; sync does not run an integrity
audit or trust a historical full-build digest.

The broader local gate passed **730 tests**: 542 unit and 188 integration tests
across CLI, retrieval/context, graph, storage cleanup and source evidence.
Eight tests remained ignored and two unchanged accounting stress matrices were
excluded. A subsequent cleanup-reporting correction passed **32 focused unit
tests and one real CLI test**. Both gates retained unchanged source pins during
execution. An earlier missing-payload diagnostic mismatch and a missing test
import were retained as failures, corrected and retested.

Independent Astra review accepted this rebuild/sync/recovery checkpoint at
**9.1/10 with zero scoped blockers**, checking the ten broad logs, final focused
results and final source pins. That score applies only to this checkpoint.

Coverage includes exact interrupted-candidate recovery, an external edit during
rebuild, readers held across publication, source title/revision updates followed
by unchanged sync and authenticated citations, missing-cache reconstruction,
older capability layouts, partial-build disposal and dry-run filesystem equality.
Retained-layout tests use a real migration and confirm that 100 mapped reads do
not reread its plan. Unsafe cache paths still refuse reconstruction; damaged
predecessors and reader-held files can remain, with deferred cleanup reported.

This checkpoint does not qualify native rebuild throughput or RSS, automatic
collection of every abandoned file, complete normalized `check`, bounded
`doctor`, all command modes, 100k/10 GB capacity or unseen semantic completeness.

### Acceptance evidence

Freeze the binary/tree, protocol, generator/seed, manifests, expected outcomes,
budgets, CLI arguments, cache/space state and ordered tasks before acceptance.
Measure monotonic subprocess start-to-exit time, including setup/proof/render.
Use fresh CLI processes; “warm” means warmed filesystem/index pages. Do not claim
OS-cold timing without controlling the cache. Report all durations and nearest-rank
p50/p95/p99/max, plus every failure/timeout. Measure process-tree peak RSS with
documented native units, including resident mapped pages. Record logical and
allocated disk bytes, peak free-space use, SQL/filesystem work and amplification.
Keep exact argv, raw outputs, hashes, resource samples and failure locations.
For host-assisted queries, record preparation, selector turnaround, final
validation and total end-user latency separately, along with visible input bytes,
calls and known usage/cost. Local CLI timing alone is not host-assisted latency.

An independent critic must score at least **90/100**, with every mandatory gate
passing and no correctness blocker: capacity/import/rebuild 25 points;
freshness/integrity/recovery/accounting 25; query resources/incremental behavior
25; reproducible evidence 15; usable commands/accurate claims 10. Unexecuted
full-tier tasks receive no passing credit. False verification, data/accounting
loss, hidden exclusions or semantic overclaims block HIGH regardless of score.

The separate frozen unseen completeness gate remains unchanged: 18/20 complete,
exact 7/9, paraphrase 5/6, multisource 4/5, new-document 3/4 (overlapping category),
four unsupported absent controls, correct citations and semantic critic ≥9/10
with no blockers. Neither gate substitutes for the other; distinguish automatic
and host-assisted workflows and report unknown model usage as unknown.
