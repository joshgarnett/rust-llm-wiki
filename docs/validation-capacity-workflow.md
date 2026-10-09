# Capacity workflow checkpoint

The preserved 0.2.0 native macOS ARM64 candidate completed the setup and import
portion of a new collection control on 2026-10-05: **1,000 captured Sources and
1,000 authored Pages**. This checkpoint precedes its frozen retrieval, churn and
cache-loss tests. It does not establish 10k/25k capacity, HIGH retrieval acceptance
or a whole-workflow score.

Executable SHA-256:
`c9eaf97e93f905300861175c3fe8319b5560dc16bb8d8e048913a9ea2cf6ba78`.
Compiled source: `38b723bda4e80068a550f71e17709fe565e74762`; native release
optimization level 3, Rust edition 2024. Hardware: Apple M1 Max, 32 GiB RAM,
APFS, macOS 26.5.2. No build, provider calls or production vaults were involved
in this run.

## Completed public workflow

The corpus contains 998 varied synthetic UTF-8 documents and two pinned licensed
Cargo documents, totaling **100,302,609 bytes**, with 1,000 distinct content
hashes. Synthetic documents vary around 80–120 KB and place facts at separated
positions. Question labels and expected answers remain outside indexed content.

Sixteen public captures supplied the graph bootstrap. Its 160 public commands
created 32 Entities and 16 reviewed literal-property Assertions; an independent
audit checked their 16 exact citations and 48 decision records. This qualifies
fixture storage and review mechanics, not ownership navigation or graph relevance.

After a complete backup, guarded storage migration and normalized activation
passed. Import preparation preview reported the remaining 984 inputs while
preserving all 866 existing vault files' bytes, sizes and modification times,
and creating no manifest. Actual preparation then succeeded.

The importer committed 62 four-item groups before the planned SIGKILL, retaining
four pending allocations at ordinal 248. Three same-key resume windows completed
all 984 inputs in 246 groups. All four pending Source/Revision IDs and capture
timestamps survived. All 2,000 canonical originals and extracted content files
match the generated input sizes and SHA-256 hashes.

All 63 Page batches passed. The 1,000 Pages have distinct stable identities and
byte-exact authored input, including the separately counted navigation overlay.
An unchanged sync reused publication 310. Full `check` completed with zero
diagnostics, canonical/cache agreement and revision-owner history checked.

## Measurements and next gate

| Operation | Observed supervised elapsed time |
| --- | ---: |
| Interrupted import plus three resume windows | 995.437 seconds |
| 63 Page batches | 213.283 seconds total |
| Unchanged sync | 2.923 seconds |
| Full check | 16.043 seconds |

These locally owned monotonic intervals include process supervision. The separate
aggregate clock also includes generation, reviews, audits and waits; summed
command times do not describe the whole session. A controller terminal input
limit was corrected before the affected product command launched; the same
aggregate clock and pinned executable were retained.

All 253 setup CLI attempts were recorded, including intentional process
termination. No memory-sampling errors occurred. Largest native main-process RSS
was 197,558,272 bytes; largest sampled process-tree RSS was 197,492,736 bytes.
Sampling does not establish an exact process-tree maximum. Account allocation
after import was 1,631,723,520 bytes, including retained inputs and the backup.

Import throughput projects beyond the four-hour 25k target; this is an
extrapolation, not a completed large-tier measurement. Unchanged sync also read
406,750,732 bytes across 5,149 inputs, despite reusing the publication. Its large
vault cost needs qualification. A bounded comparison of supported four- and
eight-item groups is the next performance investigation after this control,
before considering storage changes. No larger corpus is admitted automatically.

The frozen initial query trial subsequently stopped on its 44th task. Forty-three
tasks completed their search/context/conditional-read sequence; the next selected
search returned `CAPABILITY_UNAVAILABLE` for an ordinary deployment-lead question.
The failed command returned no data, and the supervisor stopped without a retry.
All 127 query attempts are retained: 44 searches, 43 contexts and 40 reads. The
126 successful commands and one refusal took 37.865 supervised seconds total;
the longest took 0.477 seconds. These are command intervals, not session turnaround
or warm p95 qualification. The owning clock terminated after 5,738.717 seconds,
including setup, reviews, audits, host transport and waits.

A separate one-call diagnostic on a complete disposable copy identified the
trigger: the ten discovery hits included nine Current source payloads and one
Entity with Current identity and an unsupported description. The Entity excerpt
was empty and uncited. The selected-search guard conflated identity navigation
with body-evidence authority and rejected the whole page. This question is now
development data; the failed run remains immutable.

Independent partial assessment found all 547 returned citation occurrences and
40 selected read ranges byte-correct and bound to their current revisions.
Evidence was complete for 33 of 37 completed positive tasks; including the failed
positive gives 33 of 38 attempted positives. Six completed negative tasks were
safe. Four completed positives still missed facts despite correct citations.
The successful prefix does not qualify the frozen workflow or admit another tier.

