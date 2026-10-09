# Complete-subject search and Source reading

Ordinary lexical search now prefers an existing candidate whose body contains
the complete tokenized query. This fixes a concrete discovery failure: searching
for `station 10015` previously ranked a different station mentioning activity
010015 first. Reading that Source missed both required dispatch facts.

An independently reviewed offline development comparison on the same
25,000-Source wiki completed **19/19 tasks and 31/31 required facts**, versus
16/19 tasks and 26/31 facts in the baseline's prescribed complete reads. Counting
every returned cited excerpt, the baseline supported 27/31 facts. The candidate
adds four facts absent from baseline returned evidence and fixes three first-owner
reading tasks. Every baseline-supported fact remains available.

## Public behavior

For at least two normalized `unicode61` query tokens, ordinary lexical search
prefers a complete consecutive-token match in the body within the existing final
candidate set. It requires one contiguous original-byte witness equal to the
projected text; matches assembled across removed markup do not qualify. Exact
identity, title and alias lookup retain precedence. BM25 and deterministic ties
retain their order within each preference group. No extra discovery leg is added.

The preference examines at most 80 final candidates, with a 1 MiB raw-body limit
per candidate and 16 MiB total. Exceeding this allowance retains the whole baseline
order and emits `complete_query_preference_work_limit`; catalog errors and query
budget failures remain errors. The candidate cap does not bound discovery scans.
The same policy applies to cached and selected verified ordinary lexical search
on both layouts. Its cursor fingerprint prevents reuse of old-policy cursors.
Single-token, literal, semantic, hybrid and native context policies remain unchanged.

Use the captured-text path actually returned by verified search:

```sh
"$LWIKI" --wiki "$VAULT" --offline --json search \
  'station 10015' --verify-selected
# Set PAYLOAD_PATH from the selected hit's actual locator.path.
"$LWIKI" --wiki "$VAULT" --offline --json read \
  --path "$PAYLOAD_PATH" --max-bytes 131072
```

Check the returned range, EOF, eligibility, omissions and citation. Improved owner
selection does not establish that a Source answers the question. Follow the
[bounded reading recipe](../skills/llm-wiki/references/cited-page.md#read-small-discovered-sources-completely)
and retain unchanged returned CitationRefs when authoring a Page.

## Frozen comparison and actual evidence

Both binaries used identical offline queries, limits and source revisions at
unchanged publication generation 3,130. Each task used the first actual eligible
returned Source and one complete read of at most 131,072 bytes, with no continuation,
retry, known-owner substitution or query repair. Nineteen tasks covered current,
historical and mixed facts, additional subject controls and four lexical negative
controls. Both binaries executed 38 commands; all 76 completed successfully.

| Observable | Baseline | Candidate |
| --- | ---: | ---: |
| Complete tasks | 16/19 | 19/19 |
| Required facts in prescribed complete reads | 26/31 | 31/31 |
| Required facts in all returned cited evidence | 27/31 | 31/31 |
| Current required facts | 6/8 | 8/8 |
| Historical and mixed required facts | 4/4 | 4/4 |
| Lexical negative controls | 4/4 | 4/4 |
| Authenticated excerpt/read citation occurrences | 65/65 | 65/65 |
| Station lookup native search interval | 0.758258 s | 0.832061 s |
| Station lookup native read interval | 0.065568 s | 0.061103 s |

All original UTF-8 spans, BLAKE3 hashes, Source/Revision bindings and eligibility
authenticated. Eight independent unselected-payload inspections authenticated
citations only and added no fact credit. No regression, error or exclusion was
omitted. Candidate maximum native interval was 0.953682 s and maximum native-or-sampled
RSS 54,951,936 bytes. These are warm single-pair observations with the baseline first;
**no speedup or latency distribution is claimed**. The station lookup decoded 424
catalog rows and 30,962,559 bytes in both binaries.

The owning collector interval was 1,267.720744 s, including inherited whole-account
inventories; native intervals summed to 10.180966 s. Separate preservation audits
took 32.075819 s. Recorded task durations are prefixes; the collector separately
enforced deadlines after body validation. Final report and process-exit tails
were not exactly owner-timed. These are not final-answer latency measurements.

One grouped native release checkpoint passed 61 focused tests, with one explicitly
environment-dependent probe intentionally ignored. The pinned executable used
source `bbaba416e6471426bc739b8ccdb188ba84761a7c`, native macOS ARM64, optimization
level 3 and debug level 0. Passing checks were reused without rebuilding.

## Save, reopen and discover the cited draft

A separate, prospectively frozen three-command boundary used only the sealed
candidate station evidence. It quoted the first and last distinct dispatch lines
verbatim, created a draft with `page init --source-refs`, read it by its allocated
ID and discovered it with `search --kind page --status draft --verify-selected`.
The actor received no expected facts and made no remote call or inferred answer.

Independent actual acceptance passed at **10/10 with no correctness blockers**:
both frozen facts are present, both SourceRefs authenticate, and all six generated
Source/Revision/content links resolve correctly. The 159-byte authored body, draft
identity, full-file hash, after-image and ordinary selected discovery agree.
Only the new absent Page destination changed; publication advanced once from
3,130 to 3,131. The selected Source, every retained revision payload and the
previously moved Page remained byte-exact.

All three calls passed their 15-second, 1 GiB RSS and 262,144-byte stream limits.
The owning procedure took 67.778069 seconds, operation plus validation 47.824280
seconds and preservation audits 12.096950 seconds. Native intervals summed to
0.514901 seconds; the largest RSS observation was 21,676,032 bytes. Root sealing
took 12.493535 seconds separately. Account allocation was 25,340,715,008 bytes,
with 48,831,877,120 bytes free after all remaining reserves. Whole-procedure
timing includes the unchanged boundary inventories; final emission/exit tails
were not separately owner-timed. This is a known-data quoted draft, not a general
answer-generation or performance qualification.

## Acceptance boundary

The [25K import fixture](validation-25k-import.md) contains 24,998 varied synthetic
documents and two prefixed Cargo documentation copies. Additional questions use
four known public development bodies. This is a scoped development pass, not
unseen or representative retrieval acceptance.

**Default native context quality has not improved in this slice.** Complete
Source evidence is separate from a generated answer's completeness. The original
first-eight station task remains failed under its original protocol; this later
comparison does not rescore it. Representative evaluation, semantic/HIGH/unseen
gates, the full 25K lifecycle and release qualification remain open.
