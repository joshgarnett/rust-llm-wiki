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
OS, free space, power/background conditions and binary hash for each run. Verify
and record actual compiler optimization settings. Release-performance qualification
uses `--config=release` (Bazel `opt`, Rust optimization level 3); default `fastbuild`
uses Rust optimization level 0 and is only an unoptimized diagnostic. Leave
at least 32 GiB free; the entire disposable experiment may occupy at most 100 GiB
physical storage, including input, canonical copies, history, indexes, WAL,
temporary files and controlled backups. Bulk operations allow at most 8 GiB peak
process-tree RSS. The full run allows 24 hours; no command may exceed four hours.

Cargo's own benchmarking profile [inherits its release profile](https://doc.rust-lang.org/cargo/reference/profiles.html#bench).
Here, retain the actual Rust compiler actions with executable pins; an output
filename alone does not establish which optimizations were enabled.

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

### Bounded doctor checkpoint

`doctor` now observes the selected header and constant-size operation slot without
scanning canonical notes or retained history. JSON reports unperformed audits and
unknown canonical freshness; plain output explains those limits and supplies a
command for canonical diagnostics. Dry-run skips cache and provider observation.
Legacy inspection uses a nonblocking shared lock on the existing writer lock,
held through SQLite close. Missing/busy locks and ambiguous WAL sidecars return
`present_uninspected`; unsafe present paths remain errors.

The broader native run passed **642 tests** and failed one generated command
manifest check. Regenerating the reference from the actual CLI corrected that
failure, including previously missing normalized rebuild and indexed-evidence
help. The broad run excluded two unchanged accounting stress matrices and left
eight tests ignored. Its failure remains recorded; it is not an all-green gate.

Independent review found and corrected a legacy sidecar race, including database
replacement before lock acquisition. The final focused run passed **30 tests**:
13 doctor and six skill unit tests, seven actual doctor CLI tests and all four
skill-export integration tests. Source pins remained unchanged during each run.
The earlier unit assertion that confused semantic diagnostics with a failed scan
was also corrected and retested. Formatting checks passed for changed Rust files.

Independent Astra review accepted this doctor checkpoint at **9.2/10 with zero
scoped blockers**. Tests cover a 16 MiB legacy projection with scalar-only header
access, malformed unrelated notes/history, active or damaged normalized authority,
readable headers with unaudited rows, dry-run filesystem equality, and deterministic
WAL/DELETE writer contention and replacement boundaries. SQLite SHM read marks may
change during a permitted read; cooperative SQL limits are not hard OS deadlines.
This is native macOS evidence, not Windows, 100k capacity or semantic qualification.

Reproduce the final focused selection after hydrating the build dependencies:

```sh
python3 scripts/bazel.py -- test //:unit_tests //:offline_cli_test //:skill_export_test \
  --test_arg=doctor --test_arg=skill --test_arg=--test-threads=1 \
  --test_output=errors --nofetch
```

At this doctor checkpoint, complete normalized `check`, remaining modes/default
activation, the full large-vault workflow and the unseen completeness gate were
still outstanding. The following checkpoint adds explicit checking.

### Explicit normalized check checkpoint

`check` now reaches a read-only reconciliation of current canonical input with
all 18 normalized record/fact/owner families and the complete document and graph
search indexes. It holds the writer permit, pins the selected read transaction,
reconstructs retained revision owners, then rechecks input and publication
authority. Canonical diagnostics remain separate from cache agreement. It does
not synchronize, repair, publish, or inspect every unused historical payload.

The temporary reference contains contentless FTS postings and consumed row IDs,
not copied document bodies or an in-memory backup of the full database. Complete
term/row/column/position streams, per-document token counts and native totals are checked,
including empty and zero-token rows. Source SQL uses native read-only structural
integrity checking. Fixed comparison plans refuse temporary sort operators;
the exact schema's three-literal CHECK membership is a bounded native exception.
The 32 GiB scratch cap limits growth rather than requiring that much free space
for small vaults. Per-connection cache, input, row, history, posting and elapsed
limits are cooperative admission controls, not measured peak process memory.

The focused native gate passed 20 tests, including 54 changed/missing/extra-row
mutations across every family, 17 search-index corruptions checked against the
small-fixture native oracle, resource failures and exact scratch cleanup. The
expanded broad gate passed 692 tests with three failures, eight ignored tests
and two unchanged ledger fault matrices excluded. Its 573 passing unit tests
include duplicate multiplicity, replacement surrogate IDs, and a held predecessor
reader: canonical/control/index/WAL bytes and modification times stayed unchanged,
with SQLite SHM read marks explicitly excluded. The three failures identified an
old stale-index expectation, a macOS temporary-path alias in a test helper, and
a misleading partial-output warning for intentional dry runs.

