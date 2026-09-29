# Manual follow-up invariant review

Read-only source review against baseline `2953cd5`; no builds or tests run by this reviewer. This report records diagnosis and review criteria, not acceptance of the workers' pending changes.

## Refreshed-source resolution

`graph/resolution.rs::guarded_projection` clones canonical notes but closes asset reads to the supplied dependencies. `capture_dependencies` adds every canonical note; it does not add every retained revision's original/content payload. An extraction proves its selected revision, while its source note retains both the old and new revision IDs after refresh.

`catalog/eligibility.rs` verifies every revision represented in that closed projection. The unselected sibling revision's absent payload makes it Invalid. Invalidity propagates through the source's `wiki_revisions` references and back into the selected revision/evidence. This explains why both head and explicit historical extractions fail after refresh while a single-revision source succeeds. Dry-run validates the request without constructing and validating the complete proposal.

The smallest safe correction supplies the complete retained revision asset closure needed by the resolution's referenced source(s), with matching read guards and bounded aggregate capture. Preserve original/content hash checks, revision ownership/list membership, source-head guards, closed-read isolation, and full apply-time validation. Do not make an omitted closed asset count as valid, ignore a corrupt retained revision, or relax acceptance requirements.

Required counterexamples:

- Valid two-revision source: head resolution stages/applies proposed assertions with current evidence; old-revision resolution stages/applies proposed assertions with historical evidence.
- Tampered/missing retained original or content still fails integrity; an unsupported revision still requires its original payload to verify.
- Head/source or payload edits between preparation and application invalidate the captured guards.
- A second, unrelated source does not force unbounded capture or acquire fictitious absence guards.
- No resolution silently accepts an assertion or changes an evidence revision.

## Extraction-packet retrieval

`catalog/scan.rs` already suppresses normalized body/headings for evidence, extraction and extraction-packet notes, but retains their full `raw_text`. Literal retrieval searches that raw text. Extraction packets also have no source-lifecycle eligibility branch, so a packet's copied source window remains Current after refresh or withdrawal. The packet is an immutable operational artifact, not independent current note-text authority.

Default candidate filtering must exclude the operational source copies before ranking/limits, with projection/index handling consistent across literal, lexical and semantic paths. Existing context and embedding corpus code already uses narrower valid-kind allowlists; their regression tests should establish preservation rather than imply an observed valid-packet embedding leak. Keep packet bytes/typed records available to explicit reads, import and idempotent restoration.

A valid-record-only filter is insufficient: duplicate IDs or malformed packet metadata can remove its registry row, leaving an Invalid/plain document that ordinary discovery permits. Cover recognizable operational envelopes and managed packet paths while preserving the general contract that unrelated malformed notes remain discoverable.

Change the catalog projection fingerprint and ensure older caches cannot expose packets through current query paths. Do not broadly remove historical retrieval: source revision passages must remain accessible with Historical/Withdrawn labels and verified source citations when explicitly requested. Packet exclusion should not manufacture freshness or erase canonical history.

Required counterexamples include active, refreshed, withdrawn and malformed/duplicate packet copies; old cached rows; default literal/lexical/context/embedding paths; explicit packet read/import; and requested historical source passages. Source withdrawal must never resurrect copied source text as Current through an operational note.

## Acceptance status

The refreshed-source delta was reviewed after implementation, without executing builds or tests. `capture_dependencies` now captures the selected source's complete retained original/content closure, deduplicates assets, bounds aggregate capture at 64 MiB, verifies ownership, and merges their observed hashes into preparation/application guards. Existing schema-relative paths, vault path checks and closed graph hash validation remain in force. Existing eligibility code contains an unavailable closed asset inside that revision's integrity result, preserving unrelated validation without inventing a physical absence guard; this placement is a preserved invariant, not a new change. No consequential blocker found in the refreshed-source delta.

The added regressions cover selected historical and refreshed-head evidence, proposed assertion status, corrupt retained originals, and sibling payload drift after preparation. The last case asserts `CONTENT_CONFLICT` before entity/assertion/evidence activation. This is source review of the regression assertions, not a claim they passed. Acceptance depends on root's checks. Historical P21 evidence remains unchanged.

### Packet delta finding

The first packet delta correctly adds source lifecycle classification, changes the projection fingerprint, and filters adopted packet kinds or managed packet paths before query caps. Current context and embedding corpus authority allowlists remain protective. However, it leaves one concrete malformed/duplicate fallback: copying a valid persisted packet to `packet-copy.md` duplicates its ID, removes its adopted `RecordRow`, and yields a document with `kind = NULL`, `Invalid` eligibility and searchable raw/normalized text. Outside the managed directory the default SQL policy permits this copy, including after source withdrawal. A recognizable packet envelope with another invalid field has the same result. The new malformed test currently codifies that result for `ordinary-invalid.md` containing `wiki_kind: extraction_packet`.

Recognize packet envelopes independently of successful adoption and unique identity, while preserving ordinary malformed nonpacket discovery. Exercise that recognition before literal/lexical candidate caps and retain it for older cached rows. The current new index-snapshot regression rebuilds with current code; it is not a direct regression for a pre-change cache whose packet eligibility remains Current. The simple adopted-kind/managed-path old-cache guard is supported by code review, but the broader fallback remains open.

Compatibility repair must remain possible. Rejecting an old parser fingerprint in shared `sql::validate` would block both normal sync and explicit rebuild, because publication itself calls that validator. A reader-only check against the pinned generation in `index_snapshot`, returning `OfflineUnavailable`, fits the existing writer-enabled retry paths in `verified_snapshot` and current-context verification. It safely refuses incompatible `--no-sync`/snapshot reads without mutation while permitting structural cache validation and rebuilding. This design recommendation awaits the worker's final delta.

Final packet source review closes the finding. The implementation now recognizes canonical, parsed or isolated top-level `wiki_kind: extraction_packet` independently of adoption; duplicate/malformed packet documents retain that discovery kind and have empty normalized body/headings. The pre-limit SQL exclusion therefore applies outside the managed directory too. Raw historical audit data remains available, and unrelated malformed nonpacket notes retain discovery. The reader-only parser compatibility check uses the pinned generation and leaves shared structural SQL validation unchanged. The new compatibility regression consistently stamps the old hash in both the published generation and serialized projection, asserts `OfflineUnavailable`, then exercises writer-assisted automatic sync. No consequential blocker found; root owns execution results.

## Source-add rollback triage

`changes/rollback.rs::inverse_plan` retains immutable revision notes/payloads while reversing the newly-created mutable source note to absence. Full graph validation then correctly rejects the retained revision's broken source reference. This is an unsupported inverse shape, not permission to erase immutable history.

A narrow usability correction can reject source deletion when the inverse retains that source's owned revisions, with an explicit message and a hint to withdraw the source. Perform ordinary conflict preflight first so changed source bytes still produce a real conflict. Retain complete graph validation and do not broadly exempt inverse operations. A source-refresh inverse can similarly remove a retained revision from the source manifest; do not advertise general source rollback support without defining that case.

The scoped implementation follows this boundary: ordinary plan/conflict preflight precedes the friendly rejection; it verifies the source bytes still match, requires a canonical source and a corresponding retained immutable revision, and recommends withdrawal without deleting history. Tests assert rollback rejection preserves files and edited sources retain conflict precedence. Root has separately requested cumulative debit for the new source-read loop; review acceptance of that final budget detail is pending.