Remaining repetitions, unstarted questions, Cargo overlays, source updates,
withdrawal and complete-cache-loss rebuild are unrun in this control. The earlier
[collection control](validation-1k-collection.md), accepted
[selected search](validation-verified-search.md), and failed
[allocation experiment](validation-location-allocation.md) remain separate.

## Separate native 256-current lifecycle

A separate staged control on 2026-10-06 uses the packaged candidate013 0.2.0
CLI, SHA-256
`64744ed8c8f0bf236c0dc169db4f6a7fbc79327bb015dcb931861f7c2faecaf1`,
on native macOS ARM64 with release optimization level 3. It extends a complete
clone of the accepted 64-Source draft workflow; the earlier 1,000-Source control
and its failed query trial above remain separate.

The starting vault has 64 retained Sources, 63 current and one withdrawn. Import
of 193 distinct frozen UTF-8 originals creates **256 current Sources and 257
retained Sources**. Both captured original and extracted Markdown copies match
every input's full size and SHA-256. A controlled, same-size edit of one newly
imported Source creates a distinct immutable revision; withdrawal then leaves
255 current and two withdrawn Sources. The full original vault remains unchanged.

All 72 continuation commands complete: accepted current/historical range reads,
the existing cited draft's read and title discovery, all twelve ordinary lexical
search/context pairs, guarded unchanged draft replacement, actual obsolete-hash
refusal, source refresh and withdrawal, complete-cache relocation and normalized
rebuild, plus full checks before and after rebuild. The draft's identity, body,
author notes and hash survive. All existing peer/history files remain unchanged
through the new Source's churn. Exact range-read data and same-tier post-churn
D01/D09 search/context packets match after reconstruction, apart from declared
publication/time/work fields. This does not qualify cursor continuation across
reconstruction.

| Operation | Owning supervisor elapsed time |
| --- | ---: |
| Import 193 originals in 49 default four-item groups | 66.733 seconds |
| 24 ordinary lexical search/context commands | 0.057–0.128 seconds each |
| Refresh one 110,426-byte Source | 0.653 seconds |
| Withdraw that Source | 0.290 seconds |
| Complete-cache-loss normalized rebuild | 1.704 seconds |
| Full occupied / rebuilt checks | 2.739 / 2.530 seconds |

The continuation finishes in 97.402 owning seconds. One preserved predecessor
read returns correct application output but its optional `time -l` wrapper
fails because the sandbox denies `sysctl kern.clockrate`. A prospectively admitted
continuation removes that wrapper, reuses the verified clone and repeats only
that failed invocation: **73 cumulative CLI invocations**, including the original
measurement failure. Cumulative owning execution is 100.874 seconds; external
review/launch gaps are separate. The original deadline and cumulative resource
limits remain in force. RSS and physical I/O are unavailable.

Sampled new owned allocation peaks at 397,766,656 bytes, including the complete
clone and retained/rebuilt caches; sampling is not an exact instantaneous peak.
The final free-space observation is 58,310,987,776 bytes. Supervisor logical
inspection totals 2,388,645,742 bytes across both attempts. Public decoder work
and canonical verification counters remain separate from that inspection.
These bytes are logical work, not measured physical I/O.

Independent actual acceptance passes at **9.2/10**, every scoped mandatory
observation and zero correctness blockers. The critic authenticates 164 SourceRef
occurrences (105 distinct references), including all ten references embedded in
the saved draft, against exact immutable source slices and history membership.
The original instrumentation failure remains failed. The critic completed its
report 28 seconds beyond the separate 240-second review allowance; that process
timing gate failed, and the scoped correctness judgment is reported separately.
This control uses lexical commands,
no embedding reacquisition, answer actors or new build. Ordinary relevance is
observational; the previously failed default completeness gate, semantic
readiness, unseen HIGH and practical 25K/~2.5GB capacity remain open. Import
accounts for most measured execution time; query latency supplies no current
reason to optimize decoder counters. Growth to 1,024 current Sources requires
separate admission using the complete workflow and measured resource headroom.

## Separate native 1,024-current lifecycle

The same packaged candidate013 binary completes the next prospectively admitted
whole workflow at **1,024 current / 1,026 retained Sources**, then 1,023 current
and three withdrawn after a new Source's revision and withdrawal. Import adds
769 distinct frozen originals, totaling 77,235,641 bytes, in 193 default four-item
groups. Twenty-six new development originals extend the existing corpus using
its pinned generator; labels remain outside indexed content. Both captured copies
of every input match complete size/hash pins. The accepted 256-stage vault is
cloned once and remains unchanged.