After those corrections, the final focused gate passed **44 tests**: 31 unit
tests, all six public check workflows and seven doctor CLI tests. It confirmed
managed title and new-revision updates at the current publication even after
historical build-audit fields become null, external-edit refusal, faithful invalid
document diagnostics, dry-run preservation and human output outside a vault with
spaces in its path. Source pins stayed unchanged during each gate. Between the
broad and focused gates, only CLI preview handling and the two CLI test files
changed. Changed Rust formatting and whitespace checks passed.

Independent Astra review found no blockers to this tested implementation
checkpoint and verified the final source pins, failure corrections and actual
test logs. It awarded no package, capacity or overall-goal score; those require
the remaining native qualification.

```sh
python3 scripts/bazel.py -- test //:unit_tests //:check_cli_test //:offline_cli_test \
  --test_arg=full_check --test_arg=compact_audit --test_arg=normalized_metadata \
  --test_arg=check_cli --test_arg=managed_source_title --test_arg=human_check \
  --test_arg=doctor --test_output=errors
```

The full 100k/~10 GB lifecycle remains pending. The diagnostic limit bounds accepted output after projection;
it does not bound peak diagnostic allocation. Duplicate matching charges every
visited candidate but can rescan prefixes. These costs require measurement before
claiming large-vault audit usability. The remaining modes/default activation and
unseen semantic-completeness gate are also still open.

### Public full-check resource controls

The [reproduction guide](benchmark-full-check.md) describes release builds,
compiler provenance, fixture prerequisites and the complete supervised workflow.

The **unoptimized fastbuild** native macOS rehearsal and 1k control passed with the implementation at
`bdfd8ad` and the test-only held-reader helper committed as `ae73b16`. Each run
copies canonical files from a verified closed export, adds the existing 133-file
authored/graph overlay, and invokes two public normalized rebuilds. No seed SQLite
connection or copied derived cache substitutes for either rebuild. A real
`QuerySnapshot` remains open on the first publication while the second is built.
The run then checks and queries the second publication before and after a real
100,000-byte refresh of its second source.

| 1,000-source control (100 MB current content) | Measured result |
| --- | --- |
| Two public rebuilds | 33.502 / 32.503 seconds |
| Full check plus cited query, before / after refresh | 39.919 / 40.897 seconds |
| Managed content refresh | 0.812 seconds |
| Largest sampled concurrent audit and held-reader RSS | 211,861,504 bytes |
| Conservative sum of native audit and holder peak RSS | 212,025,344 bytes |
| Largest sampled scratch allocation | 84,475,904 bytes |
| Returned logical scratch size, before / after refresh | 81,735,680 / 81,866,752 bytes |
| Whole supervised run, including fixture/account verification | 493.160 seconds |

All ten CLI calls succeeded. Both checks reported complete canonical/cache
agreement with zero diagnostics at the expected publication. Citation spans and
hashes matched the selected revisions, including the refreshed content. The
pre/post read-only inventories matched for canonical files, control files and
both catalog generations, excluding SQLite SHM read marks and writer-lock
diagnostics. The held reader verified its original complete document rows and
snapshot before closing; temporary scratch was cleaned. Original fixture and
executable bindings were revalidated. Independent Astra review checked the raw
outputs, citations, inventories and holder result and accepted this control.

The [resource protocol](full-check-resource-protocol.json) keeps whole-account
inventories and executable/fixture hashing outside the measured check/query
interval. Monitoring overhead remains included. Physical allocation and process
samples are lower bounds on transient peaks; native peak RSS and logical SQLite
scratch sizes are separate evidence. The first post-refresh query follows
untimed harness verification, so its command duration does not measure the full
acknowledgment-to-answer delay. Two observations do not establish statistical
tail latency. The test helper rearms a bounded final read on the same retained
transaction; it does not extend production query deadlines.

