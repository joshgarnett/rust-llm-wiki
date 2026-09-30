# CLEANUP C09/C22/F01/F07 research handoff

Current integration status: root has integrated this work and closed actionable source-review findings. Focused functional, format, strict Clippy and maintained workflow gates pass; corrected routine CI and the final API/research acceptance regressions pass. Full native qualification remains pending. Dated progress and failed attempts below are historical observations, not the current acceptance verdict. Final evidence will be recorded in [CLEANUP-checks.json](CLEANUP-checks.json).

Status: implementation submitted for root integration. Root owns the shared `ResearchSourceRange` DTO, CLI/schema wiring, and builds. No live provider, production vault, or credential was used.

## Implemented

- C09: `research status` now distinguishes an outstanding current/stale packet from retained report history. It exposes packet stage and warnings, gaps, captured and remaining source/byte/round budgets, import readiness, report citation freshness, and a concrete next command. `report_view` keeps the immutable report fields while adding a read-time citation lifecycle projection and next action. Withdrawal or revision changes do not rewrite the historical report.
- C22: New report artifacts are readable Markdown: question, completion reason, unassessed claims, source revision links with exact byte ranges, acquisition dates when captured or host-claimed, gaps, and retained-history freshness. The original machine payload remains in its fenced block, with the final artifact hash used for immutable identity. Existing report decoding remains supported.
- F01: Each selected source may carry explicit absolute UTF-8 byte ranges (bounded to 32 ranges of at most 4096 bytes). Without ranges, deterministic question-term ranking selects up to two bounded chunks, including relevant later sections. All passages retain exact revision and byte-span citations. Packet refresh drops stale numeric range selections and tells the host to select again.
- F07: `research maintenance` returns bounded, read-only tasks with record ID, title, path, reason and host action. It covers stale retained citations, outstanding stale packets, unassessed synthesis, relevant disputed assertions, malformed non-authoritative `wiki_evidence` navigation, noncurrent reviewed-page dependencies and broken page links. Publication remains an explicit guarded host action.

## Integration seams

Research functions are `research::status(&OfflineApp, &RecordId) -> Result<Value>`, `research::report_view(&OfflineApp, &RecordId) -> Result<Value>`, and `research::maintenance(&OfflineApp, &RecordId) -> Result<Value>`. `ResearchScope.source_ranges` contains `ResearchSourceRange { source_id: RecordId, span: ByteSpan }`; root has wired the public DTO, `--source-range SOURCE:START:END`, schema, module export, maintenance command, and recovery fixture. The report command can use `report_view` for JSON and the immutable Markdown artifact for human reading.

## Regressions and limits

`tests/research_handoff.rs` now checks selection of a later relevant section, exact explicit Unicode span and rejection of a split UTF-8 range, readable Markdown with the unchanged machine fence, and read-time stale report/maintenance state after source withdrawal. Existing stale-packet fixture gained status assertions. Root's expanded `research_handoff` gate reported 13 passes and one failure: a broader lexical passage replaced the exact host-selected range. Packet selection now prioritizes and preserves explicit ranges, with a unit regression for either input order. This correction was formatted but has not yet been rerun under the shared build lease; the gate is still pending.

Relevance ranking is deterministic local keyword matching over bounded chunks, not an embedding or semantic assertion. Maintenance is scoped to sources in the run's scope, retained report, and packet; it is a repair queue, not a complete vault-wide citation audit. A further improvement would include all captured source IDs from import receipts when they no longer appear in the latest packet or final report. Historical report Markdown records freshness at creation; `report_view` and `status` provide the live overlay.
