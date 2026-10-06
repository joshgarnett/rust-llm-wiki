# Normalized embedding workflow checkpoint

Current source connects bounded embedding preparation, cached semantic/hybrid
document search and cited document context on an explicitly activated normalized
catalog. The affected native correctness checks pass, and a frozen public
real-vector run demonstrates offline reuse and restoration. Multi-source evidence
completeness failed; full public acceptance and 25K capacity remain open.
This checkpoint is newer than the packaged 0.2.0 candidate010; that archive does
not contain these changes.

## Implemented workflow

Preparation uses exact authenticated Current Page, unmanaged-note and captured
document inputs. Identical formatted inputs share vectors while retaining every
owner's read guards. Own operational publications and unrelated updates do not
change an owner's embedding identity. Source refresh, withdrawal or changed
rendered input invalidates the affected membership. Selected inputs are checked
before dispatch, on received-output recovery and before membership commit.

`embeddings sync` uses the configured remote provider to acquire missing inputs.
An offline sync uses only the retained active space and matching preparation
settings, without loading private provider configuration or resuming accounting
jobs. It reports actual missing coverage; it cannot recreate lost vectors. Without
an active space, check's zero counts have an unspecified, unenumerated denominator;
they do not establish an empty corpus. Empty
or dimensionless preparation does not replace an established active space.

Current document search supports semantic and hybrid modes. It uses compatible
retained vectors, exact rendered input hashes and filters before dense selection.
Plain search is cached discovery. `--verify-selected` authenticates the displayed
owners, attaches exact captured-source citations and rechecks selected dependencies
before returning. Automatic `indexed-documents` context shares the existing
bounded passage allocation and selected proof. Query vectors must be compatible
with the retained active space; offline misses refuse explicitly unless the user
selected the existing lexical fallback.

For a prepared vault with cached query vectors:

```sh
lwiki --wiki /path/to/wiki --offline search 'release checklist' --mode semantic --verify-selected --no-sync
lwiki --wiki /path/to/wiki --offline context 'release checklist' --mode hybrid --scope indexed-documents
```

Retained job publication uses the normalized version-3 Run/RunEvent path without
constructing a legacy whole-vault projection. Never-applied candidates can rebase
only with exact original target and input agreement; acknowledged candidates
retain their original identity. Received-output recovery preserves paid bytes,
no-resend behavior and unknown accounting holds. Missing named proof authority
refuses bound continuation instead of searching unrelated history. Status can
still report accounting with an explicit authority warning.

## Validation and its limits

The native macOS ARM64 release checkpoint uses Rust edition 2024 and compiler
optimization level 3. Across retained evidence, **98 distinct affected checks
pass**: 62 selected unit/workflow tests, 28 semantic integration tests and eight
normalized document CLI tests. The nine embedding workflow tests cover reuse,
offline/cache misses, exact cited semantic and hybrid results, filtering,
refresh/addition/withdrawal, received stale-input recovery, commit rollback,
cursor binding and vector-read limits. Providers in these checks are local mocks.

Original compiler and harness failures remain preserved. Grouped fixes addressed
exact JobBatch fact admission, proof-deadline checks and invalid test fixtures.
A recorder filter error omitted eight workflows from one replay; those eight were
run separately. Five final adversarial fixture corrections were replayed together.
Unchanged passing checks were reused. This is an affected-check checkpoint,
not a full repository suite or proof of live-provider compatibility.

Fresh independent Astra reviews covered the job admission/rebase invariants,
semantic coordination and the five runtime fixture failures. These source and
component reviews do not substitute for the declared public-command assessment.
The later public observations below do not qualify unavailable public fault cuts
or total preparation-work counters; those keep the full gate open.
The earlier [dense development comparison](validation-native-dense-context.md)
still failed its quality gate. No new ranking or allocation quality improvement is
claimed here, and the unseen native HIGH questions remain isolated.

## Frozen public real-vector observations

A separately declared offline run executed 32 exact CLI invocations from the frozen
64-slot protocol, using the pinned release binary and existing real provider
vectors. Independent review verified all expanded arguments and returned evidence.
No new embeddings or model-assisted answer stage ran.

Current preparation reports **223/223 available eligible inputs**. Cache-only sync
publishes membership with 223 reused and zero generated inputs. Request-only
dry-run preserves the complete byte-and-mtime inventory. A mutable Source-title
update retains immutable Revision bytes and vector reuse. Complete derived-cache
loss produces explicit offline semantic unavailability; restoring the actual
compatible vector backup restores coverage and identical evidence. All five
declared lexical/restoration comparisons match substantive output exactly.

Both semantic and hybrid modes complete the ordinary and paraphrased positive
questions, but **both fail the complementary multi-source question**. All required
owners are retrieved, while the failed packet contains only one of five required
facts, one partial fact and three missing facts. Four Output passages consume
most of its 5,991-byte/1,498-token packet; Testing contributes only a 153-byte closing
paragraph. This establishes a loss after owner discovery. The returned output alone
does not establish the rank of every discarded candidate. These exposed development
questions do not qualify unseen HIGH or demonstrate broad quality improvement.

The scoped absent-information question produces no invented requested measurement
or global absence claim, but returns unrelated material without a useful explicit
answerability classification. Owner retrieval, relevance, evidence completeness
and citation integrity remain separate: **74/74 returned citation occurrences**
pass exact UTF-8 span/text, Source/Revision/vault identity, full-source hash and
quote-hash verification. Correct citations do not recover omitted facts.

