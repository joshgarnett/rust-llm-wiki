# P08 independent review

Reviewer: independent `gpt-6-sol`, authorized playbook fallback after fresh/restarted Astra review threads were unavailable at the runtime thread limit. Accepted baseline: `c90953e3e610e98f2dc2031cfeff72464a25b805`. Read-only source review; only this report is reviewer-owned. Final source re-review closes R1–R4. Worker corrected targeted runtime and Clippy passed; root acceptance remains the integration decision.

Reviewed P08 package and retrieval graph/fusion/budget, identity/invalidation, assertion/qualifier and CLI graph contracts; `src/graph/{types,query,traverse,rank}.rs`, `tests/graph_queries.rs`, and `tests/graph_cli.rs`. Canonical endpoint direction, literal decimal strings, negation/modality/date/unit qualifiers, recorded assertion paths, evidence ownership/span/hash fields and separately typed navigation are retained. Filter predicates precede SQL limits. Historical descriptions require explicit inclusion and retain eligibility; proposals and index snapshots carry no assertion citations. Expanded assertions have one best parent vote rather than a degree bonus. Cursor bindings include query, normalized plan and pinned generation/snapshot.

## Findings

**R1 — blocking, per-seed incident cap reset at each node.** `walk` takes `incident_per_seed` from each frontier's adjacency and initializes a fresh navigation counter for each frontier. A single originating seed can exceed its configured incident bound across depth two. Existing hub test checks only the global 128 bound. Disposable CLI reproduction: proposed `zz_ab: A→B`, `aa_bc: B→C`; `graph neighbors A --include-proposed --depth 2 --incident-per-seed 1 --limit 50` returned both assertions at hops one/two, `visited_assertions:2`, all omission counts zero. A shared distinct admission budget per originating seed must span typed assertions and navigation across all depths, with direct relationship seeds and repeated edges accounted explicitly. Root/worker correction pending.

**R2 — blocking, final direct-assertion ordering loses fused seed rank.** `assertion_order` compares minimum named channel rank, then ID; multiple channels can each have rank one. Disposable CLI reproduction: proposed assertion `zzz_target` titled `Other title` and assertion `aaa_title` titled `zzz_target`, with the same valid entity endpoints. Relationship query `zzz_target` returned seeds `[zzz_target,aaa_title]` but assertions `[aaa_title,zzz_target]`. Thus a one-hit page can omit the exact ID match ahead of an exact title match. Preserve the ordered direct seed rank separately from named lexical/lookup contributions and RRF votes. Root added a shared `direct_seed_rank` field; worker correction pending.

**R3 — defense, historical assertion authority.** The citation predicate accepts any assertion eligibility when historical inclusion is enabled. Invalid assertions should explicitly fail assertion citation authority. Current projector propagation already suppresses evidence: an invalid dependency on the bootstrap forward assertion returned the Invalid assertion and Invalid evidence, all citations null. No reachable escalation reproduced. Preserve historical Unsupported assertions with intact Withdrawn evidence (the required source-withdrawal case); do not replace this defense with a blanket Current-only citation rule.

**R4 — blocking, navigation hub consumes the global cap before another seed.** Navigation iterates and exhausts each frontier's list, whereas typed expansion uses round-robin lists. Two active entity seeds titled `Anchor`, IDs `aaa_hub` and `zzz_small`, respectively link three reviewed pages and one distinct reviewed page. `graph query Anchor --strategy entity --navigation --assertions 2 --limit 50` returned both seeds, but only `aaa_hub→n1` and `aaa_hub→n2`, `omitted_navigation:2`; the smaller seed got no turn. The corrected R1 source still had this nested sequential navigation loop at inspection. Navigation must share fair round-robin admission as well as incident/global limits. Root/worker correction pending.

## Evidence and limits

Reviewer ran four disposable existing-debug-binary probes, no Cargo: R1, R2 and R4 reproduced with exit zero; R3 invalid-dependency probe returned null citations. Binary SHA256 `cbaaa11e81a36eca7d7cbc6e57fec68b84ed0cf83ede30546f6ffbbb05aff983`. An earlier R1 probe with ascending edge IDs returned one edge because the already visited back-edge occupied the node-local cap; the descending IDs above isolate the defect. No real vault, provider, helper or model calls. Existing binary probes do not prove final source runtime behavior; root owns final compilation/test evidence. Native power loss, other platforms and live providers remain outside this review.

