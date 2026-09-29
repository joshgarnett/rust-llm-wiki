# Independent review of B1/B2, B5, B11

Static review of the current shared source and added regression cases on 2026-09-29. No builds, provider calls, Git operations, source edits, or nested agents. Findings were sent to root before this report. Source is being integrated concurrently; function names identify reviewed locations if line numbers shift.

## R1 — refresh forgets omitted/evicted nonexplicit captures

Confirmed in `research::engine::resume`: its inspection source IDs consist only of `head.scope.source_ids` plus IDs occurring in the old selected packet passages, capped at 32 before withdrawn sources are removed. A source captured by this run but omitted from the packet, or evicted by a later round's new captures, is absent from both lists. It only returns if the default bounded lexical search happens to rediscover it. Withdrawing current high-priority sources does not make the forgotten captures explicit candidates again. Import's new reinspection of `head.scope` repairs omitted original explicit sources but not omitted imported captures.

Minimal fix: reconstruct the run's captured source IDs from hash-verified immutable `head.receipts` / `ImportReceipt.captured_sources` (verify run, packet, submission bindings as in replay), combine/deduplicate with explicit and retained sources, resolve current active heads, and inspect them. Keep original scope/hash immutable. Bound receipt reads and candidate counts by existing run limits. Do not apply the output `MAX_PASSAGES` ceiling before eligibility filtering/candidate selection; up to 32 explicit +64 captured IDs are legitimate input candidates even though the resulting packet holds at most32 passages. Keep omitted candidates disclosed.

Regression: use multiple collection rounds whose total captures exceed64KiB of packet context, ensuring an older nonexplicit capture falls outside the old packet and outside lexical hits; withdraw selected sources, refresh, and assert that capture returns with a current verified citation. Test a refresh on another refresh as well, so candidates do not disappear after one transition.

## R2 — containment remains ordering-dependent

Confirmed in `research::engine::packet`: it skips an incoming source span only when an earlier selected span fully contains it. On a later collection import, `inspection::inspect(head.scope)` precedes `previous.passages`. A previous captured source absent from explicit scope can have a short freshly found lexical excerpt selected first, then its longer retained source passage selected later. Both survive, wasting the exact packet space this change intends to recover.

Minimal fix: when a later same-source/same-revision containing span can fit after replacement, remove the earlier contained source spans and replace at a deterministic priority-preserving position, updating bytes/counts. Do not remove selected useful context if the larger incoming span cannot fit. Alternatively order explicit/new/retained authoritative source spans before lexical candidates consistently, but retain containment handling for arbitrary incoming candidate order. Keep source and assertion citations distinct: equivalent quoted bytes do not automatically erase assertion evidence semantics.

Regression: same source/revision narrow-before-wide and wide-before-narrow permutations, then a near-64KiB case where replacing nested spans frees a slot, and a larger-span-does-not-fit case preserving the smaller selected span. Existing `explicit_sources_cover_lexical_subsets_on_start_and_refresh` covers only wide-before-narrow.

## R3 — catalog fingerprint must identify opposition eligibility semantics

Confirmed at review time: `catalog::scan::parser_fingerprint` did not include the new accepted-opposition eligibility rule. Cached projections store `RecordRow.disputed`; existing projections could therefore retain false while newly executed graph traversal finds `opposing_assertions`. Increment the semantic fingerprint so old cache projections reject/rebuild under existing machinery. This is a derived-cache migration, not a canonical record edit.

The new core rule is otherwise conservative: both rows must be accepted/current after dependency eligibility propagation. The opposition key includes subject, predicate, object-vs-literal and literal type/value, property, unit, modality, and exact date bounds; only negation differs. Proposed/rejected/unsupported/historical matches cannot create a dispute. The current tests cover multiple deterministic links, exact property/date/modality/object mismatches, accepted/proposed/rejected distinction, bounded link counts, and withdrawal. Typed values are canonically strings, so `record.string` is appropriate here.

## R4 — opposition omission disclosure (minor)