The owner completes in 85.395 seconds, 85.450 seconds cumulatively including a
preserved pre-CLI admission failure. About 80.496 seconds are strict scans of all
retained artifacts; summed command intervals are 3.339 seconds and include observer
overhead. Runtime allocation is 89.596 MB. These small-fixture observations do not
qualify shipping latency or 25K capacity; physical I/O and exact process-tree peaks
are unavailable. Original source/vector before-and-after pins agree. External
acquisition history is hashed initially and checked for unchanged metadata later;
it is neither transplanted nor a demonstrated historical recovery.

Thirty mock/fault slots and two stale-input slots remain unrun. The latter fixture
edit refused a quoted title field before launching a command. A retained socket
also caused the first admission attempt to stop with zero CLI starts; the corrected
metadata-only disk sampler counts every retained entry without opening or removing
it. Conservative cumulative time/read/artifact charges preserve that failure.
Neither gap is a passing safety observation. Missing fault/telemetry rows and the
observed completeness failure keep the unchanged >=9/10, every mandatory row,
zero-blocker gate open. No partial full-gate score is assigned.

## Capacity boundary and next milestone

A bounded, zero-provider lineage replay reproduces all eight development packets
exactly apart from their verification timestamps. Independent inspection finds
existing admitted proposals for every missing multi-source fact. Two four-proposal
counterfactual packets, one per mode, fit the unchanged renderer and limits at
**5,392 bytes / 1,348 estimated tokens**. The critic verifies all five required
facts in each rendered packet and all eight quote hashes. This establishes
allocation loss for this exposed task: complete evidence fits the existing budget.
The critic-selected witness is diagnostic evidence, not an automatic selection
workflow or a quality acceptance pass. The production default remains unchanged.

The prospectively declared selection comparison is closed: **no candidate
qualifies; retain the unchanged default.** It compared baseline B, literal
positive-IDF query-term coverage L, query-weighted facility coverage F0 and the
same facility objective with one exchange sweep F1. All 32 internal app-dispatch
development calls completed on the same four tasks in both modes, with unchanged
candidate formation, exact rendering, freshness, excerpt/owner limits and the
shared two-second proof deadline. These calls do not establish public CLI parity.

| Arm | h02 full facts, semantic / hybrid | q02 full facts, semantic / hybrid | q03 full / partial / missing, semantic / hybrid |
| --- | --- | --- | --- |
| B | 3 / 3 | 4 / 4 | 1 / 1 / 3 in both |
| L | 3 / 3 | 4 / 4 | 0 / 2 / 3 in both |
| F0 | 0 / 0 | 2 / 2 | 1 / 0 / 4 in both |
| F1 | 0 / 0 | 3 / 2 | 1 / 0 / 4 in both |

Every positive output retained the expected owners. All 148 citations matched
freshly checked immutable source slices, full-source and quote hashes, and the
literal citation JSON in rendered text. All packet, excerpt and per-owner caps
passed. The eight q23 packets invented no measurement or global absence claim,
but returned irrelevant evidence. Owner retrieval, citation integrity and topic
coverage therefore do not demonstrate requested-fact completeness. L loses full
q03 support; F0/F1 also discard evidence that made ordinary questions complete.
The earlier complete 5,392-byte witnesses rule out insufficient final capacity
as the explanation for this exposed case.

The optimized native experiment made zero provider calls and 2,141 exact render
trials. Its owning process measured 2.001 seconds including observation and
cleanup; this is a small internal diagnostic, not shipping or inference latency.
Counted/reserved reads were 919,004,833 bytes under 1 GiB. Critic read accounting
remained within the reserved 16 MiB; retaining an additional 600-second review
debit made cumulative charged work 963.001 seconds under 1,200. Physical I/O and
exact process-tree peak memory were unavailable. One failed compile and one
grouped caller repair precede the successful native build; 18 focused mechanics
checks passed, with three local mock acquisitions isolated from the real fixture.
This is not a full repository suite or live-provider qualification.

Optional local evidence is under
`.artifacts/normalized-semantic-lifecycle-001/critic/public/evidence-set-results-001.{md,json}`;
the JSON SHA256 is
`6a49891d93cbaa7039ea94b2f7fc777134e3bf9ceb57c63a49bafaa97974eef9`.
It records every cell, regression, omission and output hash. The frozen protocol
SHA256 is `271970d28bca573e4cd89cdf16636ca9723f98e2bf6603ef348fd1cdcce29a1a`;
the owned test executable SHA256 is
`f58060252927a2370a8134b8e629981a1a43d49ec179f8453126e6fb6563c54b`.
The exposed questions remain development data; native HIGH remains quarantined.

The preceding preparation path retained at most 4,096 owners and rendered the
eligible corpus twice for exact semantic discovery under a shared
64 MiB/65,536-unit allowance. The compact inventory milestone below replaces
that whole-corpus rendering path. Exact discovery still scans compatible vectors
and retains finite descriptor/vector-read limits, so 25,000 documents / roughly
2.5 GB remains unqualified. These are source constraints, not measured throughput.

Fresh architecture review rejects further tuning of these surrogate objectives.
One separately declared, blind task-aware selection control on the existing
exact candidates can test whether explicit interpretation of requested conditions
recovers the missing support. It must account for host input, calls, usage and
observed turnaround, and gets no deterministic or unseen acceptance credit.
If unchanged native replay is unavailable, a read-only selection diagnostic must
leave final packing untested. Reuse the existing public lexical investigation and
[cited-Page workflow](context-selection.md) for a separately assessed complete
question-to-brief, immutable factual refresh and unrelated-vector reuse milestone.
Normalized public host selection remains lexical only. Do not add a CLI answer
runtime or present an assisted score as a native pass.