Independent actual assessment passes at **9.2/10**, every mandatory observation
and zero correctness blockers. All 72 commands complete; 159 SourceRef occurrences
(105 distinct), including ten embedded draft references, authenticate against
exact immutable source slices, revision history and phase-specific eligibility.
The draft's identity, body, both author notes and hash survive guarded no-op,
obsolete-hash refusal, refresh/withdrawal and reconstruction. Five previously
accepted reads and four post-churn D01/D09 search/context pairs match after
cache-loss rebuild with only the declared publication/time/work normalization.
Both full checks report zero errors.

| Operation | Owning supervisor elapsed time |
| --- | ---: |
| Import 769 originals in 193 groups | 264.500 seconds |
| Largest ordinary lexical query observation | 0.348 seconds |
| Refresh one 90,415-byte Source | 1.636 seconds |
| Withdraw that Source | 0.319 seconds |
| Complete-cache-loss normalized rebuild | 6.991 seconds |
| Full occupied / rebuilt checks | 11.734 / 11.180 seconds |

The stage completes in 371.121 owning seconds. Conservatively summed owner time
is 471.994 seconds across the staged control; external review/launch gaps are
separate. All 145 cumulative invocations include the preserved first-stage
instrumentation failure. Import accounts for about 71% of this stage. Observed
import rates are similar, 2.907 versus 2.892 items/second; different occupancy and
input batches are not a causal paired performance test or a 25K projection.

Across 610 allocation samples, total owned peak allocation is 1,666,273,280 bytes
and peak logical size 1,616,164,994 bytes, including both vaults and retained old
caches. Minimum sampled free space is 57,059,971,072 bytes. Cumulative supervisor
inspection is 8,606,283,716 logical bytes and streams 1,316,226 bytes. These remain
within the independently admitted ceilings. Sampling is not an exact peak; RSS
and physical I/O remain unavailable. The critic completes within its separate
time/read allowance; the prior 256-review deadline failure stays recorded.

Storage is the next measured capacity concern. The accepted 256-vault inventory
contains 58,279,151 bytes of retained indexed deltas and 53,342,969 bytes of
proposed text/binary payloads, beyond the 51,798,938 canonical captured bytes.
Existing schema-2 storage coordination can share immutable proposed objects.
A bounded disposable-copy experiment with that existing coordinator precedes
another deduplicator or larger growth; repeated bytes are not automatically
reclaimable, and migration may temporarily increase allocation. Protected
recovery/accounting history, original source paths and author content must survive.

This qualifies the declared lexical workflow at this corpus/history. It does not
qualify broad default completeness, semantic relevance/readiness, unused retained
payload validation, universal normalized activation, native HIGH or practical
25K/~2.5GB capacity. No new build, remote provider or answer actor is involved.

## Separate storage cleanup experiment

A disposable copy of the accepted 256-Source vault tested the existing guarded
schema-2 cleanup with its default retention of twenty undo operations. Immediate
whole-vault logical size fell from **268,298,586 to 244,549,546 bytes**: a saving
of **23,749,040 bytes (8.85%)**, below the prospectively declared 10% benefit gate.
Migration metadata and retained operation evidence remain in that denominator.
The plan's estimated reduction is not an observed whole-vault saving.

Cleanup resumed an exact pinned pending operation and completed in 162.605
observed seconds. The preceding attempt was stopped by a supervisor race while
sampling a temporary file; its failure and unrun tasks remain recorded. Canonical
source revisions, Page bytes and protected authority checks passed immediately
after cleanup. The next public `check` failed with `INDEX_CORRUPT`: the normalized
identity claim differed from canonical projection. Thirty later workflow tasks
were unrun, so this experiment does not establish a completed migration workflow.

Source inspection identified a likely missing normalized publication after the
vault's schema change. One bounded diagnostic using the old executable timed out
during `index sync` after ten seconds; its subsequent check was unrun. That
diagnostic does not prove the remedy or qualify larger-vault sync latency.
No additional cleanup trials or storage subsystem were added to pursue the
failed benefit threshold.

## Fixed-input import history comparison

On 2026-10-07, one optimized diagnostic imported the same 256 distinct synthetic
100 KiB Markdown files into three disposable copies. It reused the accepted
literal-workflow CLI (`93074e69…`), unchanged production code, release optimization
level 3 and the native ARM64 compiler settings. Test-only instrumentation measured
the outer portable-path validation interval without adding overlapping timers.
No provider calls occurred. This is a mechanism experiment, not representative
25K capacity or a new retrieval-quality assessment.

| Accepted fixture stage / group size | Import seconds | Portable validation share | Added logical vault bytes |
| --- | ---: | ---: | ---: |
| 256 / 4 | 100.266 | 10.36% | 228,197,896 |
| 1,024 / 4 | 98.560 | 11.59% | 228,193,817 |
| 1,024 / 8 | 98.316 | 10.41% | 227,962,242 |

