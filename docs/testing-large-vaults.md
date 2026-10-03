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
