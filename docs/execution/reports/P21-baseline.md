# P21 fixed-corpus retrieval baseline

Status: historical targeted baseline and final matching baseline passed; P21 locally accepted; external qualification pending. Final two parents/one explicitly invoked helper passed in 12.25s, log `/private/tmp/lwiki-p21-final/tests.log`. The separately retained [final artifact](P21-baseline-final-results.json) records matching source qualification; the table below preserves the earlier targeted measurements.

Command: `LWIKI_P21_EVIDENCE=/private/tmp/lwiki-p21-retrieval-baseline.json cargo test --locked --offline --test retrieval_baseline -- --test-threads=1 --nocapture`. Exit0, two parents passed/one explicit measurement child ignored by ordinary discovery and invoked48times by its enabled parent; build1.70s/tests14.62s. Log `/private/tmp/lwiki-p21-baseline-current-first.log`. The committed [measurement artifact](P21-baseline-results.json) records every query/path's quality, citation checks, omissions, process-cold/warm timing, child peak RSS, logical disk and zero provider usage. Earlier placeholder-pin failure and pre-final early binary checks remain historical feedback, not qualification.

Hardware: MacBookPro18,2, Apple M1 Max,32GiB; macOS26.5.2/Darwin25.5.0 arm64, Rust1.98.0. CorpusBLAKE3 `76e9b46977321f65a547019d062e864e731dbbe06bc1be6bba6995eb410c9e1b`;33 byte-exact bootstrap files plus one Rust identifier page. Eight questions/six paths use identical k10/6000-byte/1000-estimated-token budgets and fixed held-out labels.

| Path | Cold median ms | Warm median ms | Peak RSS MiB | Max logical disk bytes |
|---|---:|---:|---:|---:|
| literal | 94.826 | 91.202 | 26.23 | 826994 |
| lexical | 96.277 | 91.863 | 26.78 | 826994 |
| exact_dense | 107.931 | 103.835 | 26.78 | 859762 |
| entity | 98.16 | 94.558 | 26.56 | 826994 |
| relationship | 97.358 | 91.837 | 26.36 | 826994 |
| hybrid | 113.2 | 108.986 | 27.2 | 859762 |

All48 pairs had zero citation resolution errors and zero provider requests/tokens/spend. Exact Current hashes/spans/propositions/qualifiers, separate homonyms, equal-dimension model isolation and one/all support withdrawal assertions passed. Per-method/class Recall/nDCG, evidence-set recall and supported bundle precision are in the artifact; graph/document relevance sets differ and nulls mean inapplicable. This does not score generated answers or establish semantic entailment.

Synthetic hash-derived cached vectors are independent of labels; they prove cosine/space/fusion mechanics, not real semantic quality or ranking gains. No tuning or quality/performance thresholds were invented. Process-cold uses a fresh process and prebuilt cache; OS page caches were not flushed. RSS includes setup/retrieval/validation; disk is logical bytes including transient SQLite files. No live provider, host installation, other-native-platform durability or power-loss claim follows.
