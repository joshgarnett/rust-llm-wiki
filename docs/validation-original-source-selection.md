# Original-source selection and cited Page workflow

The original isolated 0.2.0 candidate at `3e2d2b4cf3b3b4319189062231f6ef9f5968bf2b`
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


## Full development workflow attempt, October 10, 2026

A separately frozen forty-question campaign stopped after its first verified
lexical discovery. The first question was to complete a cited Page, controlled
Source refresh, author-preserving reconciliation, history and offline rebuild
before the remaining questions. None of those later stages ran. This campaign
is **NO_GO**, with no answer-quality score or native completeness gain.

The native search succeeded offline in 0.247 seconds and returned ten verified
hits from eighty candidates. Its complete 30,854-byte JSON response matched the
retained actor delivery byte-for-byte; all ten source spans and quote hashes
authenticated. The relevant Source ranked first. Its excerpt did not contain the
requested checklist, so subsequent original-source reading was still necessary.

The operator's instruction to stop on any truncation conflated bounded candidate
coverage (`candidate_cap_reached`, `truncated=true`) with incomplete tool delivery.
The actor reasonably stopped before preparation. This does not demonstrate a
native search or transport failure, successful source selection, or an answer.
The terminal recorder also lacked a transition for an actor-declared failure
after a successful command; its later phase refusal is secondary to the sealed
actor's original reason. No retry or prompt variant rescored the attempt.

One host invocation and one native call consumed 39,046 known delivered bytes,
including an 8,192-byte instruction reservation. A single owning monotonic clock
observed 121.316 seconds for the campaign and 59.039 seconds for the host interval;
these include orchestration and observation, not just model inference. Underlying
inference calls, tokens and cost are unavailable. An initial dotted-task-ID schema
refusal was corrected before question exposure, with its receipt and original
clock retained. Thirty-nine original questions and the lifecycle follow-up remain
unrun; the first question is incomplete.

Bounded discovery needs an explicit usable-state contract distinct from delivery
failure. Complete evidence selection, supported answers and maintained Pages
still require an integrated acceptance result. The native baseline remains
13/32 complete positives and 79/129 facts; semantic/HIGH, unseen, representative
25K and full release gates remain open. No Rust build was needed for this trial.

A subsequent static capability audit found an additional integration gap: the
trial's pinned shipping binary did not contain the original-source selection API
from the isolated preview. The trial stopped before invoking that API, so this is
a source/capability finding, not an observed second command failure. The next
coherent batch integrates explicit original selection and native bounded lexical
discovery into the shipping branch before further assisted acceptance.

## Integrated native discovery candidate, October 10, 2026

The practical-release branch integrates the historical and explicit-original
preview with the current embedding-proof coordinator. `--discover-originals`
now prepares a V2 task directly from the existing lexical order, eliminating
the separate host step that copied Source paths into preparation. Explicit-path
V1 requests remain compatible. This changes the assisted workflow, not native
ranking or deterministic answer completeness.

Automatic admission examines at most ten hits from eighty candidates, deduplicates
Source/revision identity, and admits complete captures under the existing raw,
indexed and serialized-task limits. It reports ineligible, empty, oversized and
serialized-size omissions without refilling the ranked page. Identical content
from distinct Sources retains distinct provenance. One pinned query/proof budget
covers discovery and authentication, including work for an owner subsequently
omitted by serialized size. Replay binds the admission decisions, request and
publication; candidate omissions are distinct from incomplete task delivery.

Both release compile phases passed with stable phase inputs. The artifact
controller then stopped because the test build unified transitive development
features and produced a different CLI hash. The failed assertion and both
artifacts remain retained. No rebuild or source change was needed: all CLI
subprocess checks and the public trace use the separately pinned ordinary
release-build executable. Library tests exercise their test-feature build.
The only declared between-phase input updates were the generated skill command
reference and manifest.

The integrated checkpoint passed **354 affected tests**, with seven existing
ignores and no test failures. These include explicit V1 compatibility, automatic
V2 admission/replay and public CLI update/history/rebuild checks. They do not
constitute the full release suite. The shipping executable SHA256 is
`7d7ef4d356a8f71f3de7622d106d5d61d31afab062682bd10c8d41007a837034`.

A separate, independently authored three-Source fixture exercised actual public
commands with two complementary Willow workshop Sources and a Cedar distractor.
The initial verifier stopped after eight successful native commands because it
expected a duplicate `originals` field outside the serialized task. The API
intentionally omits that duplicate. The failed attempt remains failed. An
independently reviewed continuation authenticated and reused those eight saved
responses without rerunning their commands, corrected only that assertion, and
completed the remaining 27 commands.

The resulting composite covers complete task preparation and exact replay, cited
Page creation, guarded author edits and stale-author refusal, a successful fresh
control before Source refresh, stale-evidence refusal, Page reconciliation that
preserves every authored byte except the declared deposit update, immutable
historical/current reads, verified discovery and offline rebuild. No distractor
rule enters the Page. The first interval was 1.968 seconds; the separate suffix
was 2.743 seconds. These are separate controller intervals on a tiny fixture,
not one uninterrupted passing trial or a capacity claim. No host, provider call
or answer-quality credit is involved. Development, unseen, semantic/HIGH,
representative scale and full release gates remain open.

Independent review of the actual envelopes and saved Page accepts these composite
native mechanics with no observed blocker. It authenticated all 18 returned
SourceRef occurrences and both final Page references, the three prepared tasks,
and every declared author/freshness/history/rebuild invariant. This clears the
native capability prerequisite for a separately frozen full development
workflow evaluation; it does not turn the original failed verifier run into a
passing trial or establish the required overall quality score.

## Integrated full-development attempt, October 10, 2026

A separate forty-question campaign using native discovery stopped at the first
reader's task delivery. Native preparation succeeded in 0.205 seconds, producing
a complete 123,886-byte task with five authenticated originals totaling 93,987
bytes. The reader requested 50,000 output tokens from the nested shell tool but
omitted the enclosing orchestration tool's output pragma. That enclosing call
retained its 10,000-token default and clipped the task. This identifies a delivery
configuration failure after successful native emission, not a measured platform
capacity limit or a retrieval result.

The campaign is **NO_GO**, with zero completed questions, the first failed before
nomination, and 39 unrun questions including all eight absence controls. No
replay, answer, Page or lifecycle operation occurred. One quality host and one
native call ran; a separate history-only diagnostic follow-up identified the
missing outer setting without reading more evidence or retrying. The wrapper
charged 132,078 bytes for emitted task plus instruction reservation; actual
received task bytes are unknown. Its corrected terminal recording preserved the
primary failure and actual report. Both disposable vaults' 994 original captures
remain unchanged.

The owning campaign interval was 98.565 seconds; the root-observed host interval
was 65.262 seconds, including observation and sealing delay. Inference calls,
tokens and cost remain unavailable. No new provider call occurred. The native
baseline remains 13/32 complete tasks and 79/129 facts. Further actor campaigns
are suspended pending an enforceable delivery boundary; changing another prompt
or output-limit instruction is not accepted as the next experiment.
