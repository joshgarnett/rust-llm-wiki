# Import throughput development control

Import mechanics passed independent scoped assessment at **9.3/10**, with no
observed correctness blocker. **Throughput advancement failed.** The eight-arm
control does not qualify 25k capacity or change importer defaults.

The native macOS ARM64 0.2.0 candidate used release optimization level 3;
executable SHA256
`19350962d3342bb6372397ffe986b6c772635523956e0154566006a1f6c6b699`.
No build, provider call or production vault was involved during measurement.

## Controlled comparison

The existing public importer prepared and ran 64 deterministic inputs per arm.
Four- and eight-item groups were compared for 1 KiB and 100 KiB documents on
isolated copies of small and occupied normalized seeds. The occupied seed came
from the [1k Sources / 1k Pages checkpoint](validation-capacity-workflow.md).
Paired cells used identical absolute input lists and manifests; execution order
was fixed and counterbalanced. There were 512 fresh captures and 44 successful
public commands across eight arms.

Rates include preparation and import run time:

| Seed | Input size | Four-item group, items/s | Eight-item group, items/s |
| --- | ---: | ---: | ---: |
| Small | 1 KiB | 3.104 | 3.438 |
| Small | 100 KiB | 3.059 | 3.303 |
| Occupied | 1 KiB | 0.901 | 1.128 |
| Occupied | 100 KiB | 0.907 | 1.108 |

The prospective advancement conditions required the occupied 100 KiB eight-item
arm to achieve at least 2.0 items/s and at least 1.25 times the four-item rate.
Its actual rate was **1.108 items/s**, with a **1.222 times** ratio. Both failed.
The 1.736 items/s arithmetic needed for 25k imports within four hours is not a
completed capacity observation or a replacement threshold.

## Correctness and limits

All 512 distinct Source/Revision pairs and 96 group Changes passed ownership
checks. All 1,024 original/extracted payloads matched their inputs. Immediate
search, context and read succeeded; the critic independently verified eight
search citations, twelve context citations and eight selected reads. Protected
existing arm, original seed, owned seed and backup bytes and mtimes were retained.

The owner completed in 419.607 seconds, including copies, hashing and supervision,
within a 24-minute aggregate limit. Each command had a 180-second ceiling;
allocation stayed within a 100 GiB account and 32 GiB free-space floor. Native
import main-process peak RSS was approximately 20–51 MiB. Process-tree sampling
does not establish an exact peak. Filesystem barrier counts, SQLite VM work and
complete immutable-read volume are unavailable.

An earlier account failed during setup because the sandbox denied its process
sampler. Its initial command was killed and all eight arms remained unrun. The
original 11,794 files were independently unchanged. A separately approved fresh
account retained identical executable, protocol and driver pins after correcting
host sampling access. The failed account remains preserved: there were **45
total CLI attempts**, including that setup failure.

These are single observations. Copy warming, OS caches and seed migration history
limit causal claims. Increasing input size 100 times barely affected occupied
run time, while occupied-seed CPU work grew substantially. This does not establish
that catalog occupancy alone caused the slowdown.

## Next measured decision

Source inspection found repeated activation validation through importer path
checks and retained Change resolution. Each validation reads, hashes and
strict-decodes the original migration Epoch. The occupied seed's plan is 647,457
bytes versus 1,691 bytes for the small seed; migration-history complexity is
confounded with occupancy. At that checkpoint this remained a hypothesis; the subsequent phase observation below attributes the repeated authentication cost.

Use bounded observational profiling before a storage change. If repeated
activation explains the excess, assess root-bound operation-scoped validation
reuse with final dependency rechecks. Preserve tamper, missing-plan, wrong-root,
concurrent-change and recovery refusals. Do not add a persistent cache or rewrite
storage from these rates alone.

## Bounded phase observation

