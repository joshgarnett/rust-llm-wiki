# Query-aware excerpt implementation

Date: 2026-10-02. Worker lease: `src/retrieval/lexical.rs`, `src/retrieval/excerpts.rs`, `tests/retrieval_lexical.rs`, this report. Root owns CLI defaults, fusion/application wiring and all builds/tests. Independent review is assigned to `ux_critic`. No nested delegation, build, provider request, credentials or real vault access by this worker.

## Problem and behavior

The curated corpus has two concrete lexical-context failures: q06 asks about `assert_cmd` / `assert_fs`; q16 asks about `#[should_panic]` and Result-returning tests. Their expected documents rank within the returned results, but excerpts contain introductory or provenance text rather than the requested concepts. Original fallback matching walks query terms in order, lets the first common term consume a global 64-match cap, sorts the surviving positions by source offset and centers the excerpt on the first position. Later useful terms cannot guide selection.

The implementation retains the full-exact-query preference and byte-exact literal branch. Lexical fallback tokenizes the query with the same SQLite unicode61 tokenizer used for discovery, scans matching normalized tokens throughout the eligible body, and evaluates bounded windows around each match. Each distinct query token contributes once within a window, weighted by `1 + ln(document_token_count / token_frequency)`. Fixed-point integer weights avoid floating-point accumulation drift; equal scores retain the earliest original match. Document BM25 ranks, candidate limits and Current/identity policy do not change. No corpus keys, source names, English stopword list or case-sensitive lexical special cases are embedded in the algorithm.

The selected excerpt still comes from original UTF-8 bytes. Returned matched spans are exact, contained in the excerpt, sorted/deduplicated and bounded to 64. Source citations retain eligibility and verified-snapshot checks and hash the actual excerpt bytes. Markdown-decoded transformations cannot manufacture exact source matches. Existing full-query matching remains bounded to 64 full phrase matches.

`pub(crate) focused_excerpt(reader, document, query, span, bytes)` provides the same selection within a supplied valid UTF-8 source span. Its Markdown mapping and final window stay inside that span, including when no query tokens match. Root integrates the helper into query-aware dense-hit rendering while preserving the public compatibility wrapper. It cannot enlarge an embedding unit's evidence scope.

## Bounds and changed paths

- `src/retrieval/lexical.rs`: shared token/window selection, UTF-8 range computation, bounded-focus helper and shared exact-text/citation builder. The caller's byte bound remains authoritative.
- `src/retrieval/excerpts.rs`: binary-search original-span lookup in ordered normalized Markdown segments, preserving the previous overlap predicates. Whole-document matching otherwise multiplies every matching token by a linear scan of all Markdown segments. Token mapping now costs logarithmic segment lookup.
- `tests/retrieval_lexical.rs`: four regression cases for natural-language questions after 160 common-word introductions, multilingual late relevant text, a late distinct-term cluster after more than 64 occurrences per term, and unchanged literal punctuation/case plus exact citation verification. They also assert returned byte bounds, valid source slices, matched-span containment and deterministic repeat selection.

Selection retains linear-size token/match storage in the already bounded document and query, rather than allocating an array of every query term against every source token. The sliding window enters/leaves each mapped match once; weights and distinct counts are bounded by query token count. Window candidates are tied to source token positions, not an exhaustive evaluation of every possible byte start. This improves lexical evidence choice without claiming semantic entailment or universal relevance.

## Validation status

Root integration accepted the change: lexical/semantic/context freshness and CLI regressions pass in the24-target aggregate, with4focused unit cases and strict format/Clippy checks. Original fixture/import/style failures were corrected and retained. Independent paired comparison on an identical index snapshot verifies unchanged rankings and improved in-sample passage usefulness;853reviewed passages byte-match sources. The8heldout questions reveal substantial remaining passage-quality limits (semantic1/6fully usable), so this heuristic is not an answer-completeness guarantee. See [final validation](UX-VALIDATION.md) and [machine checks](UX-checks.json) for exact counts, protocol changes and limitations.