The frozen 20× projection from the larger 1k measurements gives 817.932 seconds
per check/query pair, 4,237,230,080 bytes of memory and 1,689,518,080 bytes of
scratch for 10k. These were admission estimates for the same unoptimized build,
not measured 10k results or release-performance estimates. The 10k attempt
completed its first rebuild in 360.809 seconds, then was deliberately interrupted
during its second rebuild after the build-profile mismatch was identified. The
supervisor preserved the failed/incomplete report and the held reader exited
cleanly. No 10k control pass is claimed. Release qualification retains the same
corpus, outcomes and resource thresholds with newly pinned optimized binaries.
In particular,
the 1k result alone does not establish the 100k audit-time target, import
throughput, history-heavy workloads, other retrieval modes or unseen answer
completeness.

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

### Release full-check controls at 1k and 10k

The same control subsequently passed with pinned release executables. Actual
Bazel Rustc actions for the library, CLI and held-reader test binary each contained
exactly one `--codegen=opt-level=3` in `darwin_arm64-opt`; the recorded compiler was
Rust 1.98.0 with LLVM 22.1.8. Production audit code remained the `bdfd8ad` design,
with the held-reader helper from `ae73b16`. The tracked runner and reproduction
instructions are available from `f6dcfd9`. The control used unchanged corpus,
correctness assertions and resource limits from protocol version 2.

| Native macOS release control | 1,000 sources / 100 MB | 10,000 sources / 1 GB |
| --- | --- | --- |
| Two public normalized rebuilds | 9.033 / 8.178 s | 116.499 / 113.774 s |
| Full check plus cited query, before / after refresh | 9.171 / 9.309 s | 121.067 / 114.977 s |
| Managed 100,000-byte refresh | 0.494 s | 0.567 s |
| First subsequent cited query | 0.060 s | 0.062 s |
| Largest sampled audit plus held-reader RSS | 193,150,976 bytes | 768,507,904 bytes |
| Largest sampled scratch allocation | 84,475,904 bytes | 823,611,392 bytes |
| Whole supervised run, including fixture/account verification | 485.479 s | 1,118.172 s |

Both tiers completed all ten CLI commands, full canonical/cache agreement before
and after refresh, exact citations, read-only inventories and original-generation
reader checks. Both cleaned their private scratch and revalidated pinned inputs.
At 10k the conservative sum of native audit and holder peak RSS was 769,212,416
bytes; returned logical scratch sizes were 817,971,200 and 818,302,976 bytes.
The completed account, including retained prior attempts, occupied 53,531,934,720
allocated bytes. A prior release 10k attempt stopped after the sandbox denied
process monitoring; its supervisor terminated the first rebuild and retained the
failed fixture. That attempt supplies no capacity conclusion.

Independent Astra review accepted the explicit-check slice at **9.2/10** with
zero blockers after inspecting raw outputs, exact citation spans/hashes, the
40,144/40,158-file read-only inventories and the retained reader result.

These results qualify this explicit-audit control at 1k and 10k. They retain the
measurement limitations above: no cold-cache or tail distribution claim, no
acknowledgment-to-answer timing claim, and no qualification of 100k, import,
history-heavy workloads, remaining query modes or unseen answer completeness.

### Mixed normalized document workflow checkpoint

On an already activated normalized vault, ordinary lexical `search` uses published
discovery, `read` verifies the selected canonical dependency closure, and omitted
context scope resolves to `indexed-documents`. Authored text and captured source
passages share the existing selection and output budgets. Explicit full audits
remain separate. See [scope and citation details](indexed-context.md).

The release integration run executed 20 test targets: 19 passed, while the context
CLI target reported 19 passes and one error-message compatibility failure. Across
those targets there were 869 passing test executions, including 583 unit tests and
all 29 freshness tests. Nine unit tests were ignored and two unchanged accounting
fault matrices were excluded. The compatibility failure was corrected by restoring
the literal scope names in the unsupported-host-selection error. A subsequent
release run passed all 20 context CLI tests and all eight normalized workflow
tests; its only production change from the broad run was that message. Both runs
retained unchanged source hashes during execution. The broad run took 924.72 s;
the affected CLI rerun took 57.55 s. These are validation durations, not command
latency measurements.

Coverage includes selected authored/support/decision edits, removed files and
newly present expected-absent files, final rechecks, damaged cache facts, historical
captured revisions, verification-budget refusal, authored files over 1 MiB,
legacy default-scope compatibility and exact source citations. Public assembly
cannot bypass the selected-document proof to claim this scope. The public refresh
workflow checks immediate new-revision retrieval and unchanged historical bytes.