All three import calls completed their 256 mappings, with one publication per
group: 64, 64 and 32 publications respectively. Portable validation remained
below the prospective **20% attribution gate**. Eight-item grouping produced no
material observed improvement. Path-validation optimization stops here; these
observations do not justify changing ordinary allocation or portability rules.

The whole operator failed. Its first invocation placed logs in the required empty
report directory and stopped before any import. One explicitly admitted
continuation preserved that failure and all original limits. After completing the
three import measurements, its literal-search probe used `historyprobe255` while
the input contained `Historyprobe255`. The search correctly returned no hits;
the supervisor then indexed the empty result. **Five of six planned public
search/read checks were unrun.** Neither invocation establishes an accepted
import-to-discovery/read workflow, and no further retry was made.

A separate read-only preservation audit found every original fixture file
unchanged, all pre-existing immutable files in the copies unchanged, and all
1,536 newly captured original/content files exactly matching their frozen inputs.
Total new owned logical size was 2,969,367,434 bytes, within the 4 GiB limit;
terminal free space was 43,487,334,400 bytes, above the 40 GiB floor. Peak storage,
RSS and physical I/O remain unavailable. Known supervisor and audit reads totaled
13,205,479,277 bytes, below the 16 GiB known-read limit; this excludes unobserved
product runtime reads.

Each 26,214,400-byte input batch added approximately **8.70 times its input size**
to the vault. This measures logical file lengths, including operational history
and cache growth, rather than allocated disk blocks. It does not establish a
linear 25K storage law. Storage representation and realistic admission headroom
now warrant investigation before another large-tier run. The accepted default
retrieval gate remains 9/28 facts and 2/10 complete positive tasks.

The subsequent bounded source and file-metadata study attributes the new growth:

| Representation | Added logical bytes, occupied four-item arm |
| --- | ---: |
| Canonical immutable original and Markdown content | 52,428,800 |
| Retained Change original assets and Markdown postimages | 52,779,238 |
| Retained indexed-delta JSON | 56,369,036 |
| Cache, including SQLite/WAL/SHM and pointers | 62,504,904 |
| Change journals | 690,304 |

The remainder is Source/Revision headers, Change metadata and import state.
Database internals were not opened, so the cache figure does not establish
table-level duplication or reclaimable space. Current allocated-block observations
exclude directory allocation and cannot establish exclusive APFS consumption;
the baseline lacks block counts, so allocated growth is unavailable.