The scale milestone must connect changed-owner preparation, useful discovery and
storage/rebuild attribution in one release-profile control. Compare repeated
corpus rendering with a compact rebuildable unit inventory on fixed small
owner/payload sizes, initial/no-op preparation, one-owner refresh, query and
reconstruction. Attribute rendering, compatible-vector decoding/scoring and
durable publication separately. Paged preparation alone cannot qualify 25K:
the [fast-query contract](testing-large-vaults.md) also forbids exhaustive vector
scanning per ordinary query. Preserve exact retrieval as the comparison oracle;
choose a bounded candidate-generation design only after measuring the remaining
work and its effect on actual evidence. Wider caps, new dependencies and a general
repair subsystem are not justified by this experiment.
Representative capacity, default activation, full command parity and native HIGH
remain separate acceptance gates.


## Compact inventory and resumable preparation candidate

The normalized candidate now stores policy-bound compact unit descriptors and
a durable owner inventory in the rebuildable catalog. Policies bind parser,
renderer, segmentation and all embedding settings separately from embedding-space
identity. Preparation backfills in 128-owner pages and acknowledges authenticated
owner versions in the retained vector store. Each invocation has a finite deadline
and page allowance; repeated commands resume durable progress. A physical catalog
rebuild changes acknowledgment authority while retaining compatible vector blobs.

Managed publications update changed document descriptors and invalidate enrolled
proof dependencies even when eligibility and rendered input stay unchanged.
Unchanged acknowledged owners need no canonical reread or rendering during repeat
preparation. Generated/reused input counts describe selected work during that
invocation; available-unit coverage describes the retained inventory separately.
The report explicitly does not claim a complete vector-blob integrity audit.

Exact semantic and hybrid discovery consume compact descriptors, select winning
owners through indexed lookup, then authenticate and rerender only selected
canonical evidence. They still perform a finite exact vector scan. This is an
intermediate oracle, with no fast-query, 25K-capacity or retrieval-quality claim.
The failed allocator comparison and unchanged production Baseline remain intact.

Three integrated native macOS ARM64 release checkpoints supply 63 distinct passing
affected checks. The first runtime gate retained 44 passes and five
failures; grouped fixes replayed the affected workflows and native/compact audits
with 36 passes, one replacement-fixture failure and two ignored diagnostics.
The remaining fixture used an unquoted 39-digit Source ID, which invalidated its
Evidence envelope; its quoted-ID correction and explicit Current-chain assertions
passed one focused replay. These are aggregate affected checks, not a full
suite, a single green run or independent public-command acceptance.

Passing workflows include 129-owner paged preparation with an interrupted durable
Received response, no resend and stable ready-owner acknowledgments; unchanged
preparation with zero owner authentications or rendered bytes; immutable factual
refresh with truthful offline missing coverage, unrelated-vector preservation and
new exact cited semantic/hybrid results; and catalog reconstruction/cache
loss/restoration with retained compatible-vector reuse. Providers are local mocks.

The original shared-input fixture exposed an unpublishable receipt: 128 identical
Pages require 129 source guards plus the current Run guard, exceeding the existing
128-precondition publication limit. That checkpoint rejected more than 127 source
guards before dispatch, with a zero-call workflow check. It preserved the complete
guard union and existing receipt/accounting contracts, while leaving shared-input
completion open. The later automatic preparation milestone below replaces that
all-owner paid scope with authenticated supplier scopes. Intrinsically oversized,
uniquely supplied inputs and selected-reader resource limits remain bounded refusals.

Exact schema checking admits either the original catalog schema or the complete
optional inventory schema, rejecting partial or arbitrary extra objects. Native
B-tree/index checks and canonical catalog/FTS comparison remain in force. Their
canonical-agreement result does not certify semantic inventory completeness or
vector-cache health. Selected preparation and query proof checks remain separate.

The prospectively declared small synthetic mock-backed public inventory lifecycle
now passes independent acceptance at **10/10, all 17 mandatory observations and
zero observed correctness blockers**. Its repaired packet ran three existing
helpers and 14 actual offline CLI commands on the pinned release binary. The
original setup failure remains separate: two helpers, zero CLI/provider calls,
with its complete failed-stage allowance retained. No result was overwritten.

Initial preparation acquired 192 inputs in 12 mock requests. Unchanged offline
preparation reported complete 192/192 coverage with zero generated/reused work.
Both semantic and hybrid context returned the exact old observation and Unicode
phrase. Managed refresh preserved the Source ID and every old immutable asset;
offline preparation truthfully reported 191/192 available and one missing input.
The interim context disclosed incomplete coverage and emitted no stale marker.
Authorized mock reprepare acquired exactly one new input and reused two selected
inputs. Both complete-state modes returned the new fact with its new immutable
Revision citation. Catalog reconstruction changed physical incarnation; offline
preparation reused 192 inputs, and rebuilt passage objects matched their prior
versions exactly. The final check completed with canonical/cache agreement and
zero diagnostics.