Independent Astra review accepted this workflow slice at **9.2/10** with zero
correctness blockers. It checked 24 assertions across 20 public commands, then
replayed all four affected scope-rejection cases on the final pinned executable.
The review verified exact authored/source spans and hashes, selected-file tamper
refusal, scope labels, refresh results and immutable historical captures.

This checkpoint does not qualify new source import or authored mutations after
normalized activation, remaining retrieval modes, default storage activation,
100k/10 GB capacity or unseen answer completeness. Its functional fixtures import
sources before activation; they do not substitute for the remaining import path.


### Normalized Page authoring checkpoint

An activated normalized vault supports Page initialization, guarded replacement
and bounded cross-linked batches, followed by immediate read, lexical discovery
and mixed-document context. Staged changes use retained version-3 write proofs;
deployed version-2 refresh recovery remains supported. See
[Page command behavior and limits](indexed-context.md).

The terminal release checkpoint passed all ten targets: 626 unit tests and 185
integration test executions (811 total). Eleven tests were ignored; two unchanged
accounting fault matrices were excluded. Source hashes remained unchanged during
the 985.05-second validation. The only later code change reordered module
declarations to satisfy formatting; `cargo fmt --all -- --check` and
`git diff --check` passed. Strict Clippy was not run; compiler warnings remain.
The reproducible integrated command is:

```sh
python3 scripts/bazel.py -- test //:unit_tests --test_output=errors --nofetch --config=release \
  --test_arg=--skip=jobs::accounting_tests::every_actual_ledger_io_boundary_replays_without_duplicate_charge \
  --test_arg=--skip=jobs::accounting_tests::every_bootstrap_genesis_head_and_canonical_create_io_boundary_is_retryable \
  //:offline_application_test //:offline_cli_test //:changes_recovery_test \
  //:catalog_scan_eligibility_test //:sources_evidence_test //:graph_queries_test \
  //:graph_review_test //:entity_decisions_test //:machine_contract_test
```

Independent Astra acceptance was **9.3/10**, with zero correctness blockers.
The public-command review verified guarded edits, cross-linked
batches, immediate content, exact source citation bytes and BLAKE3 hashes,
selected-file tamper refusal, stage/apply/retry, stale-stage author preservation,
and persisted review-policy restoration. Fourteen preview cases preserved every
file and directory's bytes and modification times, including SQLite shared
memory. Preview explicitly marks indexed admission, dependencies and collisions
unchecked; an omitted put destination remains unresolved. A preview is not an
admitted or stageable change.

Native injected failures before and after SQLite commit exercised real sealed
Page admission and retained replay, old-reader isolation, canonical/cache oracle
agreement, policy restoration and terminal idempotence. These are native
in-process fault tests with reconstructed sessions, not process-death or
power-loss qualification.

This checkpoint covers Page authoring only. New-source capture/withdrawal,
collection import, remaining query modes, default activation, 100k/10 GB capacity
and unseen semantic completeness remain separate acceptance requirements.

### Captured-source lifecycle checkpoint (2026-10-04)

Explicit normalized vaults now support ordinary source add, refresh and withdrawal,
including staged apply, exact retained replay and historical inspection. New Source
IDs encode the full UUIDv7 value as 39 decimal digits; existing IDs remain valid.
A compiled oracle against the pinned Unicode folding dependency checked every valid
Unicode scalar: decimal names have no other folded inverse. Native collision,
selected-path and 1k/10k source-root work checks passed. Those path checks are
component measurements, not whole-import throughput or a capacity qualification.

The grouped native release checkpoint passed 651 unit tests and 24 offline CLI
tests (10 ignored, two unchanged accounting matrices excluded), in 304.64 seconds
overall, with source pins unchanged. The three other relevant integrations had
already passed on the preceding candidate: offline application (19), source
Evidence (19) and machine contract (7); they were not repeated unchanged. Native
pre/post-SQL cuts, exact full-reconstruction oracles, lifecycle field permissions,
selected dependents, pure request previews and legacy identity compatibility passed.

The independent public-command critic recovered the exact failed withdrawal from
the preceding candidate, verified unchanged immutable bytes and modification times,
and completed a fresh add → cited context → refresh → staged withdrawal → history
workflow, including sole/shared support invalidation. Complete cache-loss rebuild
was an identified maintenance blocker at this checkpoint; the following checkpoint
records its repair and separate acceptance. No semantic, bulk-import or 100k/~10GB acceptance claim follows from these
results. Compiler warnings remain; Clippy was not run at this checkpoint.