A subsequent observation-only optimized build preserved all authentication
checks and added aggregate timers to the import command. The instrumented
executable is separate from the user candidate, SHA256
`249dced3f2389994de4a40273098179b062c81b1b89cd0a71aba58bd516b94ef`.
Four accounting checks passed in one 101.332-second build/test checkpoint. Its
second test filter omitted a module; only the five previously unrun layout checks
were then executed from the pinned test binary and passed in 1.169 seconds.
There was no rebuild or repetition of passing checks.

Two protected seed copies each imported sixteen identical 1 KiB inputs in two
eight-item groups, followed immediately by verified search, context and read.
The ten-call owner completed in 33.439 seconds under its five-minute limit.
Independent actual-outcome assessment passed mechanics at **9.4/10** and the
unchanged attribution gate. It verified all 32 Source/Revision pairs, four group
Changes, 64 payloads, four citations, two selected reads and protected seed/arm
bytes and mtimes. No product command or build was rerun during that assessment.

| Internal command observation | Small seed | Occupied seed |
| --- | ---: | ---: |
| Command elapsed | 4.783 s | 14.554 s |
| Migration authentication residence | 0.252 s | 8.285 s |
| Composite strict decode | 0.035 s | 7.318 s |
| Authentication attempts | 1,600 | 1,600 |
| Successfully read plan bytes | 2,705,600 | 1,035,931,200 |

Authentication explains **82.2% of the occupied-minus-small command difference**
and **56.9% of occupied command time**, crossing the prospectively declared 60%
and 30% attribution thresholds. Decode includes strict parsing, envelope
serialization/checksum and Epoch conversion. Child timers are disjoint;
authentication remainder and command unassigned time are reported separately.
These are elapsed residence measurements, including instrumentation and cache
effects. Plan bytes count logical reads, not device I/O. Profiling overhead is
unavailable separately; this is not a shipping speedup or capacity result.

The narrower proposed optimization reuses successful Epoch semantic validation
within one root-bound import invocation while **still reading and hashing the
full plan on every check**. It would skip repeated decoding only for the same
authenticated activation identity/version/hash. Missing, changed or invalid
plans must retain existing refusals; errors cannot populate the memo. The subsequent implementation and paired control are recorded below; the phase observation alone did not qualify a speedup.

Detailed pinned drivers and raw evidence remain local. This summary records a
historical development control; a fresh checkout can exercise the public
[import workflow](source-imports.md), but does not contain those frozen seeds.


## Per-import validation reuse and paired control

The importer now reuses successful migration Epoch semantic validation within
one root-bound library `run` or `resume` invocation. Every activation still
reads and hashes the full plan and freshly checks the layout and vault marker.
The memo holds one exact activation/version/hash predicate, never a filesystem
observation. Errors invalidate it; custom/budgeted readers remain uncached.
Nested and disabled scopes suspend reuse, and thread-bound RAII teardown clears
it on returns and unwinds. No persistent cache, dependency or public flag was added.

One native macOS ARM64 release checkpoint passed **37 tests**, with no failures
and two explicit experiments ignored. It covered eight memo invariants, five
layout bindings and twenty-four enabled ordinary importer/lifecycle/recovery
checks. Build/test elapsed was 213.309 seconds; tests took 113.15 seconds.
The emitted Rust 2024 executable uses optimization level 3; all 384 compiled
source/assets/config commitments matched. No observer remains in this binary.
The broad suite and latest strict Clippy are separate, unrun gates.

The first paired account stopped after four successful baseline commands because
its harness incorrectly required nonpartial context for a 100 KiB input under
1 KiB excerpts. Independent review confirmed valid Current citations and honest
excerpt-bound omissions. Sixteen captures and all original bytes were retained;
read and all candidate/comparator arms were unrun. That account remains failed,
with no comparative speed result. Product code and budgets were unchanged.