Independent inspection verified **70/70 citations** against exact UTF-8 slices,
full-content and quote hashes, vault/Source/Revision identity, Current labels and
literal rendered citations, plus **192 initial immutable revision files**. Every
context packet uses 11,703 bytes / 2,926 estimated tokens below the declared
12,000/3,000 limits. Packets remain truncated/partial with distractors and omissions;
only the narrow requested fact is complete in all six complete-state observations.
The incomplete-state query is a coverage/refusal task, not an answer-quality pass.
The fixed mock geometry supplies mechanics evidence, not learned relevance.

Across the preserved failure and repaired packet there are 19 child invocations
and 25.005783084 charged recorder seconds. Observed combined allocation is
24,522,752 bytes with 60,576,096,256 free bytes at independent verification.
Monotonic recorder intervals and asynchronous review wall time are separate;
these small synthetic observations do not establish shipping latency. Logical
reservations and disclosed reads were reconciled conservatively at
2,145,963,032 / 2,147,483,648 bytes, including the failed stage, admission reads,
validator reads, CLI counters and finalization allowances. This is not a complete
physical-I/O measurement. The evaluation is closed; packaging is separate work.

Production journals retain all 13 settled mock attempts as `unknown_reserved`;
billable units/cost remain unknown. Every actual public CLI observation reports
network false. This is not public online setup or live-provider compatibility.
Compatible-input acquisition and later complete offline reuse pass the declared
workflow; this is not a separate byte-for-byte audit of every retained vector blob.

The independent result JSON SHA256 is
`ef61b92aa197b20e5756b599bd5a61a4bb6ad54a2a8b62786cdff2a854d68f95`;
the Markdown SHA256 is
`7296632dda121d01d0c595a1d207c3ca938d01a2a665c434df22a4edbc8d76e6`.
The tested release CLI SHA256 is
`75458e73a89b4b55aeae583868c69772a0a4095b7e9fbe55ee72bc6e27553eec`.
Original and labeled-repair protocols remain frozen, with no query/geometry,
retrieval-bound or acceptance-threshold change.

This is a useful point for a new scoped local 0.2.0 trial. The release review found
stale embedded agent guidance denying implemented normalized semantic/hybrid
retrieval; its correction is a separately verified release-data change, with
core workflow checks retained rather than represented as new-binary replays.
The next consequential milestone is the ordinary collection loop: automatic
receipt-feasible owner groups, bounded semantic discovery chosen after controlled
release attribution, cited investigation, factual refresh and recovery through
representative tiers toward 25K. Broad answer completeness, normalized parity and
default activation, native HIGH, fast queries and capacity remain open.

Optional local protocols, source freezes and complete failure logs are retained
under `.artifacts/normalized-semantic-lifecycle-001/inventory-001/`; independent
source reviews and the prospectively frozen public protocol are under the sibling
`critic/public/` directory. Tracked documentation does not require those ignored
artifacts to understand these limits.

## Automatic receipt-feasible preparation

Normalized preparation now selects one authenticated feasible supplier for each
missing input before merging paid-task guards. It packs distinct inputs by the
actual guard union, provider item count and encoded wire bytes, reserving the
current Run guard within the existing 128-precondition limit. It acknowledges
every reusing owner through its separate current proof. Identical content no
longer requires users to rearrange documents or acquire the same vector repeatedly.
An intrinsically oversized unique supplier refuses before page dispatch, with
required/available counts; another feasible supplier or a compatible cached vector
permits fully authenticated reuse. No receipt, catalog or vector-store format changes.

Tasks within one preparation page retain one Run and its lifetime allowance.
Received outputs replay before pending paid work. Terminal reconciliation does
not grant a new Run or release unknown charges; uncertain retry remains explicit.
Proven pre-send releases retry within the same Run. Budget exhaustion pauses that
Run so the existing explicit amendment can continue it.

Independent acceptance passes at **9/10, all six frozen mandatory tasks and zero
observed blockers within that scope**. There are **61 distinct affected native
passes**: 30 unit and 28 semantic integration passes, followed by three corrected
test replays. Twelve new regressions cover 128/129/257 repeated owners, exact
126/127/128 guard boundaries, overlapping and disjoint scopes, recovery and
two-request exhaustion/resume/amendment. This is aggregate evidence, not a full
suite or an originally green run. Compiler and runtime failures remain retained.
Two repaired mechanism fixtures use authenticated historical Entity incidence;
their original dense-Evidence read refusal and the disjoint failure's unavailable
underlying error are not promoted into resolved capacity findings.

The pinned native release CLI completed 19 public lifecycle steps: three existing
mock/fixture helpers and 16 offline commands outside a vault path containing
spaces. Independent verification checked **91 exact citations**, all 192 original
Revision assets and 63 unchanged unrelated Source envelopes. Initial preparation
acquired 192 inputs in 12 mock requests; factual refresh acquired one new input in
one request, reused unchanged inputs and truthfully exposed 191/192 coverage
before acquisition. No-op preparation generated/reused nothing. Ordinary offline
reconstruction reused all 192 inputs; substantive cited evidence matched the
prepared packet, and final canonical/cache checking reported zero diagnostics.
All 13 validated attempts retain unknown accounting charges.

The CLI SHA256 is
`3cb2e7a3ed00b6ca1d7ac0a10c2e89aea10edd1e4e633acadf1ddc2339592caf`.
Its complete 468-input native freeze has SHA256
`f1c0a5dfe00bacdbb97b303c5263670ea570c917e536bb61b2500c06345504e9`;
the public summary has SHA256
`9e4f8efd63a30ed2b4115f99eb57abbbbf45c6a7fee6013518cafa0fb631881f`.
The native build uses rustc 1.98.0, macOS ARM64, optimization level 3 and no debug
information. Detailed local evidence is optional under the sibling
`guard-preparation-001/` and `critic/public/` artifact directories. Preserved private
test hooks remain local; the compiled source is disclosed as dirty.

