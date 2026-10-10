# Original-source selection and cited Page workflow

The 0.2.0 candidate at `3e2d2b4cf3b3b4319189062231f6ef9f5968bf2b`
adds an explicit assisted route from discovered captured Sources to compact,
authenticated evidence. Ordinary automatic context selection is unchanged.
The branch is `impl/original-source-selection-20261009-001`.

Use verified discovery to obtain captured payload paths. Prepare them with
`context QUESTION --scope indexed-documents --mode lexical
--prepare-original-selection --selection-original-path PATH`; repeat the path
flag for complementary Sources. A host receives the exact selector task and
returns one versioned JSON nomination of original IDs and UTF-8 byte ranges.
Replay the identical request with `--selection REPLY.json`. Native replay
authenticates the bytes and supplies SourceRefs for ordinary typed Page writes.
Explicit historical questions use `--include-historical` on both requests.

Preparation admits at most 16 paths, 96 KiB complete captured text and 512 KiB
indexed payload. The serialized task ceiling is 130,048 bytes, with a separate
1,024-byte transport reservation. Replies admit at most 4,096 bytes and 16
nonoverlapping ranges, four per content owner, under the excerpt limit. No
original is silently truncated. Changed requests, budgets, publication or
dependencies invalidate the reply. The CLI calls no model; host work needs
separate accounting. Direct selected paths avoid another broad discovery query.

## Correctness checkpoint

One grouped optimized native ARM64 checkpoint built the CLI and passed 245
related tests, with seven pre-existing ignores. Four skill-export checks passed;
one failed because maintained exported snapshots were stale. Those snapshots
were refreshed from the pinned executable, and only the failed check was rerun
and passed. Production code was not rebuilt after that correction. These are
affected correctness checks, not a full release suite.

## Independent integration evidence, October 10, 2026

Fresh actors received only their question, normal product guidance and bounded
public-command access. They discovered their own owners; setup IDs, paths and
expected answers were withheld. A new fictional corpus contained three Sources
and five immutable captures, totaling 5,429 bytes. Labels were not indexed.

| Observed requirement | Result |
|---|---:|
| Question tasks completed, including absence | 6/6 |
| Required facts in final evidence and answers | 19/19 |
| Returned exact citation spans | 10/10 |
| Binding, input, discriminator and freshness controls | 8/8 |
| Original frozen lifecycle observations | 6/7 |

The separate lifecycle actor returned the changed governing fact after Source
refresh. Guarded Page reconciliation preserved authored text and replaced
generated references with the new native SourceRefs.

The original integration gate remains **NO_GO**. Its final historical read used
a positional mutation-plan binding: slot 1 was `original.bin`, whereas the cited
payload was `content.md`. The CLI correctly refused that unindexed path; the
following current-citation read was unrun. This is an operator input error, not
an observed failure to read the cited content. An initial actor-launch pathname
correction before question exposure is retained and its additional bytes charged.

An independent Astra reviewer prospectively authorized a separate two-call,
read-only supplement using the actual locators already returned in the successful
packets. Both reads passed: the old capture was Historical with unchanged full
hash and exact original cited bytes; the new exact span was Current with the
identical typed citation. The closed original ledger stayed unchanged. No answer,
nomination, SourceRef or canonical data was rewritten. Cumulative native attempts
were 45, including the failed read. The earlier root-relay attempt, which produced
only one completed answer before delivery failure, also remains failed.

The supplement's native work finished within the retained audit deadline, but
the composite report and final preservation seal exceeded that deadline. This
timing failure is retained. Independent review supports a **useful scoped
assisted-workflow preview with no observed correctness blocker**; it does not
rescore the original gate or award an aggregate score of at least 9.

Seven host jobs used 425,772 known charged input bytes including conservative
transport reservations. Inference-call count, tokens, cost and inference latency
are unavailable. Original native intervals totaled 4.884 seconds; the supplement
owned 0.242 monotonic seconds. These small-fixture measurements do not establish
shipping latency or capacity. All 17 unfinished main-worktree files and 331
candidate source pins remained unchanged.

The optimized executable SHA256 is
`ba2b19d6032e55570b4cb7b1ccabf5e7b5ce7b1bc685498c7309720faf27c238`.
Original assessment SHA256:
`6aaf3d3e3df0acf52ed5e33b76ff0ac5963d37f7d7467bbb5b88d7dd7f091769`.
Composite assessment SHA256:
`920904122ef2e36ff5757400e326f8f55e10d0cc1cd74d67466c4fe7ca4f8dd4`.
Preserved result SHA256:
`fb761e0af84bc5776250f58e3c38dcf41894933dbf7a3786145d6ea075c3a858`.
Detailed protocols, labels and raw evidence remain local.

Native automatic completeness, semantic retrieval, representative 25K / roughly
2.5 GB behavior, HIGH and full release acceptance remain open. The overall goal
is active. Next work should measure representative default tasks and operational
readiness rather than repeat this assisted fixture or tune its exposed questions.