Initial reviewed SHA256s (not final corrected tree):

| File | SHA256 |
|---|---|
| `src/graph/types.rs` | `91294278a38154e7c0eec761821f046c56c5ccff8d4938c6ae2d9c5348526bb6` |
| `src/graph/query.rs` | `29aac7ece437aa16937ff9fed9a29e9b1705e0701f96ff7b09bf93243f2de13b` |
| `src/graph/traverse.rs` | `55ee7729422c49558351fb9443ea3be717f866004aa85eaeffdee05d06ec9701` |
| `src/graph/rank.rs` | `321e895e99a0c9e1863a76ca813a4a279497c7b818017bf30c985dbd5d0a8ae8` |
| `tests/graph_queries.rs` | `71edcfc438b321bffe5e4b4648ef37453f71659c332ef976f2538deeaf682d37` |
| `tests/graph_cli.rs` | `09acdc495c9aaf2538fa2b6a6eaa2ba25a1a0f99530c2017d8b98993dd4a8a6b` |

Types hash includes root's just-added direct-seed-rank field; traverse/rank still showed R1/R2 behavior at inspection. Root reports its initial 85-test gate passed before these findings; that pre-fix gate is not acceptance. Corrected final source hashes and targeted regression evidence require re-review after worker handoff. P09 implementation remains dependency-blocked until P08 acceptance.

## Corrected source re-review

Worker's stable corrected traversal uses one admission count per originating seed across all depths/categories, charges direct relationship seeds once, and keeps distinct per-seed typed/navigation visit sets. An edge already admitted globally still consumes a new contributing seed's slot. Rejected navigation does not enter the seen set, permitting another seed to admit it; final omission sets remove eventually admitted edges. Typed and navigation candidate lists now rotate nodes within each original seed and take one candidate per seed per round. Navigation depth omissions set `depth_limited` without reporting an already admitted backlink as new omitted coverage.

Direct assertions retain `direct_seed_rank` from ordered seeds; the comparator uses this metadata without inserting an extra ranking vote. Expanded-only assertions still have one best parent contribution, update their recorded path under a later better parent and propagate through that parent's queue. The citation predicate explicitly excludes Invalid assertions while preserving Unsupported withdrawal history.

Six concrete added regression tests cover branched depth-two per-seed limits and direct seed charging, navigation limits across hops and rejected-edge admission by another seed, exact-ID first-page ordering with cursor continuation, Invalid historical citations, navigation round-robin/depth labels, and depth-two origin fairness where repeated visits do not consume turns. Existing path/direction/literal/negation/date/dispute, homonym, current-description exclusion, filtered candidates, zero-model rebuild equivalence and best-parent regression coverage remain. No new blocker found in the corrected source.

Final corrected source SHA256s at stable handoff:

| File | SHA256 |
|---|---|
| `src/graph/types.rs` | `91294278a38154e7c0eec761821f046c56c5ccff8d4938c6ae2d9c5348526bb6` |
| `src/graph/query.rs` | `29aac7ece437aa16937ff9fed9a29e9b1705e0701f96ff7b09bf93243f2de13b` |
| `src/graph/traverse.rs` | `6dcb3ae212e2e946dba2c10d0a7d8ea4946f6a1498969233e6d897e09546d6cd` |
| `src/graph/rank.rs` | `4596990f12568524e306bf5ca2c7f02abd0ff7addd784d6fa1b36727a1da0d48` |
| `tests/graph_queries.rs` | `a215f80c56312ac0ca1235cc9d269a9619890b9a70e05b554a844b414237131e` |
| `tests/graph_cli.rs` | `09acdc495c9aaf2538fa2b6a6eaa2ba25a1a0f99530c2017d8b98993dd4a8a6b` |

Worker-reported corrected gate (reviewer did not run Cargo): `graph_queries`17/17 in5.70s, `graph_cli`3/3 in1.34s, all-target Clippy with `-D warnings` passed in1.90s. Reviewer independently reread the final skip-until-admitted origin queues and sixth fairness regression; source hashes above match worker handoff. The queues rotate original seeds and node branches, skip repeated/rejected candidates without consuming an admission turn, and break after one admitted candidate per seed. No remaining blocking review finding. Broader root tests/format/acceptance evidence belongs to the root integration report.