This milestone is later than packaged candidate011 below. At that checkpoint,
mixed stale suppliers with unsent sibling tasks remained safely stranded. A
separate source review found that per-Run limits and recomputed deadlines did not
establish an aggregate invocation allowance or startup deadline across multiple
fresh/resumed Runs; its proposed public reproduction was then unrun. Both were blockers for the integrated
collection workflow, outside this one-page partitioning gate. Exact vector scans,
broad fact completeness, native HIGH and practical 25K qualification remain open.
The next work is a complete cited-brief/update/recovery workflow and one admitted
default-geometry collection attribution experiment, with task completeness,
citation integrity and resource measurements assessed separately.

## Invocation allowance and independent recovery

The later 2026-10-06 batch addresses two blockers in ordinary preparation. One
operation-owned allowance now composes request, byte, unit, cost, concurrency and
rate limits across owner pages, recovered pending tasks and retries. The original
operation deadline composes with retained Run deadlines. Exact owned attempts
are projected from the existing ledger; authenticated settlement can recover
slack, while unknown charges remain reserved. Replaying a received response
does not send another request. A fresh explicit command gets a fresh operation
allowance without changing a retained Run's lifetime limits or deadline.

Historical authority is limited to authenticated cache-only embedding work.
Immutable descriptors, original hash-bound receipts and original settlement
events remain required after legitimate spool cleanup or later billing
reconciliation. Pending work, live dependency closures and global Run guards
stay current. Authenticated rejected tasks finish failed; independent current
tasks continue in their original Run, which truthfully finishes failed if any
task failed. A distinct current input may be prepared under remaining allowance;
an equivalent failed scope does not silently fund replacement work.

Rejected setup probes have a separate narrow continuation check. A settled,
terminal, one-task embedding-check rejection with exact descriptor, receipt,
Space and current global guards can be left paused with its warning and unknown
holds while ordinary collection preparation proceeds. Tampered receipts or
unresolved attempts refuse before another corpus request. This does not finish
the probe, release its holds or broaden historical embedding authority. The batch
also restores strict retained change-proof decoding for the existing legacy and
indexed variants; no ledger, receipt, marker or catalog format changed.

There are **205 distinct source-qualified affected native passes**: 138 unit,
28 semantic retrieval, 21 API extraction and 18 remote CLI checks. Passing
unchanged controls are retained from the earlier frozen checkpoints; changed
paths received targeted replays. This is composite evidence, not one green broad
suite. The 532-cut ledger fault sweep passed. The 370-cut bootstrap sweep timed
out and remains unqualified; four ignored helper/diagnostic tests are not passes.
Compile, runtime and runner failures remain retained. Repairs were grouped after
serial checkpoints; their cumulative owning-process interval was
1,387.762983542 seconds within the original 1,500-second ceiling.

The baseline public CLI actually sent five requests under a four-request command
limit. With the final release binary, the same 129 distinct short Pages, provider
batch maximum 32, default 12,000-byte geometry and no quality target produce
**four requests of 32 inputs**, then `BUDGET_EXCEEDED` with 128 generated inputs,
the retained Run ID and explicit continuation advice. Repeating the identical
authorized command sends **only the remaining input**. Final online and offline
preparation both report 129/129 cached coverage. Original Run specifications,
limits, start/deadline, receipts and unknown-charge holds remain intact; the
first command's incomplete active-space state has an explicit offline refusal.
The recorder's owning-process interval was 7.448774 seconds; the eight children's
own duration sum was 6.921723 seconds. There were five loopback mock requests and
zero real-provider calls. Independent Astra inspection passes the declared
mechanics control: actual input bodies, original Run specifications/deadlines,
journal prefixes, receipts, markers, immutable Pages and accounting holds match
the continuation contract. Mixed-stale paths have affected native evidence,
not a separately accepted public lifecycle. This adds no answer-quality score.

The final CLI SHA256 is
`ed9e7fc421e1d6dc94ca411a184e15d2b27dfc02145dcccee85801fe851b5630`;
the 469-input source freeze SHA256 is
`0a0ca4858c079ac26132bd1ca56706c2c62211b9c40ef10196bba6b7fa2949d1`.
The public summary SHA256 is
`4b210cd4f4db52a62bec113b1e69556e2ccd750690c83943cef392f63debe2b7`.
The independent acceptance JSON SHA256 is
`22782436c32e9c6f76f7e7b3c4d80dd459b8f3e2c9d12391558425b081baf15d`.
Actual compiler actions use rustc 1.98.0, macOS ARM64, edition 2024, optimization
level 3, debug information disabled and minimum macOS 26.5. Detailed optional
evidence is under `invocation-budget-001/`; preserved private test hooks remain
disclosed and excluded from production CLI code.

This closes narrow preparation defects rather than broad retrieval quality.
The next milestone is a complete cited investigation through factual refresh and
reconstruction, with one admitted default-geometry collection attribution run
to choose the storage or bounded-discovery intervention. The allowance currently
replays enrolled journals on admission, so its work costs still need attribution.
Exact vector scans, broad completeness, unseen native HIGH and practical
25K/~2.5GB qualification remain open; 100K is deferred. No mock or local native
check establishes live-provider, power-loss or other-platform qualification.

