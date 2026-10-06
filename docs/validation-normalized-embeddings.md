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

Preparation currently retains at most 4,096 owners. Exact semantic discovery
renders the eligible corpus twice under a shared 64 MiB/65,536-unit allowance.
Those limits alone prevent qualification of 25,000 documents / roughly 2.5 GB;
they are source constraints, not measured throughput. Whole-preparation work
must not be described as proportional only to changed owners.

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
