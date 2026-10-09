# Varied 1K import and retrieval control

On 2026-10-09, the existing native macOS 0.2.0 preview imported 1,000 distinct
documents totaling exactly 100,000,000 current-content bytes into a new
normalized vault. All imported original and extracted bytes matched their
inputs. This is a measured preparation and import control for the
[large-vault protocol](testing-large-vaults.md), not 25K capacity or retrieval
quality acceptance.

## Inputs and executable

The two public documents are the complete Cargo configuration and features
references from commit `66221abdeca2002d318fde6efff516aab091df0e`; their README
and both upstream license files were retained. The other 998 documents contain
deterministic synthetic activities, distinct identifiers and routes, Unicode,
headings, tables, code blocks, and early and distant facts. Their base sizes are
199 × 80,000, 600 × 100,000 and 199 × 120,000 bytes, with small documented
adjustments so the whole public files remain intact and the total is exact.
The templates provide structured load, with limited semantic diversity. They
do not represent a natural corpus or establish semantic retrieval quality.

[large_vault_packet.py](../scripts/large_vault_packet.py) prepares these
source-only inputs without running the wiki CLI, acquiring a corpus, or adding
questions or expected answers to the indexed content. Its `plan` command writes
a proposed packet and symbolic command arguments. `generate` requires explicit
input-generation admission and refuses counts above 1,000; `verify` checks exact
membership, deterministic hashes, UTF-8, sizes and notices. Partial outputs are
preserved rather than overwritten. Planning a 10K or 25K tier does not admit it.

This historical experiment deliberately pins executable SHA256
`398229c96b305cc166f84cd80fb379719fbfbbfd1036bb4acafddfb114404679`, from production
commit `a06e296b6043c2f1c04b8c125aee4eb3ee48d533`. It used native arm64 release
settings, optimization level 3 and debug information level 0. The local preview
is not included in a fresh checkout; the script's pin is an experiment constraint,
not a guarantee that a newly built executable has identical bytes. No Rust
rebuild was needed for this control.

## Observed import

The public commands initialized the absent vault, activated the normalized
catalog, prepared an import manifest, and imported groups of four using a stable
key and a maximum of 64 groups per invocation. One initial run and three resumes
completed the 250 groups. The completion journal covered every input ordinal
once and returned 1,000 distinct Source and Revision identities.

| Observation | Result |
|---|---:|
| Input generation and verification, before final output emission | 3.388 seconds |
| Active import commands, including final status | 318.552 seconds |
| Whole import supervisor interval, including audits | 322.171 seconds |
| Largest native CLI peak RSS | 38,731,776 bytes |
| Native supervisor peak RSS | 29,065,216 bytes |
| Allocated runtime account after import | 1,016,643,584 bytes |
| Free space after import | 124,610,670,592 bytes |
| Original/extracted sources matching input bytes | 1,000 / 1,000 |

These are owning-process monotonic durations on this host, with warm filesystem
metadata. RSS includes native high-water measurements and sampled process-tree
observations. The sampled resource guards can overshoot, and their inventory
and process-monitor overhead is included in command measurements. The runtime
account includes inputs, canonical files, retained changes, indexes and logs;
its allocation is not a forecast for 10K or 25K. The resource envelope was
3.5 GiB runtime plus 512 MiB external reserve and at least 32 GiB free space.

A subsequent read-only binding check verified every Source current head and
Revision owner and matched all 1,000 current captured index rows to the import
mapping at publication epoch 251. This verifies the frozen imported membership;
it does not establish global filesystem freshness. An attempted `index status`
command returned `USAGE` because that command does not exist; the failed attempt
was retained, and publication identity was read from the quiescent catalog.

## Retrieval control and first loss

A frozen, offline pilot executed 20 tasks in three interleaved repetitions at
publication epoch 251. Every command used the same 1,000 immutable Source
revisions. Search retained at most 80 candidates and five hits with 1,024-byte
previews; context used a 6,000-byte/1,500-token packet. The companion read arm
read only the exact preview spans of the first two returned captured-source
hits. No query rewriting, expected-source lookup or model calls were allowed.

| Observation | Result |
|---|---:|
| Actual search / context / exact-read calls | 60 / 60 / 87 |
| Command or independent citation-audit errors | 0 |
| Complete positive tasks, context | 17 / 18 |
| Complete positive tasks, search plus fixed reads | 15 / 18 |
| Correct scoped absent-information tasks | 2 / 2 |
| Independently checked citation occurrences / distinct citations | 450 / 69 |
| Warm repetitions 2–3 p95 search / context / read | 97.840 / 79.295 / 30.938 ms |
| Whole supervised interval, including source-pin audits | 134.096 seconds |
| Largest native CLI peak RSS | 47,267,840 bytes |

All task outcomes were stable across the three repetitions. Source heads,
revision ownership, quote hashes, selected-document proof scope, filters and
final frozen input pins passed independent review. These are warm measurements
on a nonexclusive host, not an OS-cold or 25K performance qualification.

The scoped critic score was 9.5/10, but acceptance **failed** the mandatory
all-positive-task gate. One synthetic distant-dispatch task found the wrong
station; two Cargo search previews omitted required setting conditions even
though context contained them. A separately frozen three-call development
diagnostic paged the original 80 candidates and found the required station at
rank 68, with both needed facts already in its preview. Literal phrase lookup
returned it first. This isolates that failure to owner ranking before the
five-hit boundary; the Cargo failures occur at preview selection.

Pilot questions used to guide a fix are development data. The three public Cargo
cases do not establish natural-corpus or unseen acceptance, and the synthetic
templates have limited semantic diversity.

## Paired ranking and preview experiment

