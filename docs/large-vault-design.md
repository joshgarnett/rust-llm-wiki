# Large-vault architecture and validation

Status: architecture plan with an initial [indexed-context implementation](indexed-context.md), not a supported-capacity claim. The initial target is
100,000 distinct current documents containing approximately 10 GB of UTF-8 text.
Count original copies, retained revisions, operational records, retrieval units,
vectors and index files separately. A 10,000-document / 1 GB tier supplies the
scaling control. Million-document capacity is outside this initial gate.

The existing [storage](technical/storage.md) and
[retrieval](technical/retrieval.md) contracts remain authoritative until a tested
replacement explicitly changes them. This work must preserve canonical Markdown,
immutable source revisions, rebuildable local indexes, exact citations, offline
operation, bounded remote embedding calls and recovery/accounting invariants.

## Why a redesign is necessary

A public 299-source corpus exposed an operational failure before answer quality
could be evaluated: all 64 prospectively selected questions exhausted the maximum
65,536-entry context-verification budget. Repeated nested-vault checks enumerated
an entire sibling directory for each file being checked. Removing that repeated
work addresses one defect, but cannot establish large-vault support.

Several other operations still depend on the entire corpus:

- Opening a strict catalog reader checks the database, deserializes a complete projection and
  reconstructs another catalog and both search tables in memory to compare rows.
- Current-context verification captures canonical files and revision assets
  twice. For 10 GB of captured text, original/content duplication alone implies
  approximately 40 GB of payload reads per query, before metadata work.
- Publication rewrites a complete generation and search tables. Repeating source
  additions compounds whole-vault validation and history scans.
- Semantic retrieval renders the corpus and scans vectors; embedding scheduling
  and prior-accounting discovery also materialize or enumerate global state.

Increasing proof limits or bounding the number of returned passages does not
bound those operations. Ten GB of current text also needs substantially more than
10 GB of disk: original/content copies, retained history, indexes, journals and
temporary rebuild space all contribute.

The whole-catalog JSON value is also a hard storage obstacle, independently of
memory. SQLite defaults to a 1,000,000,000-byte string/BLOB limit and supports at
most 2,147,483,645 bytes for one value; its length limit also applies to a complete
row. A projection containing 10 GB of source text cannot be stored in that single
value. Increasing query budgets cannot repair this representation; publication
must use independently addressable rows.
[SQLite implementation limits](https://www.sqlite.org/limits.html)

## Chosen direction

Use independently addressable catalog records, source manifests, reverse
dependencies and retrieval units. Query only required rows and bounded candidate
sets. Replace full read-time reconstruction with selected-row checks plus an
explicit full integrity audit. Selected-row checks cannot rule out cache omissions.

Publish changed rows, affected dependency closures and search-index changes in a
single SQLite transaction. Reader transactions must retain a coherent published
generation. Reconciliation and rebuild must stream bounded batches, with durable
checkpoints and interrupted-publication recovery. Batch source capture must avoid
one full publication per document while preserving individual immutable revisions.

Keep SQLite first. FTS5 already supports row updates, internal segments and
incremental merging; the application must maintain row/index consistency.
Measure delta publication before adopting another lexical engine.
[SQLite FTS5](https://www.sqlite.org/fts5.html)

Separate graph validation from retrieval-text emission before replacing storage.
Measure control-record, graph-snapshot and dependency memory at the 1k and 10k
tiers; raw envelope size alone cannot establish whether the graph fits in memory.
A streamed document sink should serve an actual rebuild path. Move graph state
to a disk workspace when the measured resource requirements demand it.

Avoid persisting raw and normalized text again inside row JSON, whole-catalog JSON
and FTS content tables. External-content FTS can index the stored text, but the
application must keep postings and content consistent, including during migration
and deletion. Its integrity check must explicitly compare external content.
[SQLite external-content tables](https://www.sqlite.org/fts5.html#external_content_tables),
[FTS integrity checks](https://www.sqlite.org/fts5.html#the_integrity_check_command)

The rebuild publication topology still requires a separate contract and tests.
Building a sibling catalog in bounded transactions and then publishing a durable
selector can control rebuild WAL growth, but introduces selector recovery and
reader-retirement obligations. That filesystem commit point differs from the
SQL transaction used for an in-place delta. Do not combine their atomicity claims
or replace an open database file. Measure old/new catalog and WAL peaks before
qualifying the full tier.

Persist retrieval units and eliminate per-unit lookup overhead before selecting
an approximate vector index. Compare any embedded ANN candidate index with exact
search on frozen vectors, including restrictive filters, deletion and refresh
churn. Exact reranking cannot recover neighbors that candidate generation omitted.
No new database service, local model or ANN dependency is selected by this plan.

## Freshness must be explicit

Retain the existing strict verification behavior. Global identity uniqueness,
current eligibility and arbitrary external edits require full reconciliation;
strict requests must fail without verified output when their budget is exhausted.

A separate fast workflow may search published generation G and recheck selected
source bytes, head records and dependencies. Its output must identify that
generation, the scope of the byte checks and the absence of a global-currentness
or discovery-completeness claim. It must not reuse `verified_snapshot` for this
weaker guarantee. It must reject changed selected dependencies rather than merely
attach a warning to invalid evidence.

Timestamp caches cannot prove arbitrary-edit freshness: same-size writes can
retain indistinguishable timestamps. Watcher barriers can help establish which
events were observed, but lost continuity requires reconciliation. An optional
watcher is an optimization with explicit platform assumptions, not a replacement
for the strict contract. [Git racy index](https://git-scm.com/docs/racy-git),
[Watchman clocks](https://facebook.github.io/watchman/docs/cmd/clock),
[Watchman recrawl recovery](https://facebook.github.io/watchman/docs/troubleshooting)

## Implementation and acceptance order

1. Repair repeated directory traversal and rerun the fixed public questions under
   unchanged budgets. Preserve the failed baseline and all new failures.
2. Introduce bounded catalog reads and the explicit freshness distinction, with
   selected/unselected corruption and external-edit tests. Preserve strict mode.
3. Replace full projection publication with normalized incremental storage,
   streaming rebuild and recoverable batch ingestion. Test readers across commits
   and every publication/restart boundary.
4. Bound embedding scheduling and accounting discovery, preserving unknown holds.
   Evaluate vector-index changes only against a frozen exact baseline.
5. Run the declared scale workloads, then freeze the candidate for independent
   unseen answer-completeness evaluation.

The [scale acceptance protocol](testing-large-vaults.md) freezes prospective
workloads and thresholds separately from the semantic gate. Freeze hardware,
fixtures and work limits before measurement. Record
query latency, peak memory, ingestion/rebuild throughput, bytes read/written,
directory work, storage amplification and failures. Include no-op sync, one-file
and batch updates, deletes, duplicate IDs, same-timestamp edits and interrupted
publication. Synthetic fixtures test operations; public relevance labels and
independently graded evidence test retrieval quality separately.

Neither a fast synthetic benchmark nor a correct citation establishes answer
completeness. The existing [quality gate](rag-quality-targets.md) remains separate;
large-vault support requires an independent critic to assess the complete workflow
at the declared tier, not just isolated query timing.