## Local 0.2.0 candidate011 artifact

The new unsigned macOS ARM64 trial packages production commit `08dd303` and
embedded-guidance commit `5ce278a`. Its one planned release build passed in
55.512 process seconds with native rustc 1.98.0, edition 2024, optimization level 3,
debug information disabled and minimum macOS 26.5. All 346 prior core/test/build
inputs remain unchanged; the final freeze additionally records the five embedded
static skill resources. Preserved local private test hooks make the source tree
dirty and are disclosed; they are excluded from the production CLI and bundle.

The final binary passed actual version, capabilities and fresh skill-export
checks. All five static exported resources equal maintained source bytes; all six
content BLAKE3 hashes and the aggregate manifest checksum match. Generated help
covers all 53 registered commands and 20 schemas. The earlier public workflow
and native aggregate bind the precursor binary, not these different final bytes;
unchanged passing checks were retained rather than repeated.

The package verifier checked all 15 regular archive members against exact file
bytes and Unix permissions, including executable permissions. The archive is
11,135,579 bytes with SHA256
`a6002d31f2a9d49308c2354a1d0355a8ef0b9c86216f7ed70f4a903335c0f378`;
the final CLI SHA256 is
`b9759d308bb3e3514170e2ed711f42d0579d62042e2c72289c04685b2f3f9152`.
Packaging took 2.248 process seconds, allocated 42,901,504 bytes before its final
receipt and retained 60,482,732,032 free bytes. It ran no build, application command,
provider call or evaluation replay. This is a local trial, with no public release,
signing, native other-platform, live-provider, broad completeness or 25K claim.

## Local 0.2.0 candidate012 artifact

The later unsigned macOS ARM64 trial packages production commit `3cd7b48` and
the exact final CLI used in the invocation/recovery native and public controls.
It reuses that release binary without another build or evaluation replay. A Git
blob audit matches 466 frozen inputs exactly to committed source; the remaining
three entries are the preserved private test-module hooks and two probe files,
excluded from the production CLI. No clean-tree claim is made.

Actual version, offline capabilities and fresh skill export pass. All five static
resources equal maintained bytes; six BLAKE3 content hashes, aggregate manifest,
53 command-help entries and 20 schema entries match. All **16 regular archive
members** pass exact inventory, byte/hash and Unix-permission checks, including
CLI executable mode. Packaging completes in 3.374633 owning-process seconds,
with zero builds/provider calls; recorded allocation is 43,327,488 bytes and
host free space is 60,730,798,080 bytes.

The local artifact is named `lwiki-0.2.0-macos-arm64-candidate-012.tar.gz` under
`.artifacts/releases/0.2.0/`; it is 11,184,138 bytes with SHA256
`6fed7a4ef716319175cefba9eb40007ee78370724a762a7217e3cad57f84f4d0`.
The CLI SHA256 remains
`ed9e7fc421e1d6dc94ca411a184e15d2b27dfc02145dcccee85801fe851b5630`.
The packaging receipt SHA256 is
`b4d72125ef3ffc6e79e66daefec6805a10bcb5dffde9abf9f0c8a9e61d791862`.
The bundle includes its exact command reference, exported skill, build profile
and scoped provenance. It excludes fixture vaults, raw logs, private probes and
credentials. Candidate011 and every failed attempt remain preserved.

This provides a new local 0.2.0 trial, not a public release or whole-goal
acceptance. Complete cited-answer quality, unseen native HIGH, affordable storage,
fast queries and representative 25K/~2.5GB behavior remain open. Minimum macOS
26.5, live providers, native other platforms and power-loss behavior retain their
qualification limits.

## Default-geometry collection attribution checkpoint

A separate development fixture uses 64 captured files, totaling 6,251,835 bytes:
two licensed Cargo 0.85 documentation files and 62 varied synthetic operational
documents. Twelve development questions remain outside indexed content. The
shipping candidate012 CLI is unchanged; a narrowly scoped ignored test helper
adds manifest-bound application dispatch, compact inventory and observations.
It uses input-only synthetic vectors with 1,536 dimensions, default 12,000-byte
inputs and no quality target. The transport is in-process: there are no HTTP or
live-provider calls, and its vectors establish no semantic relevance.

One grouped helper compilation checkpoint passes with actual release opt3
parameters. The sandbox startup refusal and subsequent adapter type mismatch
remain recorded; cumulative checkpoint time is 138.161 seconds. No previously
passing production tests or shipping CLI build were repeated.

The initial public run completes all 231 planned commands in 135.998 owning-process
seconds. Import stops at four acknowledged items with 60 remaining, then resumes
under the same key to complete all 64. Preparation publishes 562 units; all 12
query embeddings are cached, and the cached preparation control sends no new
requests. Total synthetic dispatch is 48 requests/574 inputs with 6,362,968 request
bytes and 402,653,184 response-reservation bytes. Unknown billing holds remain
reserved. Final observed owned allocation is 89,624,576 bytes; sampled active
allocation reaches 97,333,248 bytes. Exact peaks and physical I/O are unavailable.

**The native content baseline fails.** Its mistaken `--kind source` filter excludes
captured payload rows, leaving all 108 context packets with only a 165-byte scope
header and no passages. Successful command exits establish mechanics only.
Independent source/output inspection identifies this as an evaluation-scope
mistake, without establishing a shipping defect. The original run is preserved.

