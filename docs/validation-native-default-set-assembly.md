# Native default set assembly: rejected experiment

This branch preserves a tested experiment, **not a retrieval quality improvement or a release candidate**. The ordinary lexical context path replaced irreversible passage ordering with redundancy-aware set assembly. Independent development evaluation found worse task completeness, so the change was not promoted into the 0.2.0 preview.

The experiment retained owner discovery, source revisions, structural proposal construction, 32 proposals per owner, and public budgets. It considered at most 320 proposals from ten owners. Distinct normalized lexical tokens, local-pool IDF, weighted Jaccard similarity and raw owner-rank/local-relevance weights formed a facility coverage objective. Additions used exact coalesced rendered-byte cost; one replacement sweep rebuilt original membership through existing citation and admission checks. A fixed 3,072 addition and 1,024 exchange realizations bounded work. Interrupted comparisons preserved the previous committed membership.

Evaluation used 994 real captured Sources from WixQA, ConditionalQA and QASPER, totaling 5,582,095 UTF-8 bytes. Forty unchanged natural questions comprised 32 positive tasks and eight absent-information controls. Baseline and candidate used identical Source revisions, query wording, caches, offline lexical defaults, 12,000-byte/3,000-token context budgets and proof limits. Development labels were fixed before queries; the holdout remained sealed.

| Observation | Baseline | Candidate |
| --- | ---: | ---: |
| Semantically complete positive tasks | 13/32 | 9/32 |
| Support tasks complete | 11/20 | 7/20 |
| Long-document tasks complete | 2/12 | 2/12 |
| Supported required facts | 79/129 | 73/129 |
| Strict prescribed-span completeness | 11/32 | 3/32 |
| Native context p95, nearest rank | 0.1133 s | 0.2505 s |
| Maximum native context time | 0.1369 s | 0.2835 s |

The candidate preserved 62 of the baseline's 79 supported facts, lost 17 across ten tasks, and gained eleven. Six previously complete tasks regressed; two became complete. All forty contexts activated the experiment and reached its work allowance. No native command failed, no case was excluded, and no greedy fallback occurred. All 369 source citations and 208 selected owners' Current locator bindings were authenticated. These are small-corpus observations; per-query RSS was unavailable and no 25K capacity claim follows.

The declared development gate required at least 17 complete positive tasks and preservation of every baseline-supported fact. It failed. One exposed baseline label ambiguity remained uncredited without changing the 129-fact denominator. Scoped absence observations do not establish answer-level abstention.

The release executable used Rust 1.98.0, optimization level 3, debug level 0 and stripped debug information; actual compiler flags were recorded. The grouped correctness checkpoint passed 272 unique affected checks with seven ignored tests. A test import error and seven shared fixture setup failures were retained; test-only corrections replayed the failed stage/cases without another production build or repetition of unchanged passing checks.

The evaluator now recognizes the actual `indexed_evidence` selected-document contract only when mode, domain, generation and timestamp bindings agree. It preserves the distinction from global snapshot verification. Citation ownership requires the same containing Source or byte-identical complete originals. The evaluator's 34 Python checks passed. Existing baseline outputs and their original freshness errors were retained and rescored separately under independent amendment.

The experiment is retained at commit `80c1217808ccbb9e382bbfd4f88393e74b53cabe` on branch `impl/representative-default-quality-20261010-001`. The subsequent [query-ranked lexical-unit experiment](validation-native-lexical-units.md) also failed its fixed task and preservation gates. A bounded continuation diagnostic recovered only part of one case and failed its inherited elapsed allowance in three others. Neither result justifies further facility-score or cap tuning. The independent holdout, semantic, HIGH and representative 25K release gates remain open.

See the maintained [evaluation protocol](evaluating-context.md) and [dataset reference](eval-datasets-quick-reference.md). Detailed run records are local evidence; this summary does not require ignored artifacts to explain its conclusions.
