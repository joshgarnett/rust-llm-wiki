# Normalized literal workflow validation

Normalized exact literal search now supports the ordinary cited workflow:
search for punctuation or a symbol, verify selected results, read the returned
original byte range, assemble literal context, update Sources and Pages, and
recover after losing the whole disposable cache. Automatic omitted-scope literal
context uses indexed documents on a normalized vault. Lexical defaults and
unsupported explicit scopes remain as documented in [indexed context](indexed-context.md).

The implementation binds the literal query as SQLite data and uses Boolean
`instr` over filtered raw text. It retains exact case, Unicode spelling, quotes,
front matter and punctuation. Rust supplies original UTF-8 byte offsets. Exact
matching-document counts and candidate truncation share a pinned snapshot.
Selected verification reuses canonical authentication and final rechecks.
A single elapsed query clock survives SQLite and Rust work; finite query, proof
and output budgets remain distinct. COUNT and selection can scan filtered text
twice: limited decoding does not establish limited corpus scanning.

## Correctness checkpoint

On 2026-10-06, the native macOS ARM64 release checkpoint passed **276 affected
tests** across retrieval, catalog-query, CLI admission and offline public
workflows. Six explicitly ignored diagnostics were not run. One compiler error
and three new fixture failures remain recorded. Their grouped corrections
replayed only failed or previously unreached cases; unchanged passing groups
were not repeated. Production code did not change during those fixture fixes.

The public fixture runs from outside a vault whose path contains spaces, in both
retained schema-1 storage and migrated schema-2 storage. It captures actual
Sources, checks symbol/Unicode excerpts through exact cited reads, builds context,
preserves bytes and mtimes during dry runs, edits a Page with an author hash,
rejects a stale hash, refreshes and withdraws a Source, preserves historical reads
and rebuilds after moving the whole cache aside. It also requires an immediate
complete zero-error check after cleanup, without a manual sync.

Paired fixtures check legacy/normalized matching, filters before candidate caps,
exact counts, cursors, case/Unicode/front-matter offset behavior and bounded
decoding. Selected-proof cases cover same-size/same-mtime tampering, wrong
owner/revision/spans, final selected Source refresh, unrelated edits and budgets.
A Source filter legitimately includes associated Evidence as well as captured
content. An unrelated new publication alone does not invalidate a held selected
reader; the race test changes the selected Source's current head.

Cleanup now publishes normalized canonical changes while retaining the same
writer, then refreshes bounded storage inventory. Failure diagnostics distinguish
committed cleanup, failed publication and inventory failure after successful
publication. The earlier [cleanup experiment](validation-capacity-workflow.md#separate-storage-cleanup-experiment)
still failed its 10% storage-benefit gate, and larger cleaned-vault sync latency
remains unqualified.

## Frozen performance and default controls

The pinned CLI SHA-256 is
`93074e694692fd731f708b5d257298fce5d4fd70137963a234dd1225ff9d9873`.
Actual compiler parameters confirm optimization level 3, debug information 0,
Rust edition 2024 and native `aarch64-apple-darwin`; no provider was used.

A complete disposable copy of the accepted 256-Source lifecycle vault ran twelve
prospectively frozen commands: a symbol-bearing query, a common substring, a
1,024-byte absent string and a Source/path-restricted query, each through ordinary
discovery, selected verification and omitted-scope context. All twelve passed
within their five-second gates, taking **0.017–0.106 seconds each**. Presence was
checked in the same returned excerpt or passage as the expected Source owner,
not in the query header. The original vault's pinned inventory stayed unchanged.

Eight separate frozen development controls compared the previous accepted CLI
with the candidate on D01 and D09 lexical search/context. All passed; returned
evidence packets matched after excluding declared observation metadata.
These controls establish preservation of those outputs, not answer quality.

Timings use the owning process's monotonic clock and include launch, emission,
capture and receipts as declared by each protocol. Logical inspection and
exposed decoder/proof work are accounted separately; native work, RSS and
physical I/O were unavailable. Samples do not establish instantaneous peaks.
Preparation timing failures and unnecessary executable hashing remain recorded
separately from successful actual command intervals.

## Acceptance limits

Independent feature correctness passes at **9.3/10**, all eight mandatory
workflow groups and zero observed product correctness blockers. The critic
authenticated 49 SourceRef occurrences (22 distinct references across 11 owners),
inspected actual returned evidence and ran twelve additional offline commands
covering human presentation, schema migration, exact reads and dry-run preservation.

The reviewer exceeded the separate ninety-second whole command envelope:
first-to-last UTC observations span about 121 seconds, despite summed active
command intervals below two seconds. The claimed 240-second review duration
measures from the reviewer's first observation; dispatch-to-final timing cannot
be certified from the available root record. These procedural limits remain
failed or unqualified. This is scoped correctness acceptance, not an unqualified
all-review-gates pass or full release qualification.

This capability earns **no improvement in default retrieval completeness**.
The native development baseline remains 9/28 facts and 2/10 complete positive
tasks; the failed structural and contextual candidates remain unpromoted.
Paraphrase relevance, complete multi-source answers, semantic default activation,
full HIGH acceptance and 25K capacity remain open. The next priority is ordinary
context retaining complete, correctly scoped evidence under unchanged budgets.