Independent architecture review **rejected a new compact-delta format before
implementation**. `captured_content_document` stores normalized Markdown body
text separately from raw text. Heading-rich inputs therefore cannot share those
two whole strings by exact equality. Referencing raw content alone plausibly
removes about one input copy, roughly 12% of this increment, below the proposed
20% whole-import benefit gate. The 56 MB delta bucket is not all removable text.
No codec, reference format, historical rewrite or new storage subsystem was added.
The existing external-content FTS mechanism also already avoids an additional
FTS content table; changing to it would add no benefit here. See the
[SQLite external-content contract](https://www.sqlite.org/fts5.html#external_content_tables).

Preserving all history at 25K needs a separately admitted disk envelope. Applying
8.70 times to 2.5 GB gives a rough 21.75 GB additional-vault scenario, not measured
capacity or a universal bound. Neither the earlier cleanup result nor the present
rejected hypotheses authorize deleting retained evidence to make that tier fit.

## Separate native 4,096-current Markdown lifecycle

A new public workflow on 2026-10-07 reused the accepted 0.2.0 native macOS ARM64
CLI, SHA-256 `93074e694692fd731f708b5d257298fce5d4fd70137963a234dd1225ff9d9873`,
compiled from `8efb1cd971e1db6df706b3497b6653cf15310561` with release optimization
level 3. No Rust rebuild, provider call or production vault was involved.

The complete starting copy contains 1,023 current and 1,026 retained Sources,
including three withdrawn Sources and 1,029 immutable revisions. Importing
**3,073 distinct synthetic Markdown originals totaling 308,661,338 bytes** reaches
**4,096 current Sources**, 4,099 retained Sources and 4,102 revisions. Existing
licensed Cargo originals, content, notices and history remain byte-exact. An
optional HTML preparation was rejected before execution: ordinary named `.html`
inputs currently retain unsupported originals without searchable extracted text.
The original Markdown inputs were selected prospectively; the rejected pins,
single preflight and preparation evidence were retained. This is Markdown-only
qualification, not HTML import support.

All **45 declared public commands complete**, with no unexpected errors or unrun
steps. The stale-author-hash refusal is expected. Thirteen four-item-group windows
commit 769 groups under one resume key. Public status confirms a clean acknowledged
pause, and verified literal discovery plus an exact cited read work immediately.
This tests group-boundary continuation; it does not add a crash-safety claim.

The workflow performs D01/D09 lexical search/context, appends one freshness note
to the existing cited draft, refuses its genuinely obsolete author hash and checks
the occupied vault. Refresh of one newly imported Source creates a new immutable
revision; old/current ranges retain their respective eligibility. Withdrawal
leaves **4,095 current / 4,099 retained / four withdrawn Sources / 4,103 revisions**,
and current scoped search excludes that Source. Complete cache relocation,
normalized reconstruction and a second full check preserve same-tier context
passages, the exact edited draft and the earlier cited range. The old cache remains
retained. Original-vault and peer/history hash assertions all pass; every new
original/content pair matches its frozen input.

| Operation | Observed owning elapsed time |
| --- | ---: |
| All import windows, including supervision | 1,180.199 seconds |
| Maximum actual search/context command | 1.314 seconds |
| Individual Source refresh / withdrawal | 0.626 / 0.327 seconds |
| Complete-cache-loss normalized rebuild | 33.602 seconds |
| Full occupied / rebuilt check | 49.229 / 43.533 seconds |
| Whole operator, including summary | 1,443.650 seconds |

The new stage totals 1,456.450 known owning seconds including preparation,
preflight and sealing, below its 2,400-second envelope. Separate process intervals
are summed; nested wrapper time is not added again. Import stays below its
1,800-second aggregate acceptance threshold, which is checked after calls rather
than serving as a separate preemption timer. No latency distribution or 25K
extrapolation is qualified by these individual observations.

Across 217 samples, maximum owned allocation is **5,312,458,752 bytes** against
7 GiB, with 61,812 entries. Minimum sampled free space is **38,134,980,608 bytes**,
above the prospectively declared 32 GiB floor. Previous 40 GiB experiment limits
and failures are unchanged. Known operator inspection is 23,386,788,060 bytes
against 64 GiB; streams total 232,649 bytes. These exclude unobserved runtime work;
physical I/O, RSS and instantaneous allocation peaks remain unavailable.

Independent actual-content review scores the declared lifecycle **9.2/10**, with
all seven mandatory tasks and no observed correctness blocker. It authenticates
107 citation occurrences and 35 exact-text rechecks across eleven Source/revision
pairs, with zero quote/hash errors. Complete preservation relies on the pinned
operator's full-file assertions, supplemented by independent selected payload and
Page checks; the reviewer did not repeat a full-corpus rehash. Its initial report
exceeded the separate 300-second reporting envelope by **7.555 seconds**, with
final accounting disclosure following later. That procedural failure is retained
and prevents an unqualified all-review-gates claim.

Default answer completeness remains insufficient: D01 omits required
`--offline`/`--locked` semantics, while D09 includes archived distractors and lacks
a complete approved current procedure. Current citation eligibility does not
establish procedural approval. The original native fact/task gate remains failed.
This scoped lifecycle adds no unseen HIGH, semantic readiness, practical
25K/~2.5GB, live-provider, HTML extraction or complete-release acceptance.

## Public 1K to 10K lineage and sync diagnosis

The October 7, 2026 public Markdown run uses the accepted `e82cab472ce39a3fb6f2cca58dc7c360a0fed7e8`
source and the pinned local 0.2.0 CLI (`ec1478bdd0a14f3bfc1a9d89c0eeb2c8b0f130515d2068153d81cd8739ede146`).
Its native aarch64 build uses optimization level 3 and no debug information.
Inputs combine two licensed Cargo documentation files with 9,998 prospectively
generated synthetic documents of approximately 80–120 KB each. Labels stay out
of indexed content. The vault also retains a fixed 1,000-Page authored overlay.
No provider calls occur.

The original attempt failed during an operator backup-inventory naming collision.
A separately reviewed continuation preserves that failure and executes only the
remaining work. Across these discontinuous attempts, the 1K operational tier
completes its checks, rebuild comparison, negative controls, immutable-history
refresh/withdrawal and external-editor Page deletion/restoration. Independent
review authenticates the returned citations and comparisons. Its five mechanics
questions still supply seven of eight facts and complete four of five tasks;
the two-source task remains incomplete. This is scoped operational evidence,
without a new quality score or a continuous-run claim.

Growth admits all 9,000 additional documents in 2,250 four-item groups. Public
status reports complete, with no pending group or Change. The resulting vault
has **10,000 current captures / 1,004,348,092 content bytes**, 10,002 retained
Sources and 10,013 revisions. The planned interrupted import recovers and resumes;
four selected captured identities retain their original mapping.

The next ordinary `index sync` fails the prospectively frozen **five-second
no-change limit**. The owner sends SIGKILL after 5.016 seconds; there is no output,
RSS violation or observer error. This is a censored runtime exceeding five seconds,
not a completed-sync measurement or corruption finding. Consequently the 10K
initial quality questions, churn/update measurements, rebuild comparison and
25K growth never run. Both original attempts remain failed.

A faithful disposable copy subsequently passes the unchanged CLI's full canonical
and cache check in **173.575 seconds**, with zero errors, revision-owner history
checked, cache agreement and approximately 759 MiB native process peak RSS.
Every original byte, path, mode and timestamp is preserved. Qualification covers
this diagnostic seed; it does not repair the earlier timing failure. Complete
normalized dependency/row equality and unused-retained-payload qualification have
their existing explicit limits.

One separately frozen, test-instrumented native observation calls the ordinary
no-change service and returns the exact prior snapshot with `reused:true` and no
build. Its service interval is **29.516 seconds**; launch-to-reap is 29.743 seconds.
The diagnostic uses a 200 ms lock timeout instead of the CLI's 5,000 ms default,
and excludes CLI parsing. It cannot substitute for the public five-second gate.

| Disjoint service phase | Seconds |
| --- | ---: |
| Initial and final membership census | 1.679 |
| Initial note read/hash | 4.055 |
| Parsing/materialization | 0.438 |
| Dependency SQL and asset read/hash | 9.960 |
| Final note, asset and layout recheck | 13.270 |

Logical I/O reconciles to two passes over 14,908,260 note bytes and 2,011,229,140
asset bytes, plus 1,294,530 layout bytes: **4,053,569,330 bytes**. The reported
logical-I/O ledger accounts for these two bulk-payload passes; no third pass
appears in that ledger. Parsing contributes only 1.48%; removing it would leave
29.079 seconds. It fails the predeclared materiality/headroom screen, so deferred
parsing is rejected as this workflow's solution. Read-bearing phases do not yet
separate pathname resolution, binding checks, streaming/hash and SQL costs.
The subsequent canonicalization screen, below, also rejects its proposed reader
change. Neither observation justifies bypassing fresh byte, membership or
authority checks.

Afterward, an ordinary selected verified search takes 0.539 seconds and an exact
cited read takes 0.020 seconds; returned bytes match the canonical range. These
two development commands supply neither a latency distribution nor answer-quality
credit. Full preservation finds only expected writer-lock ownership changes and
SQLite shared-memory timestamps on the copy, with canonical/history/catalog bytes
unchanged. Native process RSS is observed; exact tree RSS and transient allocation
remain unqualified. Default completeness remains 9/28 facts and 2/10 positive
tasks. Native HIGH, semantic readiness, the remaining 10K lifecycle, practical
25K capacity and full release remain open.

### Reader attribution and closed optimization hypotheses

A separately frozen refinement returns the same unchanged snapshot and logical
usage in **30.241 seconds**. Independent review reconciles the nested timers,
returned bytes and publication. Its disjoint reader categories are:

| Reader category | Seconds |
| --- | ---: |
| Path resolver, including canonicalization | 6.801 |
| Fresh leaf checks and opening | 2.026 |
| Streaming, hashing and allocation | 18.333 |
| After-read binding checks | 0.274 |
| Enclosing reader residual | 0.516 |

Canonicalization is a child of the resolver, not an additional disjoint phase.
Eligible calls account for **5.449 seconds / 18.02%** of the whole service;
raw-layout calls are excluded. Even free canonicalization would leave **24.792
seconds**, failing both predeclared conditions: at least 30% removable residence
and at most four seconds remaining. The directory-reader proposal is rejected
as the next solution to the five-second gate. No retry or combined-removal claim
follows. Streaming/hash/allocation remains internally unattributed.

The actual linked BLAKE3 Rust compiler response confirms optimization level 3,
no debug information and native aarch64 targeting. Unoptimized BLAKE3 Rust is
therefore rejected as a cause; every other dependency and native build-script
object is not separately qualified. The refinement is 2.46% slower than the first
observation, an overhead sanity check confounded by cache/order variation, not a
calibrated observer-cost measurement. All original bytes, paths, modes and
timestamps remain unchanged; the copy again differs only in expected lock
ownership and shared-memory timestamps.

Normalized ordinary lexical search and omitted-scope context use the published
catalog directly. Global sync is explicit maintenance and external-edit
discovery, while acknowledged CLI-managed writes publish their selected changes.
Legacy ordinary search has different verification behavior. Fast selected
observations neither make strict sync pass nor establish global freshness.

### Exposed native evidence-boundary audit

A bounded development audit reuses two exposed incomplete tasks and one complete
control at the restored 10K publication. Actual shipping defaults are unchanged:
ten owners, 80 candidates, 1,024-byte excerpts, 12,000 rendered bytes and 3,000
estimated tokens, with no additional scope or filters. Six ordinary/prepared
responses retain the exact publication. Independent review authenticates all
27 default citation occurrences across 22 distinct ranges and active heads.

The three-condition Q041 failure is not reproduced; Q001 remains complete.
Q061 still omits a requested condition even though its support appears in the
prepared cards from an owner already returned in the default packet. This
establishes availability at the exposed selection interface, not a complete
legal alternative packet or the internal cause of omission.

The planned oracle replay fails during operator preparation: its reply file is
absent, and the CLI refuses before inspecting a valid reply. The eighth and final
native attempt is retained without a retry. Complete same-budget avoidability
remains **unproved**; standalone card lengths are insufficient. No native
optimizer, weight/cap change or quality gain is justified by this audit. A
subsequent assisted workflow must use only the task and public evidence, account
for its additional work, and remain separate from native-default acceptance.

### Selected 10K lifecycle continuation

A separately admitted continuation executes the remaining selected lexical
workflow on a faithful copy of the qualified 10K corpus, using the same accepted
native release executable. The original failed sync and the first continuation's
process-observer failure remain failed. This continuation performs **952 commands:
947 successes, four expected refusals and one timeout**. It completes 700 context
requests: 100 tasks repeated three times initially and after churn, followed by
100 requests after complete cache-loss reconstruction. Each phase also includes
20 search audit tasks and their conditional exact reads.

The frozen profile uses five owners, 80 candidates, 1,024-byte excerpts, 6,000
rendered bytes and 1,500 estimated tokens. It is a different evaluation profile
from the shipping context defaults. Independent inspection of returned evidence
finds:

| Phase | Supported required facts | Complete positive tasks | Safe zero-current-fact controls |
| --- | ---: | ---: | ---: |
| Initial | 120/152 | 60/92 | 8/8 |
| Post-churn | 123/148 | 64/89 | 11/11 |
| Rebuilt | 123/148 | 64/89 | 11/11 |

Withdrawal changes the denominators; these are different corpus states, not an
algorithm improvement. The four licensed Cargo tasks complete only one task in
each state. A refreshed deployment-lead condition regresses, while the 95
unchanged synthetic fact units lose no previously supported fact. Unfiltered
absent-target questions can return unrelated evidence; honest evidence labels
do not demonstrate a generated answer's semantic abstention.

All **3,460 structured citation occurrences / 444 distinct ranges**, 3,216
rendered passage mirrors and 62 exact reads authenticate with correct currentness.
All 100 post-churn/rebuilt context pairs, 20 search pairs and 19 conditional read
pairs match under the frozen exclusions. Dependency-fingerprint and complete
normalized-row equality remain open. Owning context-request p95 is
**0.211 / 0.227 / 0.235 seconds**, with maxima below 0.469 seconds.

One hundred Source head changes complete, including 90 new revisions and ten
historical reactivations. Ten withdrawals leave 9,990 current Sources. True no-op,
historical/current/withdrawn reads, complete backup, full checks and offline
cache-loss reconstruction pass; rebuilding takes 121.432 seconds. Dry-run,
selected tampering, duplicate identity, unselected cache omission and insufficient
proof controls pass, including restoration. The fixed Source refresh/read/no-op,
add/withdraw and control-Page initialization also complete.

The ordinary external-editor deletion workflow then **fails its 60-second gate**.
Sync reaches a censored timeout observation at 60.023 seconds with no output;
the whole deletion interval reaches 60.025 seconds. Its native exit and completed
duration are unavailable. A subsequent controller cleanup permission error
prevents the terminal ledger; a later exact-PID check finds no remaining process.
Deletion disappearance, restoration, retirement, all 1,000 separate genuine bulk
updates and final accounting remain **unrun**. The 100 head changes do not satisfy
the bulk-update gate. No whole-tier score, strict sync pass, 25K qualification,
native quality gain or release acceptance follows.

### Rejected seeded external-Page sync candidate

A subsequent candidate attempted to reuse the selected catalog's unchanged
captured text and retained revision ownership while reconstructing the complete
canonical graph and policy projection. It cloned the SQLite catalog into an
unpublished candidate; it preserved the full external-input checks and used the
ordinary full-build fallback for unsupported changes. This was a performance
experiment, not an accepted change to ordinary sync.

The native macOS ARM64 release build used optimization level 3 and no debug
information. Candidate executable SHA-256:
`86c4caf0e3763ed1add13c32d7d33dce6d238c7457dafa428df5884f7a37550b`.
All **42 affected native tests** passed, including twelve new cases for seeded
reconstruction, full-build comparison, both layouts, fallbacks, committed WAL,
copy limits and acknowledgement recovery. Independent small-fixture commands
demonstrated captured reuse, external edits, deletion/restoration, exact citations
and agreement with independent unseeded reconstruction on both layouts.

The frozen small-fixture acceptance nevertheless failed. Its tiny authored Page
was a **draft**, which the existing document-context authority rules exclude;
exact read and lexical discovery worked, while context remained empty under both
seeded and unseeded reconstruction. An explicit draft-status filter does not
override that exclusion. This is a task/contract mismatch, not evidence for a
short-text retrieval defect. The critic also exceeded its declared inspection
allowance by repeatedly reading its receipt log. Both original failures remain;
the gate was not rescored.

One complete, independently qualified copy of the pre-deletion 10K state then
ran the frozen large-vault experiment. The original 90,658-byte control Page read
passed. Its external deletion followed by ordinary sync again **failed the
60-second whole-task limit**, at **60.034 seconds**. The supervisor killed and
reaped the owned process with exit -9; it emitted no result packet. Natural
completion time and a speedup against the earlier censored timeout are unknown.
Observed native and sampled process-tree peak RSS was **803,831,808 bytes**;
226 process-tree observations had a maximum gap of 0.285 seconds. These are
observed resource bounds, not exact transient maxima.

The old publication remained selected at epoch 2708. The new unpublished
candidate contained retained owners and reconstructed metadata, but remained in
the building state without final commitments. Partial rows demonstrate progress,
not completed reuse, publication, deletion correctness or the exact bottleneck.
Deletion absence checks, restoration, same-size/restored-mtime edit, subsequent
checks and cache-loss comparison were **unrun**. A prior relative-path controller
launch failed before starting a product process and remains recorded separately.

The candidate was not promoted. Its source, executable and failed evidence remain
preserved; only its six isolated working-source paths were restored to the
accepted implementation after checking all 495 frozen build inputs. The passing
component checks do not establish a useful large-vault speedup. Further sync
tuning is deferred while a fresh architectural review returns to default-context
completeness. The five-second global-sync failure, 1,000 genuine updates, practical
25K capacity, default-quality advancement and full-release acceptance remain open.

### Separate 1,000-update scalar measurement

The accepted optimized native executable then ran a separately declared
standalone update experiment on a complete qualified copy of the preserved
post-churn 10K backup. The starting state retained 9,990 current Sources,
10,002 Sources, 10,103 immutable revisions and all 1,000 authored Pages.
Each of 1,000 selected local inputs was verified different from every retained
revision of its owner; historical reuse and no-ops could not satisfy this gate.
The whole interval included request construction, input hashing and public calls.

**The 600-second gate failed: 272 updates completed, with attempt 273 interrupted
at 600.033 seconds.** Independent inspection authenticated all 272 successful
receipts, their consecutive publications, exact new payloads, current Source
heads and preserved old revisions, without a prefix correctness error. The
remaining 727 updates were unrun. This censored result does not establish a
complete baseline time, a speedup or full-tier acceptance.

The interrupted attempt had installed its four canonical targets and durably
reached FILES_APPLIED, but had not published or acknowledged its intended epoch.
A separately bounded ordinary `recover` exceeded 60 seconds without completing.
The existing exact `changes apply` for that one retained Change subsequently
completed in 0.708 seconds; exact Source and asset bytes stayed unchanged,
authority became idle and publication advanced once. The three-call recovery
observation took 1.296 seconds. This finishes the interrupted intent without
retrying the Source refresh or rescoring the original throughput failure.

The measured repeated-write cost admits a bounded batch of guarded existing
Sources: one joint dependency projection, one retained Change and one publication.
That candidate remains unqualified until correctness and complete paired
performance pass. Require all 1,000 candidate captures within 600 seconds and
at least 1.5 times improvement against a complete, prospectively pinned scalar
arm on the same prestate and inputs. Keep default retrieval quality, external
sync, whole lifecycle and 25K acceptance separate and open.
# Unpromoted parallel final maintenance recheck

This experimental branch bounds the final full-content recheck to four scoped
hashing workers and four pending results. Initial capture, dependency observation,
path resolution, descriptor admission, mutable accounting, parsing, SQL and
publication remain serial. The single cumulative byte allowance and original
deadline apply to all workers; tight budgets drain pending work and fall back to
serial reads. Stable-file identity, complete hashes, ordered errors, failed-read
byte charges and the final serial layout check remain required. Catalog formats
and public commands do not change.

Independent source review and the native release checkpoint pass **79 tests**,
with one authentic historical-fixture test intentionally ignored. These checks
cover serial/parallel accounting parity, exact-budget fallback, file growth and
replacement, restored-mtime changes, worker panic/error order, Page publication,
predecessor preservation, recovery and complete-cache-loss reconstruction. The
initial CLI launch failed before application execution because its pinned binary
environment variable was absent. Supplying that executable and completing an
omitted maintenance test module required no code changes or repeated passing
groups. Compilation used native macOS ARM64, Rust 1.98 and release optimization
level 3 with debug information disabled.

**Performance is unmeasured and this candidate is not promoted.** Existing timing
evidence does not justify launching the 25K external-edit-to-verified-read task
under its unchanged 60-second limit, including required account inventories and
monitoring. External-edit, interrupted recovery and complete backup/rebuild at
25K remain open. This checkpoint establishes bounded mechanics; it supplies no
25K qualification, default-context improvement or full-release acceptance.