An independently admitted six-command D09 control replaces that kind filter with
`--path-prefix sources/` on the same CLI, vault, query and caches. All three searches
return five cited captured hits. Lexical context returns five captured passages
in 5,980 rendered bytes; synthetic semantic/hybrid contexts return three passages
in 4,854/4,844 bytes. The control takes 1.263 owning-process seconds and makes no
provider calls. Independent inspection confirms the filter correction and these
returned-content counts, without assigning a fact-completeness, semantic-quality
or workflow score. The control exceeds its small logical-read reservation by
31,325,376 bytes of executable verification; retain the full 840,826,048-byte debit
against the remaining whole-workflow allowance. Its standalone resource quote
does not pass, and these valid filter observations do not require a replay.

The corrected all-task baseline completes 72 commands in 13.378 owning-process
seconds, with no command errors, unrun commands or provider calls. Independent
assessment finds all 12 expected source-owner positions across the ten answerable
tasks, but only **9/28 required facts and 2/10 complete positive tasks** in lexical
context. All 52 checked lexical citations match the captured and original byte
ranges, identities and quote hashes. Accurate citations and owner retrieval do
not establish sufficient evidence. Mock semantic/hybrid relevance is unscored.

The separately declared assisted trial stops after its first two tasks return
partial answers. Each uses one additional offline public command. D09 consumes
47,594 evidence bytes and still lacks three required facts; D01 consumes 45,309
bytes and lacks the profile qualification. Both stay within the 48 KiB evidence
and 6,000-byte answer limits. Model token usage, cost and inference time are
unavailable. The remaining ten tasks are explicitly unrun. This is a failed
completeness trial, not a complete twelve-task comparison or a workflow pass.

Publication, refresh, withdrawal, reconstruction and growth are not executed for
this failed trial. A fresh architecture review examines fact selection and the
bounded reading workflow before another intervention. Keep any eventual draft
outside `sources/`, count reacquisition after cache loss, and score native evidence
separately from assisted answers. The complete workflow, representative 25K
capacity and unseen HIGH remain open.

## Omission compaction and targeted reading