Graph edges expose `omitted_opposing_assertions`, but `graph::query::result` did not include that count in its omission warning or `truncated` computation. If a caller checks only top-level truncation, an otherwise complete graph can look exhaustive despite capped opposition links. Add a specific warning/top-level truncation signal when any emitted edge omits opposition links. Existing evidence omission semantics already use per-edge counts; avoid implying opposition records are equivalent to explicit contradicting evidence citations.

## B5 reviewed with no consequential defect found

`ResearchReport.partial` now represents requested-but-unfinished continuation: false when the agent finishes (even with honestly retained gaps), true while follow-up is requested and at a round-limit terminal stop. `completion_reason` distinguishes `agent_finished`, `follow_up_requested`, and `round_limit`. No-follow-up gaps remain visible rather than being cleared. Terminal status still follows presence/absence of a next packet. The optional/default-deserialized field preserves reading existing report artifacts without claiming they had a newly known completion reason.

The new tests assert finished-with-gaps is not interrupted and preserve follow-up/round-limit behavior. Maintain these semantics in user documentation. No dynamic test pass is claimed by this review.

## R5 — refreshed withdrawal reintroduced by collect import

Confirmed new regression: refresh removes withdrawn source IDs only from its temporary `inspection_scope`, preserving immutable `head.scope` as intended. The newly added collect-import call to `inspection::inspect(...,&head.scope)` reintroduces a withdrawn explicit source. `source_citation` can resolve its retained head, but `view.verify(...,Current)` rejects it. Therefore start with explicit source → withdraw source → refresh collection packet → submit valid new source fails despite the refreshed packet no longer depending on that withdrawn source.

Minimal fix: share active-source-filtered candidate reconstruction between refresh and collect import while leaving the immutable scope unchanged. Regress the complete lifecycle above and assert no withdrawn passage is reintroduced.

## R6 — newly inspected existing-source proofs missing from publication read set

Confirmed at review time: collect import's `dependencies` starts with `inspection::verify(previous)` and the old packet artifact. The newly inspected passages added to `next` carry dependency proofs, but those proofs are not merged into the final ChangeDraft read set. An omitted explicit source can now be included in the next packet without publication rechecking its bytes. An external source change after inspection can therefore commit a stale next packet and cause `outcome` to report failure after commit.

Merge dependencies of selected next-packet passages into the publication read set, excluding newly created operation targets (their proposed content is already bound by the operations and cannot be required to exist before publication). Detect conflicting expected hashes instead of silently accepting the last entry. Candidate reconstruction through saved receipts likewise needs receipt binding proofs carried into its publication read set if it reads new authority during the operation. This maintains the existing commit/freshness contract; it does not require broadening source selection.

## Follow-up closure observed

Root added `opposing-accepted-assertions-v1` to the catalog semantic fingerprint and incorporated `omitted_opposing_assertions` into a dedicated warning plus top-level `truncated`. R3 and R4 are statically closed by those source changes. R1/R2/R5/R6 were sent to root for the research worker. No builds/tests were run during this review.

## Final research delta review

R1/R2/R5/R6 are statically closed by the final Sol delta:

- `current_context_scope` reads hash-bound immutable receipts, checks run/packet/submission bindings and aggregate capture counters, collects recent captures plus explicit/retained sources without the former32-candidate truncation, deduplicates, and drops withdrawn heads. Both refresh and collect import use it. Receipt hashes enter the publication read set. Missing/invalid source identities fail explicitly.
- `packet` now reclaims previously selected contained source spans, inserting a larger span at the first replaced position only if the resulting byte total fits. It keeps smaller context if the wider span cannot fit. Existing wide-before-narrow removal and source/revision separation remain intact.
- Import merges selected next-packet dependency proofs, excludes proposed newly created targets, and rejects conflicting expected states instead of silently overwriting them. Refresh independently verifies fresh passages plus receipt read dependencies.
- New regression code covers eviction across rounds and rediscovery after withdrawing the newest capture, withdrawal→refresh→collect import, and narrow-before-wide replacement with a no-capacity fallback. Existing explicit-source subset tests cover the reverse containment order.

No further consequential correctness defect was found in this bounded rereview. Root owns dynamic tests; this review does not claim they passed. B12's owner clarification has also been recorded in the embedding report: six units for five sources/no pages at12,000 bytes is consistent with one article splitting into two, and the owner reclassified the request as heading-quality improvement.
