# Inspect a named Entity and its evidence

On a normalized catalog, `graph neighbors ID` returns the selected Entity's
accepted, Current assertions, their direction and qualifiers, supporting evidence,
contrary evidence and opposing assertions. It authenticates displayed records and
their complete selected dependencies. Discovery remains tied to the published
index generation; global membership, completeness and unselected freshness are
unverified. The [independent workflow and access validation](validation-named-neighbors.md)
passes at 9.5/10 with all thirteen tasks complete and no correctness blockers.

Use this with an existing reviewed graph. Graph extraction, import, resolution and
review retain their legacy workflow; their normalized adapters are still pending.
`index rebuild --normalized` explicitly activates the normalized layout.

## Find the Entity, then inspect its neighbors

Search names or aliases, inspect the matches, and copy the chosen hit's
`record_ref.record_id`. Similar names can identify different Entities.

```sh
lwiki --wiki '/path/to/My Wiki' --offline --json search 'Harbor Relay' \
  --mode lexical --kind entity --verify-selected
lwiki --wiki '/path/to/My Wiki' --offline --json graph neighbors ENTITY_ID
```

An Entity with Current identity remains navigable when its description is
Unsupported. The response hides that unsupported description. Each assertion
retains its recorded subject, predicate, object or literal, negation, modality,
time, property and unit. An incoming edge keeps its original direction; a two-hop
path describes recorded edges and does not assert a new transitive relationship.

The default depth is one, with at most 16 incident candidates per visited Entity
and ten displayed assertions. Depth zero returns the Entity alone; depth two
allows bounded additional traversal. Cycles terminate through stable-ID
deduplication. Output order is hop count followed by Assertion ID.

```sh
lwiki --wiki '/path/to/My Wiki' --offline --json graph neighbors ENTITY_ID --depth 2
lwiki --wiki '/path/to/My Wiki' --offline --json graph neighbors ENTITY_ID --limit 1
lwiki --wiki '/path/to/My Wiki' --offline --json graph neighbors ENTITY_ID \
  --source-id SOURCE_ID --path-prefix knowledge/assertions/
```

Kind, tag, path and authored-status filters apply to Assertions before candidate
limits. The named root and displayed endpoint identities remain visible. A Source
filter requires Current supporting evidence from that Source and scopes displayed
evidence; a contrary association alone cannot include an Assertion.

## Read supporting and contrary source bytes

Each `support` or `contradictions` item contains a `source` reference with Source
ID, immutable Revision ID and byte span, plus an authenticated citation. Use those
returned values for an exact read:

```sh
lwiki --wiki '/path/to/My Wiki' --offline --json read \
  --path 'sources/SOURCE_ID/revisions/REVISION_ID/content.md' \
  --start START --end END
```

Supporting evidence, contrary evidence and an opposing Assertion are separate
fields. Exact citation bytes establish integrity; assessing whether those bytes
support the proposition still requires reading the evidence. The CLI makes no
model or provider call for named neighbors or these offline reads.

## Bounds, omissions and freshness

`truncated`, envelope `meta.partial` and `coverage` disclose incomplete traversal.
When `omissions_are_lower_bounds` is true, zero known omissions does not establish
that nothing was omitted. Incident sentinels do not reveal an exact total,
especially when the same Assertion is encountered from multiple Entities.

Display defaults are two supporting and one contrary item per Assertion.
Verification authenticates complete selected support, opposition and policy
membership before applying display caps. Evidence omission counts include capped,
ineligible and Source-filtered associations. Excessive connected proof can return
`BUDGET_EXCEEDED`; lowering displayed support does not shrink the required proof.

Fixed verification bounds are 64 MiB, 4,096 files, 16,384 entries and two seconds.
Catalog reads are bounded to 4,096 decoded rows, 8 MiB per row, 256 MiB aggregate,
30 seconds and 10 million SQLite VM steps. Maximum depth is two and maximum
displayed assertions is 50. Traversal has no continuation cursor.

External edits to selected records or immutable source bytes cause a freshness
conflict. Run explicit `index sync` after a valid mutable Markdown edit. Preserve
immutable revisions; use managed `source refresh` to capture changed source bytes.
Refresh retires old Evidence offsets without silently revalidating them. Surviving
Current support can keep an Assertion eligible; withdrawal of its last Current
support removes it from Current discovery. Exact old source reads remain available
with historical or withdrawn labels.

For explicit cached adjacency, use `--no-sync`. Its references have no verified
citations and metadata reports `index_snapshot`. To authenticate selected
dependencies while retaining that flag, add `--verify-selected`:

```sh
lwiki --wiki '/path/to/My Wiki' --offline --json graph neighbors ENTITY_ID --no-sync
lwiki --wiki '/path/to/My Wiki' --offline --json graph neighbors ENTITY_ID \
  --no-sync --verify-selected
lwiki --wiki '/path/to/My Wiki' --offline --json --dry-run graph neighbors ABSENT_ID
```

Dry-run validates the request and returns null results before opening the graph
catalog or resolving the target. Normal vault/configuration binding still occurs;
target existence, layout capability and freshness remain unknown.

Normalized named lookup requires an exact Entity ID and the Current view.
Historical/proposed graph views, Page/provenance navigation, semantic seeds,
cursors and general `graph query` refuse as unavailable on this layout. Exact
historical source reading is a separate supported operation. Legacy regular graph
commands retain their existing snapshot and navigation behavior.

See [selected context and source reading](indexed-context.md), [the contract map](current-contracts.md)
and [graph schemas and review](knowledge-graph.md) for the surrounding workflow.