### Complete-cache-loss maintenance checkpoint (2026-10-04)

A previously activated normalized vault with its entire derived cache removed can
now reconstruct from intact idle operation authority. A bounded outside-cache
reservation fixes its starting publication and next candidate identity. Retry
rotates unacknowledged identities, retires authenticated candidates through leases,
and preserves at most eight unclassified candidates totaling 1 MiB of actual
SQLite and sidecar bytes. Unknown or unsafe paths and missing authority refuse;
publication cannot recreate authority through a legacy fallback. This is an
explicit complete-loss workflow, not general cache repair.

The grouped correction checkpoint passed 356 native release tests (three
ignored), in 188.22 seconds overall with source pins unchanged. Ninety-eight
unchanged passing retrieval checks from the first checkpoint were reused;
454 distinct successful test names cover the combined slice. The first attempt
retains three fixture failures: empty migrated storage had activation but no
payload directory yet, identical source contents intentionally shared an owner,
and real maintenance updated writer-lock diagnostics. All were corrected and
replayed together. Final CLI targets passed 24 offline, seven machine-contract,
five schema-contract and six full-check tests. Compiler actions confirm native
ARM64 Rust optimization level 3. These are correctness checks, not capacity data.

Independent Astra acceptance scored **9.3/10** across all nine prospectively
specified maintenance tasks. It repaired the original failed cache-loss fixture,
and separately completed a genuinely migrated retained-layout workflow. Forty-seven
public commands verified unchanged canonical/history bytes and modification times,
identical citations, historical reads, subsequent source updates, refusal controls
and pure previews. Native tests cover actual reservation, creation, retirement,
acknowledgment and publication cuts, preservation bounds, foreign controls and a
held selected reader's final freshness recheck. They inject returned errors and
reopen in one process; they do not qualify process-kill or power-loss recovery.
A real active-operation refusal changed only writer diagnostics and an existing
SQLite shared-memory modification time with identical bytes; dry-run had no
whole-tree changes. Full-scale reconstruction remains unqualified.

### Normalized host-selection mechanics checkpoint (2026-10-04)

Lexical indexed-document context supports preparation and fingerprint-bound
ID-only replies while retaining its closed selected canonical proof. Host packets
preserve the existing passage selector's retained priority before owner interleaving
and the 80-card cap. Automatic context retains its previous policy. Exact source
citations, authored locators, final budgets and stale-reply rejection apply to both.
Preparation is a task for a host agent, not final answer context; CLI accounting
cannot observe that external agent's input, usage or cost.

Independent Astra mechanical acceptance scored **9.5/10** with all 12 declared
tasks complete and no correctness blocker. Six fixed development queries retained
exact Automatic baseline outputs. Public mixed-source/Page preparation and reply
application, unrelated unindexed growth, staged/committed source refresh,
withdrawal, Page replacement, altered canonical bytes, bad replies, unsupported
modes and proof limits behaved as specified. The review audited 18 context outputs,
61 passages, 55 source citations and 16 packets. A targeted replay fixed misleading
plain preparation output while preserving the original task's exact 4,027 bytes
and fingerprint. Header-only empty-result hints and stale-reply continuation
remain presentation follow-ups.

Bazel formatting and final CLI compilation passed. Strict Clippy remains failing
on unused adapters and style findings; its complete diagnostics are retained locally,
with no lint waiver or clean-lint claim. A six-case development lineage harness
has executed, but harness success is not a quality score. Public equivalence,
actual evidence completeness and any separately declared host-model arm require
their own assessment. The unseen semantic HIGH gate, bulk import, remaining
retrieval modes and mandatory 100k/~10GB lifecycle remain unfinished.

### Source-add attribution and context development checkpoint (2026-10-04)

A native release checkpoint added test-only source-add attribution and exact
interval-coverage experiments. Seven focused correctness tests passed; two
explicit profiling tests remained ignored. The preceding attempt failed with
seven related profiler API errors and executed no tests. Compiler actions confirm
optimization level 3 on native ARM64; the production CLI hash is unchanged.

Four closed disposable controls varied Source count S, retained Change count H
and retained object count O independently: (10,31,91), (30,31,91), (10,51,91)
and (10,31,131). Eight actual adds, four staged applies and eight public full
checks completed with exact new citations and unchanged old canonical bytes and
modification times. Increasing S added no executed traversal work in these
controls. Increasing H by 20 added 1,280 whole-add entry visits; increasing O by
40 added 480. Counts include repeated physical and logical visits, not unique
files. Whole-add observations were 432–454 ms; inclusive portable validation
accounted for approximately 13%. These tiny, single-observation controls establish
growing unrelated history/object work, not latency dominance or shipping capacity.
Parent resource sampling overlaps the operations; filesystem sync counts and
exact native peak RSS remain unavailable. No full-scale import is qualified.