A candidate added a generated, escaped whole-query phrase leg on the existing
positional FTS index, retained broad-term discovery, and reused the structural
passage proposal pool for ordinary search previews. Exact identity/title tiers,
filters, the 80-owner cap and selected citation checks remained in place. The
candidate also invalidated old lexical/hybrid cursors because ranking changed.
It introduced no storage service, index migration or model calls.

The source and native release executable were frozen before opening twenty new
independently authored task strings. Both binaries then ran all twenty development
tasks and twenty new tasks in three interleaved repetitions on the same unchanged
Source revisions and publication. The new tasks included paraphrases, distractors,
separated and multiple-source facts, identity, filters and absent information.
Their public Cargo family and synthetic templates limit generalization.

| Observation | Baseline | Candidate |
|---|---:|---:|
| Complete positive tasks, raw context | 30 / 34 | 32 / 34 |
| Complete positive tasks, fixed first-two reads | 25 / 34 | 29 / 34 |
| Complete positive tasks in both arms | 25 / 34 | 29 / 34 |
| Required owner occurrences in top five | 40 / 42 | 42 / 42 |
| Correct scoped absence/filter tasks | 6 / 6 | 6 / 6 |
| Warm search p95 | 70.062 ms | 78.409 ms |
| Warm context p95 | 87.111 ms | 81.373 ms |
| Warm read p95 | 25.517 ms | 22.522 ms |

Every task outcome was stable across repetitions, with zero observed required-fact
regressions. The 822 actual commands completed without errors, and independent
review authenticated all 1,740 captured citation occurrences, 212 distinct
SourceRefs across 53 Sources. Final source, executable and protocol pins matched.
The whole supervised interval was 543.612 seconds; the largest native CLI RSS
was 49,741,824 bytes. Resources passed their declared limits. These are warm,
nonexclusive 1K observations; repeated decoded bytes are not physical disk growth.

The previously missed dispatch owner moved to rank one with complete evidence.
Three new tasks also gained complete evidence. However, **acceptance failed**:
the candidate completed the old context/read gates at 18/18 and 16/18, and the new
gates at 14/16 and 13/16. Five mandatory workflows remained incomplete. Two old
Cargo reads returned only a 127-byte settings metadata block; other failures lost
documented defaults, feature-combination behavior or an override condition after
finding the right owners. Context and reads were assessed separately.

The independent score was 9.5/10, but it cannot override the prospectively required
all-task gate. **The candidate remains experimental and is not promoted.** A pool
of structural passage proposals does not guarantee its first item is a complete
explanation. This result measures a gain in this paired slice; historical default
quality remains failed at 6.5/10, with natural/default, HIGH, semantic, 25K and full
release gates open. Any new question used to guide the next fix becomes development
data and needs replacement for another unseen claim.

The experimental source is preserved at commit
`6d160b7c9d7c22e999fc3545f98fa390ea300d52`, on
`impl/retrieval-workflow-20261009-001`. Its frozen executable SHA256 is
`5d60c722f6ca33a39767703af13a96d791bf4c91553f19fa01434443c9f4b91a`.
The checkpoint is not an accepted release or a change to production defaults.

## Source refresh and cited Page control

The accepted preview subsequently completed a separately frozen 24-command
workflow in this same 1K vault. It staged a novel 100,000-byte Source revision,
reviewed every proposed payload before applying it, retrieved the new fact through
search, context and exact read, then reconciled a cited draft Page using its
actual author hash. Independent review scored this declared subset **10/10**,
with all six mandatory outcomes passing and no correctness blocker.

All 3,000 pre-existing immutable files and 999 non-target Source headers remained
byte-identical. Source identity and origin remained stable. The old exact
quotation retained its Source, Revision, span and hashes while becoming historical.
The Page kept its identity, draft status and every author byte outside the
generated citation block, including its note. Its final citations and navigation
pointed to the old and new revisions. Twelve inspected citation occurrences
authenticated against the immutable bytes.

| Observation | Seconds |
|---|---:|
| Whole workflow, including inventories and audits | 21.786 |
| Source refresh stage / apply | 0.207 / 0.335 |
| Identical-input Source refresh | 0.019 |
| Explicit unchanged index sync | 2.029 |

Publications advanced from 251 to 254 through Page creation, Source refresh and
Page reconciliation. Repeated refresh reused the current Revision without a new
Change, and index sync reused the complete final publication. The index no-op
still inspected 4,007 files and reported 403,232,838 input-I/O bytes; this warmed
observation does not establish constant work or larger-vault throughput. Context
reported its budget omissions while returning the requested fact completely.

This passes one Source/history/Page workflow, using two revisions of one Source.
It does not exercise two independent Sources, stale-guard refusal, withdrawal,
reactivation, interruption recovery, backup or larger-tier acceptance, and does
not change the retrieval candidate's failed completeness gate.

## Validation limits

Ten affected preparation controls pass across the initial checkpoint and the
replay of two repaired tests. The initial checkpoint retained one test-fixture
error; the unchanged passes were reused. Four import-supervisor tests cover
completion, journal coverage, final pin drift and deadline failures with a fake
Runner. These checks do not demonstrate live-provider compatibility or recovery.

The candidate's focused native release checks retain 200 passing retrieval tests
from the initial checkpoint and four passes after grouped test-fixture repairs,
with six ignored tests. The repairs changed mock schemas and the explicit eligible
Page filter, preserving malformed-hit refusal and authority checks. This is
204 distinct passes across two checkpoints, not a full green suite. Actual paired
content review, rather than these component checks, establishes the failed quality
gate above. No rebuild followed the unchanged passing replay.

Full retrieval acceptance, additional management workflows,
interruption recovery and larger tiers require separate actual-command acceptance. The
previous failed 1,000-change/600-second gate remains mandatory for full capacity.
Default retrieval quality, semantic acceptance and full release qualification
remain open.
