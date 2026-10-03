# Actual-context retrieval review

Completed 2026-10-03 UTC. Query-focused spans and 1024-byte CLI context defaults materially improve usable evidence. This independent host-agent review inspects actual returned text: **usable** supports the request, **partial** supports a subpart/premise, **not-useful** lacks requested facts. These are qualitative agent judgments, not human evaluation or generated-answer scoring.

Both binaries were compared against the same final vault with frozen questions/sources and 6000-byte/1500-estimated-token budgets. The historical first baseline remains preserved; added provider bookkeeping altered FTS statistics before the paired rerun. No initial-to-final lexical rank gain is attributed to code.

| 22 answerable queries | Paired old usable/partial/not-useful | After usable/partial/not-useful |
| --- | --- | --- |
| Lexical | 1 / 6 / 15 | 11 / 9 / 2 |
| Semantic | 0 / 9 / 13 | 11 / 10 / 1 |
| Hybrid | 0 / 9 / 13 | 11 / 10 / 1 |

None of five multipart questions was fully supported before. After: lexical four usable/one partial; semantic/hybrid one usable/four partial. Both absent facts remain unsupported in every mode. Nonempty neighbors establish neither measured lwiki latency nor embedding pricing; claim generation/abstention was not tested.

Residual examples: q04 captures XDG but omits precedence; q16 captures `should_panic` but omits Result-returning tests. q06 semantic/hybrid now captures both testing crates; lexical still misses `assert_fs`. q17 lexical captures serialization but misses integration-test placement; semantic/hybrid finds placement but misses serialization. q07 semantic/hybrid remains not-useful, selecting observation/repetition instead of choice/minimal-explanation guidance. q09 lexical loses Reference because larger excerpts admit only three owners. These are window/ranking/facet and finite-budget tradeoffs; no additional executable logic or citation-integrity defect was established.

All **539 paired-old + 314 after passages** independently byte-match immutable source spans. The old default often returned provenance or cut off assertions. Cached offline 1024/2048-byte experiments showed more detail but less source diversity; 1024 retained tutorial/quality sources where 2048 dropped quality. Raising both context budgets, narrower questions, source filters, or reading cited revisions can recover omitted material. High source-ID recall alone does not demonstrate answer completeness.

[Comparison and per-query evidence](../../../.artifacts/ux-corpus/retrieval-review/comparison.json) links through local phase JSON/snapshots; experiments remain under `excerpt-experiments`. No corpus/label mutation or worker provider call occurred.

After benchmarking, explicitly refresh CLI-testing from corrected MIT-only text, preserving source identity and immutable old revision; verify new revision, embedding coverage invalidation and current/historical evidence. External manifest correction alone does not repair its captured header.