On the same six development queries and retained evidence, a frozen test-only
interval objective improved final complete answerable tasks from 0/5 in the three
controls to 2/5. One question lost previously supported facts. Three positive
candidate decks displayed complete fact sets; two failed the unchanged five-second
proof deadline after thousands of native packet serializations. All errors stay
in the denominator. Independent audits passed exact retained/displayed evidence
and final citations; the complete pinned input tree was unchanged. Neither
native exact-fit replays nor host selectors ran, because the five-positive
feasibility requirement failed. Production context policy remains unchanged.
The unseen semantic gate, strict Clippy and the 100k/~10GB lifecycle remain open.

The next bounded measurements separate durability cost from traversal and exact
packet-building cost from semantic selection. They must preserve existing
allocation, collision, recovery, freshness and budget contracts; a scoped
development gain does not permit weaker acceptance limits.

### Durability attribution and selection-cost checkpoint (2026-10-04)

One native baseline pair completed two captures, one staged apply and two public
checks, with identical prior traversal counts, exact citations and unchanged
existing canonical bytes and modification times. The pass-through adapter
delegates each durability operation once and preserves its result. During the
443 ms whole-add observation, 31 intercepted file syncs took 146 ms and 219
directory syncs took 103 ms: approximately 56% combined. These counters exclude
SQLite's internal syncs and direct file/adapter paths. They establish a material
shared-operation cost, not total filesystem sync work or a latency distribution.
Existing-directory syncs protect interrupted directory creation; this experiment
does not justify removing them.

The test-only candidate-deck implementation now ranks eligible candidates before
trying native packet construction. Full-pool native validation still rejects
malformed omitted candidates; native construction still decides feasibility.
The objective, owner rounds, tie order, exact spans and final packing are unchanged.
Eleven focused selection correctness/equivalence checks passed. The first combined
checkpoint also retained one fixture failure; after adding the existing required
vault marker, only the affected durability-adapter test was rebuilt and passed.
The eleven unchanged selection passes were reused. Native compiler actions confirm
optimization level 3, and the production CLI executable hash stayed unchanged.

All twelve development arms then completed. Positive decks used 82 counted native
packet constructions in 82–89 ms, plus the original preparation call; the absent
case used 69 constructions in 61 ms. Prior successful deck bytes and formerly
failed accepted prefixes were preserved exactly. The unchanged deterministic final
selector still completes only 2/5 positive tasks. An independent critic found all
five displayed positive fact sets complete and verified five reference ID replies
producing complete, correctly cited final contexts of 2,170–4,123 bytes. These
reference replies prove feasible packing, not autonomous selection quality.

Six fresh host selectors subsequently ran once each. Actual tool traces exposed
truncated input in the first trial despite a sufficient nested file-read limit.
That failure remains in the denominator; the remaining five used a prospectively
recorded outer-and-inner limit correction and received exact complete tasks. All
six raw replies passed native replay without retries or changed source files.
Independent assessment found complete returned content for 4/5 positive tasks,
including the invalid-input trial. The other failure omitted required prerequisite
competence despite its presence in the full displayed deck. Combined input-valid
and complete-task credit is therefore 3/5. The absent control returned no passages;
all 17 final citations and six native budgets passed. The assisted gate failed.

Current host execution logs supply actual harness-reported usage: six selector
invocations required twelve model responses, totaling 336,400 input tokens
(253,568 cached) and 2,215 output tokens, including 786 reasoning tokens. Visible
application tasks and wrappers totaled 360,381 bytes. Usage includes harness and
repeated context; monetary cost remains unavailable. Observed external-agent UTC
intervals totaled 331 seconds, include orchestration and do not measure inference
time. None of these
development experiments qualifies the unseen semantic gate or the 100k/~10GB
lifecycle. Strict Clippy remains failing.

The next acquisition milestone is a resumable manifest importer sharing bounded
capture groups through one Change publication. Its pending state must durably
name every Source, Revision and Change identity and timestamp before retention.
Per-file durability remains intact. Unrelated Change/object traversal growth
remains a separate scale blocker; batching alone does not remove that growth.