A separately reviewed fresh account corrected command-specific truncation
validation and ran all four arms in the frozen small-baseline, small-candidate,
occupied-candidate, occupied-baseline order. Each used sixteen identical 100 KiB
UTF-8 inputs and two eight-item groups, then verified search, context and read.
The four arms shared one absolute input list and identical manifest hash; new
account paths differ from the failed attempt while input bytes remain identical.

| Seed | Baseline items/s | Candidate items/s | Candidate / baseline |
| --- | ---: | ---: | ---: |
| Small | 2.877 | 2.706 | 0.940 |
| Occupied | 1.004 | 2.208 | 2.200 |

Rates are sixteen divided by unrounded prepare-plus-run launch-to-reap intervals
from the owning process's monotonic clock. The occupied candidate crossed both
prospective thresholds, 2.0 items/s and 1.50 times baseline; the small ratio
crossed its 0.90 floor. These are single observations: the small rate was about
6% lower, and OS caches, copy warming and seed migration history limit inference.
They establish neither a latency distribution nor 25k capacity.

The twenty-call fresh owner finished in 74.417 seconds within its ten-minute
limit, with all 64 captures and preservation checks passing. Including the
failed account there were twenty-four product attempts. Independent actual
mechanics acceptance passed at **9.4/10 with no blockers**, with all three frozen
performance conditions passing. The audit checked all 64 Source/Revision pairs,
128 payloads, eight group Changes, eight empty authenticated identity hits, four
search citations, eight context citations and four verified reads. Existing seed
and arm bytes, mtimes and memberships, source pins and native artifacts matched.
All four contexts returned identical passage spans/text and honestly reported
the same six excerpt-bound omissions. Main-process observed peak RSS was about
47.6 MiB; sampled process-tree observations do not establish an exact peak. The earlier group-four/eight gate remains failed.

The next workflow milestone is complete cited answers, followed by integrated
refresh, withdrawal/history and cache-loss behavior, and explicit 25k qualification.


## Import reader lifetime checkpoint

The importer now releases its projection reader once the owned preparation is
complete, before opening the publication session. The session still independently
checks its base, selected dependencies and authority and keeps its own reader
through publication. External readers retain their original snapshot. No journal
policy, forced checkpoint, schema or public flag changed.

One native macOS ARM64 optimized release build passed in 104.621 seconds; actual
CLI/library compiler parameters use Rust 2024 and optimization level 3. The affected
import/replay group passed 25 tests, including run/resume/completed replay with an
external reader across both layouts. Three experiments were ignored in that group.
The capture projection/recovery group passed nine tests. An initial named capture
filter matched zero tests and establishes no coverage; the corrected group was run
without rebuilding or repeating the passing import group.

The separately frozen, single WAL mechanism control completed 128 captures in
two small-seed cells, with eight groups of eight per cell. Natural imports had all
observed frames backfilled after every group; the deliberate external-reader cell
retained an outstanding frame gap. However, neither cell changed its reset salts:
valid frames grew from 489 to 4,532, and allocated WAL grew to 18,878,464 bytes in
both. **The declared reset/reuse hypothesis was not established.** A fully backfilled
log does not imply that its allocation shrank or that subsequent writes reused it.
The final writable probe closed after measurement and checkpointed the held-reader
cell; that intervention is separate from natural import behavior.

Independent review passed mechanism correctness: all 128 Source/Revision pairs,
256 original/content payloads, frozen inputs and both returned reads matched.
This separate correctness pass does not change the failed reset/reuse hypothesis.

The control took 42.666 seconds including setup and observation, allocated
161,341,440 bytes and observed approximately 68.3 MiB sampled child RSS. Exact
process-tree peak is unavailable. All work was offline, with no provider calls or
retry. These padded inputs and unpaired observations establish neither a throughput
improvement nor occupied-vault capacity. The lifetime correction is retained with
these limited claims; further importer microbenchmarks and journal-policy changes
are deferred in favor of the integrated user workflow. Full HIGH and 25k gates
remain open.