A fresh architecture critic identified repeated omission records as a concrete
transport cost in the failed D01 and D09 packets. Context now groups identical
record/path/reason entries in first appearance order and sums their existing
counts. It changes neither passage selection nor the citation schema. The
[targeted reading guide](indexed-context.md#reading-a-missing-condition-within-a-discovered-source)
uses ordinary searches within returned source payloads and verified range reads.

One grouped release-opt3 native checkpoint builds the CLI and unit tests in
112.800 seconds, with both affected checks passing. It reuses the earlier
unchanged correctness evidence. Exactly two public context captures compare
against the preserved baseline: D01 falls from 30,507 to 17,270 JSON bytes
(43.390%); D09 falls from 31,936 to 18,604 bytes (41.746%). Evidence, text,
passages, citations, bundles, dependencies, snapshot, warnings, truncation and
substantive usage match exactly. Only the verification timestamp and omission
representation differ. The seven omission records in each packet preserve
their distinct keys, first ordering and total counts of 151 and 152.

Two fresh assisted tasks retain the original questions and limits. D09 completes
its six requested fields and linked procedure with four additional public calls,
32,146 total evidence bytes and a 4,046-byte answer. D01 completes both requested
aspects with seven calls, 43,829 bytes and a 793-byte answer; one usage error and
its correction remain recorded. Targeted searches miss its profile statement,
which a bounded verified read supplies. Both stay within 12 extra calls, 48 KiB
of full raw evidence, 6,000 answer bytes and 120 observed UTC seconds per task.
Observed host turnaround is 66.242/113.081 seconds, including dispatch overhead;
model usage, cost and inference time are unavailable.

Independent acceptance is **9.5/10, every scoped mandatory condition passed and
zero correctness blockers**. All ten final SourceRefs match returned evidence,
captured and original ranges, identities, eligibility and quote hashes. This
accepts packet compaction and these two assisted answers. Native completeness
remains 9/28 facts and 2/10 positive tasks; the original twelve-task trial remains
failed, with its ten unrun tasks preserved. Publication, the full collection
lifecycle, semantic relevance, unseen HIGH and practical 25K remain open.

## Complete assisted draft lifecycle at 64 sources

A prospectively declared successor reuses the two accepted answers and completes
the remaining ten development tasks under the same questions and limits. The
corrected twelve-answer set passes independent review at **9/10, every mandatory
answer condition met, zero remaining correctness blockers**: all 28 requested
facts across ten positive tasks and both explicit absent-information answers.
The reviewer verifies 42 retained SourceRefs against returned, captured and
original evidence. One extra archived comparison initially cites the wrong
record; removing that comparison and its unused citation preserves the original
failure and corrects the answer without another actor or query. The ten new
actors use 32 extra public commands and 649.795 summed host UTC seconds.

The same actual draft Page then passes save, title discovery, guarded author
clarification and obsolete-hash refusal; Source refresh, immutable-history reads
and prose reconciliation; withdrawal with current restore gaps and authenticated
historical references; complete derived-cache loss, normalized rebuild, counted
reacquisition and final guarded no-op. Both author notes, Page identity and draft
status survive. The refreshed restore window becomes 37 minutes with new
revision citations. After withdrawal those values remain historical evidence,
while current deployment facts stay supported. Cache reconstruction preserves
canonical files and retained history and advances publication identity.

Independent final acceptance is **9/10, every scoped lifecycle mandatory met and
zero remaining correctness blockers**. It checks actual receipts and 29
reconciliation citation occurrences rather than relying on summary status. All
72 postwithdrawal search/context captures succeed and return no SourceRef from
the withdrawn Source. Reacquisition processes 553 corpus units and twelve query
vectors; subsequent cached sync/check use no network and start no run. The final
guarded edit is an authenticated no-op with identical full-file hash and body.

Seven operators take 113.983 owning-process seconds, including 95.033 seconds
for reconstruction, reacquisition and replay. The three reconciliation actors
use fourteen verified reads, 32,710/40,216/41,207 raw evidence bytes and
59.706/80.060/57.105 observed host UTC seconds. Cumulative synthetic transport is
97 requests, 1,142 inputs, 12,640,725 wire bytes and 813,694,976 response-reservation
bytes. Observed owned allocation peaks at 152,711,168 bytes, including retained
and rebuilt caches. These are native release-opt3 development observations;
physical I/O and model usage, cost and inference timing remain unavailable.

Earlier incomplete reviews, timing failures, the original failed answer trial
and conservative accounting holds remain preserved. Subsequent review allowances
are funded prospectively from unused reservations within the original finite
envelope; passing this successor does not retroactively pass those attempts.
The exported cited-Page recipe now describes direct context and bounded targeted
reading, with ID-only selection optional. Export/build/transfer qualification is
a separate checkpoint. Native completeness remains **9/28 facts and 2/10 positive
tasks**. Synthetic vectors qualify mechanics, not semantic relevance; native
HIGH, full layout parity and practical 25K capacity remain open.

## Local 0.2.0 candidate013 artifact

Candidate013 packages production commit `e74b042`, including omission compaction
and the revised embedded cited-Page guide. The native ARM64 CLI reports
`lwiki 0.2.0`; its actual compiler arguments retain optimization level 3 and
debug information level 0. Binary SHA-256 is
`64744ed8c8f0bf236c0dc169db4f6a7fbc79327bb015dcb931861f7c2faecaf1`.
The 11,143,316-byte archive and its checksum pass byte comparisons for all four
members, including executable permissions. Archive SHA-256 is
`b4ce37da9b9e591b7a532d0c0b2f1d240b6289dd47466898dc68112d4ba42dce`.

The copied CLI successfully exports the Codex skill. All seven exported files,
every manifest BLAKE3 digest and the package checksum pass; both changed guide
assets match repository bytes. One optimized CLI build runs after correcting
sandbox startup and cache-selection failures, both preserved before compilation.
The checkout retains unfinished test-only source edits, so source provenance
remains informational. This is a local candidate and export-integrity checkpoint;
fresh-host transfer, full release qualification, native quality, HIGH and 25K
remain separate. Candidate012 is preserved; no tag or release is published.

## Native structural evidence development comparison

A frozen native candidate retains readable HTML through lexical normalization,
exact original-byte mapping and structural passage selection. Complete bounded
definition pairs remain together, and lexical packing charges rendered cost
without rewarding surrounding padding. Parser-fingerprint migration rebuilds the
derived catalog automatically; the affected cache-only test retains immutable
sources and reuses compatible vectors without network calls. Forty-three distinct
focused checks pass. The initial migration-fixture failure and its test-only
correction remain recorded; unchanged passing checks are reused.

The candidate improves this development set from **9/28 to 15/28 supported facts**
and **2/10 to 5/10 complete positive tasks**, but **fails advancement**. The frozen
gate requires 24/28 facts, 8/10 complete tasks including D01/D09/D10, and retention
of every baseline-supported fact. D01 and D09 remain incomplete; D09 loses one
previously supported restore-volume condition. D10 becomes complete. Both absent
tasks retain bounded unsupported outcomes.

Both executables query the same 64 original sources and immutable identities,
with independently rebuilt catalogs, offline lexical discovery and identical
5-owner/80-candidate/1024-byte excerpt/6000-byte/1500-token limits. All 48 paired
search/context commands succeed. Independent inspection authenticates all 52
baseline and 85 candidate context citation occurrences, plus 51 search citation
occurrences per arm. Every expected owner is found. Exact citations and owner
hits therefore do not establish complete task evidence: a correctly cited
captured archived draft can still be irrelevant to a request for approved guidance.

The setup guard initially stopped before any quality query because it treated a
mutable writer-lock PID diagnostic as immutable evidence. The preserved, reviewed
continuation excludes that diagnostic, checks the released lock and idle authority,
and reuses the prepared fixtures. Cumulative work is 57 children and 34.826 owning
process seconds, with no provider calls or excluded questions. Every query is
below five seconds; observed maxima are 0.081538 seconds for baseline and 0.079020
for candidate. These single 64-source observations do not establish a speedup or
large-vault capacity.

Candidate CLI SHA-256 is
`5388d17864c01820a9e92cb36f8ee4120f6f0fbb4b5f01c933fddd955f28c0b8`;
the paired baseline is candidate013 above. Both use release optimization level 3.
The failed quality candidate is not promoted. Fresh architectural review identifies
loss of command subject, procedure scope and complementary evidence after owner
discovery. The next experiment compares one fixed contextual structural-passage
BM25 reference against the frozen candidate, tracing generation, candidate
reduction and final packing under unchanged limits. A persistent passage index,
another weight sweep and larger packets are not justified by this result.
Native HIGH, default-layout parity, bounded semantic candidate discovery and
practical 25K capacity remain open.
