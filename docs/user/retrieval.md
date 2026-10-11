# Retrieval reference

## Search and inspect cited context

~~~sh
lwiki --wiki "$WIKI" --offline search 'atlas_migrate' --mode literal --verify-selected
lwiki --wiki "$WIKI" --offline context 'Atlas deployment' --max-bytes 6000 --max-tokens 1500
~~~

Literal search matches case-sensitive UTF-8 substrings, including punctuation,
with no wildcards, regex or FTS operators. Lexical search ranks word matches.
Semantic/hybrid modes require compatible prepared vectors and a query embedding;
[provider setup](sources-and-providers.md#optional-remote-providers) is separate.
Offline never creates missing embeddings. Narrow broad scans with `--source-id`
or `--path-prefix`; candidate limits bound results, not all scanned text.

On legacy indexes, ordinary context defaults to synchronized, whole-wiki `current`
verification. On an explicitly normalized catalog, search uses published discovery
and context defaults to `indexed-documents`, authenticating selected dependencies.
It does not prove global membership, identity uniqueness, unselected freshness or
answer completeness. Run `index sync` to discover external edits; `check` is an
explicit complete audit. Do not rebuild merely to change layout for a recipe.
Explicit strict `--scope current` is unavailable on normalized indexes.

Plain normalized search is uncited discovery. `--verify-selected` authenticates
displayed dependencies and adds citations to nonempty captured excerpts. It
preserves ordering, filters and cursors and can accompany `--no-sync`. Selected
changes or proof exhaustion refuse the complete verified page. `read --no-sync`
and `context --scope snapshot` are cached, uncited routes.

Captured text has Source/Revision/span/hash citations. Authored passages are
verified `note_text` with record/path/hash locators, without invented source
citations. Draft Pages are discoverable with `--kind page --status draft` but
excluded from default current context. A current revision can contain draft or
unapproved statements: byte integrity does not establish truth or approval.

## Scope, filters and bounds

`--source-id` may repeat; `--path-prefix` selects one namespace prefix. Kind,
tag and status filters apply to indexed documents. `--kind source` or `revision`
selects metadata records, not captured payloads. Under a controlled layout,
`--path-prefix sources/` includes captured text and metadata; inspect returned
`captured_source` locators before making source-backed claims.

~~~sh
lwiki --wiki "$WIKI" --offline context 'What changed in the policy?' --scope indexed-documents --mode lexical --include-historical
lwiki --wiki "$WIKI" --offline context 'release checklist' --scope indexed-evidence
~~~

Historical inclusion supports normalized literal/lexical document context and
keeps Current/Historical/Withdrawn labels and exact revision IDs. It does not
support historical semantic/hybrid or card selection; strict historical scope
has a separate contract. More revisions compete for the same evidence budget.

`indexed-evidence` is a narrower offline lexical route for already indexed
captured Sources. It verifies selected Source heads, ownership and complete
original/content hashes without syncing or rebuilding. It excludes authored
Pages, graph, literal/semantic/hybrid modes, kind/tag/status filters and host
selection. A missing/obsolete cache fails with an explicit rebuild hint. SQLite
readers can create WAL/shared-memory coordination files despite read-only queries;
canonical files and operational journals stay unchanged.

Rendered context defaults to 12,000 bytes/3,000 estimated tokens. Examples use
6,000/1,500. Raise both bounds if intended; the first exhausted bound wins, with
reservations included. Byte/4 estimates are not actual model tokens. Full JSON
contains additional metadata and repeated passages; after checking success use
`data.text`, or plain context stdout, for budgeted model evidence. Inspect full
warnings/omissions separately. Output limits do not raise the proof allowance.
Selected proof caps are 64 MiB, 4,096 files, 16,384 entries and two seconds; catalog
row/VM/time bounds also apply. Cooperative checks are not hard process-memory or
wall-clock guarantees. See [the contract](../developer/contracts/retrieval.md).

## Read an exact captured range

Use the payload path and span returned by verified discovery, not a Revision
metadata ID. These placeholders must be replaced with actual returned values:

~~~sh
lwiki --wiki "$WIKI" --offline --json read --path 'RETURNED PAYLOAD PATH' --start START --end END --max-bytes 4096
lwiki --wiki "$WIKI" --offline --json read --path 'RETURNED PAYLOAD PATH' --start START --max-bytes 8192
~~~

Omitting end reads toward EOF within the byte allowance. Use returned
`continuation.start`/`end` for another bounded read; each range has its own hash
and citation. Invalid UTF-8 boundaries or out-of-range endpoints fail. Empty EOF
reads have no citation. Inspect `source_citation.eligibility`: reading historical
or withdrawn bytes does not make them current. Cached, legacy and authored reads
do not produce captured-source citations. Dry-run returns a request plan without
target resolution, range validation, text or proof.

Captured spans address complete content bytes. Authored context/search spans
address the whole file including frontmatter; ordinary authored `read` offsets
address its body. Do not pass whole-file spans unchanged as body coordinates.
For exact physical offsets, use [inspect](everyday-use.md#inspect-and-edit-exact-bytes).

If a discovered Source misses a condition, search within its actual payload path
and read returned spans instead of repeatedly asking the same broad question:

~~~sh
lwiki --wiki "$WIKI" --offline --json search 'MISSING CONDITION' --mode lexical --path-prefix 'RETURNED PAYLOAD PATH' --limit 1 --candidates 80 --excerpt-bytes 512 --verify-selected --no-sync
~~~

Set finite calls/time/evidence bounds before investigating. Count full responses,
warnings and failures; stop with explicit gaps when exhausted. Empty discovery
or bounded context does not prove absence or complete coverage.

## Inspect named neighbors

With an existing reviewed graph, find a named Entity and copy the selected hit's
`record_ref.record_id`; similar names need not identify the same Entity:

~~~sh
lwiki --wiki "$WIKI" --offline --json search 'Harbor Relay' --mode lexical --kind entity --verify-selected
lwiki --wiki "$WIKI" --offline --json graph neighbors "$ENTITY_ID" --depth 2
~~~

Normalized neighbors require an exact Current Entity ID. An unsupported description
is hidden while its eligible identity stays navigable. Edges preserve their
recorded direction, negation, modality, dates, units and other qualifiers. Incoming
or two-hop paths do not imply inverses or transitive facts. Read actual returned
support and contradiction Source spans with the exact-range command above.

Default depth is one, at most 16 incident candidates per Entity and ten assertions;
maximum depth is two and display limit 50. Filters precede caps. Source filtering
requires current supporting evidence and scopes displayed evidence. Defaults show
two support and one contrary item per assertion, but complete selected evidence
membership is still proved. Lowering display caps does not reduce proof cost.
`truncated`, `meta.partial` and coverage disclose omissions; lower-bound zero
omissions do not prove nothing was missed. There is no continuation cursor.

`graph neighbors ID --no-sync` returns uncited cached adjacency; adding
`--verify-selected` authenticates selected dependencies. Dry-run does not resolve
the Entity or check layout/freshness. General graph queries, historical/proposed
views, semantic seeds and Page navigation remain unavailable on normalized storage;
legacy graph workflows have their own support. See [graph review](../developer/knowledge-graph.md).

## Select passages with a host

Use this explicit assisted route when automatic context omits complementary facts.
The CLI prepares evidence, a separately available host selects IDs, and the CLI
verifies original text. It does not launch a host or generation provider. Host
calls, input, latency and unknown cost need separate accounting and evaluation.

On an existing normalized catalog, keep question, filters and budgets identical:

~~~sh
lwiki --wiki "$WIKI" --offline context 'Which setup steps and exceptions apply?' --mode lexical --scope indexed-documents --limit 5 --candidates 80 --max-bytes 6000 --max-tokens 1500 --prepare-selection > selection-task.json
lwiki --wiki "$WIKI" --offline context 'Which setup steps and exceptions apply?' --mode lexical --scope indexed-documents --limit 5 --candidates 80 --max-bytes 6000 --max-tokens 1500 --selection selection-reply.json
~~~

Check preparation success. Plain output is the exact selector task; with `--json`,
deliver only `data.selection_packet.selector_input`. The selector sees the task
and candidates, returns the exact fingerprint and ordered supplied card IDs, and
keeps its support/gap ledger separately. Save the reply unchanged. Inspect
`schema context-selection`. Legacy selection uses supported `--scope current`;
normalized selection is lexical/current-only. Do not silently switch unsupported
modes or include historical material.

The task caps 80 cards and 130,048 bytes, with 1,024 transport bytes reserved by
the evaluation protocol. Replies cap 20 unique IDs/4,096 bytes. Extra, duplicate
or unknown fields/IDs fail. Ordered selection still competes for final passage,
byte/token and per-source bounds. Changed requests, snapshots or evidence invalidate
replay; editing a fingerprint cannot repair it. Use only successful final verified
context for an answer. Empty selection does not establish absence.

For complete-original host reading, normalized discovery has a distinct route:

~~~sh
lwiki --wiki "$WIKI" --offline --json context 'ORIGINAL QUESTION' --prepare-original-selection --discover-originals > prepared.json
lwiki --wiki "$WIKI" --offline --json context 'ORIGINAL QUESTION' --discover-originals --selection reply.json > replayed.json
~~~

Deliver the exact selector_input and replay the unchanged range-only reply with
identical query/filters/limits. This uses `lwiki.context-original-selection.v2`;
inspect `schema context-original-selection-auto`. Explicit supplied payload paths
use `--prepare-original-selection --selection-original-path PATH` and V1 instead.
Fingerprints/replies are not interchangeable. Complete originals cap 96 KiB text,
512 KiB indexed payload and 130,048 serialized bytes; rejected owners are disclosed,
never clipped and called complete. Final rendered bounds remain unchanged.
Original selection can explicitly include historical captures; card selection
cannot. Changed evidence/publication refuses replay.

The [cited-Page recipe](../../skills/llm-wiki/references/cited-page.md) covers a
checked brief, explicit gaps and guarded draft reconciliation. Assisted success
does not qualify native completeness. [Evaluation](../developer/evaluating-context.md)
defines unseen questions, selector isolation and separate accounting.
