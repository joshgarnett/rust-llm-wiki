# Complementary context evaluation datasets

Keep QASPER for long scientific documents and add ConditionalQA, WixQA ExpertWritten, MuSiQue-Full and a small versioned developer-documentation suite. Use ContractNLI as an auxiliary test of exceptions, contradiction and missing information. These address different weaknesses; a larger QASPER sample alone does not establish multi-source task completion or revision correctness.

This is a dataset acquisition and adapter proposal, researched on 2026-10-03. The retained samples are **development data**, not acceptance results. No CLI evaluation, embedding request, answer generation or paid API call was performed in this research. Follow [the context protocol](evaluating-context.md) and [the critic workflow](testing-usability.md) when implementing adapters and evaluating returned evidence.

The [evaluation datasets quick reference](eval-datasets-quick-reference.md) consolidates the complete recorded catalog, including subsequent development evaluations, considered alternatives, operational fixtures and current acceptance gates.

## Recommended mix

| Dataset | Contribution | Gold supervision | Practical first experiment |
| --- | --- | --- | --- |
| ConditionalQA | Conditions, exceptions, scenario-dependent answers and natural missing information | Answer/condition sets and evidence strings | 24 train questions, eight each with conditions, without conditions and unanswerable; expand by source family |
| WixQA ExpertWritten | Real support questions and instructions combining articles | Expert answer and required article IDs; no gold byte spans | 22 development questions across one, two and three-plus articles; independently annotate required propositions before runs |
| MuSiQue-Full | Two-to-four-hop chains and paired missing-bridge negatives | Answers/aliases, supporting paragraphs, decompositions and answerability | 12 question pairs per hop count, isolated instance corpora; retain both members of every pair |
| ContractNLI | Long document exceptions, explicit contradiction and absence | Three-way labels and noncontiguous evidence spans | 12 train contracts retained; sample tasks by label without letting 17 repeated hypotheses dominate the aggregate |
| Cargo documentation snapshots | CLI flags, configuration precedence, code examples and revision changes | Source Markdown; independent questions/labels still required | Two immutable releases, five documents each; author 24 realistic tasks plus refresh/withdrawal controls |

These are proposed experiments, not declared acceptance denominators. Freeze concrete source families, task counts and thresholds before exposing a new holdout. Report each category independently and preserve the existing HIGH gate; do not use the large ContractNLI task count to mask a failing multi-source category.

**Implementation priority:** first adapt ConditionalQA's scenario/evidence/condition mapping and ContractNLI's three-way labels, then add the independently labeled WixQA and scoped MuSiQue arms. The [ContractNLI adapter](evaluating-contractnli.md) now prepares an evaluator-compatible, train-only fixture from the pinned archive. The original research sample directories still use research manifests; use the supported adapter's output for evaluation. ConditionalQA's first adapter exposed HTML-source rendering and full-scenario query issues in actual CLI runs; the [revised adapter](evaluating-conditionalqa.md) preserves mapped Markdown and full queries. WixQA is deliberately raw corpus plus external expert answers awaiting independent proposition/span labeling. MuSiQue data acquisition and Cargo question authoring remain pending.

## Additional acquired development inputs

The following inputs were acquired and structurally audited on 2026-10-03. They broaden evaluation capabilities without changing the frozen acceptance set. Downloaded files, original notices and acquisition failures remain local under `.artifacts/eval-2026`; no result below is a retrieval or semantic-completeness score.

| Input | Retained data | What it can test / remaining work |
| --- | --- | --- |
| [T²-RAGBench](https://aclanthology.org/2026.eacl-long.8/), FinQA development subset | 883 questions and 299 distinct page contexts, containing 1,339,673 UTF-8 source bytes | Text/table retrieval, entity/year disambiguation and numerical evidence; independently audit question reformulations and required operands/qualifiers before grading completeness |
| [BRIGHT](https://huggingface.co/datasets/xlangai/BRIGHT), Stack Overflow, economics and Pony subsets | 332 queries, 165,195 short documents and 2,951 long documents; all referenced gold IDs resolve | Reasoning-intensive retrieval, code/documentation and cross-domain transfer; relevance labels are not sufficient-passage labels |
| [DevDex](https://github.com/firecrawl/benchmark-devdex), public release | 594 tasks: 201 documentation, 195 issue/PR and 198 repository lookups | Paraphrased developer search and URL/reference ranking; source pages need immutable acquisition and independent supporting-span labels before local context evaluation |

T²-RAGBench was published at EACL 2026; its questions are model-reformulated, with only a subset checked by people. Its current card explicitly removes VQAonBD for poor reformulations and contains conflicting aggregate counts, so use counts from the retained bytes. The downloaded FinQA development file has no conflicting text for a shared `context_id`. Its released `context` strings are already text plus Markdown tables: source preparation preserves those bytes, keeps questions/answers outside the corpus, and treats each as a page context, not an entire financial report. The first train-file request was rejected before transfer because its advertised size exceeded the declared 50 MB cap; that failed attempt is retained rather than silently expanding the download.

BRIGHT's short and long document sets are alternative retrieval settings, not independent source samples. The largest downloaded long document is 9,188,634 bytes; validate ingestion and context budgets before importing it, and never silently trim it. Keep `reasoning`, `gold_answer`, gold IDs and query exclusions outside indexed content. The official card declares CC BY 4.0, as does T²-RAGBench; preserve upstream source notices as well. DevDex's repository carries MIT, but linked external pages retain their own terms. Its August 2026 release measures reference retrieval, not answer completeness, and its vendor-authored comparisons do not establish an independent product-quality target.

Acquisition retained 17 successful files totaling 114,587,939 bytes, plus metadata manifests and the bounded train-file failure. BRIGHT schema/reference auditing used isolated PyArrow 21.0.0 without changing project dependencies. No upstream benchmark code, provider API or model was run during these checks. The existing context evaluator needs dataset-specific adapters and independently frozen sufficient-evidence labels before these inputs can contribute complete-task scores.

Essential immutable inputs (download into new paths, retain README/license files, then verify SHA256):

| File | Bytes | SHA256 |
| --- | ---: | --- |
| [T² FinQA dev](https://huggingface.co/datasets/G4KMU/t2-ragbench/resolve/adf7fe1541ac37351ce1142544d8e3b43010ed92/data/FinQA/dev/metadata.jsonl) | 8,604,079 | `24048e3250d4178c7fb433aea21439dca23f62abbcbd00d415d9798902a544a5` |
| [BRIGHT Stack Overflow queries](https://huggingface.co/datasets/xlangai/BRIGHT/resolve/3066d29c9651a576c8aba4832d249807b181ecae/examples/stackoverflow-00000-of-00001.parquet) | 250,458 | `97d417ba449ef70c9c9ae2937e9df106654a2554ce1533b090cb64b998a077e1` |
| [BRIGHT economics queries](https://huggingface.co/datasets/xlangai/BRIGHT/resolve/3066d29c9651a576c8aba4832d249807b181ecae/examples/economics-00000-of-00001.parquet) | 219,518 | `2a79f0f3a881c7c03a258cf8ef8ac2db1ca9080963252d9a020bb45a264aa037` |
| [BRIGHT Pony queries](https://huggingface.co/datasets/xlangai/BRIGHT/resolve/3066d29c9651a576c8aba4832d249807b181ecae/examples/pony-00000-of-00001.parquet) | 27,722 | `0c0718d3e0ef05da42f75b7c03e755d3e03d9fcdbd32b67dddd221768a8377d7` |
| [DevDex documentation tasks](https://raw.githubusercontent.com/firecrawl/benchmark-devdex/24e60473887d33960bf155a9e73affcd07d288a3/devdex/gt/docs_public.jsonl) | 209,709 | `7bf25a95911b23cba828efc6c4920a65673d63d4233d910773c1dfd15e641b69` |

At the same BRIGHT revision, replace `examples/` with `documents/` or `long_documents/` for the corresponding source files. At the same DevDex revision, `fix_public.jsonl` and `repo_public.jsonl` provide the other tracks. Use explicit download deadlines and byte caps; the completed acquisition used at most 50 MB per file and 200 MB total. Do not execute a release's model runner merely to obtain its data.

### ConditionalQA: preserve the conditions

The [original paper](https://aclanthology.org/2022.acl-long.253/) describes conditional answers over government documents. The retained train release contains 2,338 questions and the document release contains 652 documents. Structural counts are 550 questions with answer conditions, 1,697 answerable questions without conditions and 91 unanswerables. The adapter must pass the **scenario and question together**; the short question can omit decisive user facts.

The [project site](https://haitian-sun.github.io/conditionalqa/) declares the dataset CC BY-SA 4.0. The pinned repository's [LISCENSE file](https://raw.githubusercontent.com/haitian-sun/ConditionalQA/77bd295952daf415548b3244db10880d3d55cfe0/LISCENSE) is instead BSD-2-Clause software text attributed to Bhuwan Dhingra in 2017. Retain both notices and use the explicit dataset declaration for the local research copy; do not claim that the repository file licenses the government text or removes share-alike obligations. Preserve GOV.UK attribution and original source URLs. The snapshot is historical research material.

The [official evaluator](https://raw.githubusercontent.com/haitian-sun/ConditionalQA/77bd295952daf415548b3244db10880d3d55cfe0/evaluate.py) separates answer and condition scores. Its missing-prediction branch sets zero values but does not append them to the arrays later averaged. A missing output can consequently disappear from the denominator. Use an independently checked complete denominator and explicit failed-case zeros instead of treating this script as an acceptance gate. Matching the answer string without its applicability conditions must fail whole-task completeness.

### WixQA: realistic technical support

The [original paper](https://arxiv.org/abs/2505.08643) releases a knowledge-base snapshot with expert, simulated and synthetic questions. The [publisher's dataset card](https://huggingface.co/datasets/Wix/WixQA/blob/d662dc42479c14e202eccd832f8c4b66a035c4cc/README.md) identifies the snapshot as 2024-12-02 and declares MIT. The pinned tree has no standalone LICENSE file; retain the card and paper's explicit release notice, and do not invent additional license text.

The acquired corpus has 6,221 articles and ExpertWritten has 200 questions. Actual article-ID counts are 148 single-article, 46 two-article and six three-plus-article questions, with every referenced ID present. The paper reports 6,222 synthetic pairs whereas this release's card/corpus count is 6,221; use retained bytes rather than assuming those versions are identical. Prioritize ExpertWritten; simulated and synthetic queries introduce model-authored distributions.

Article IDs establish document relevance, not complete supporting passages. Independently decompose each expert answer into requested steps, prerequisites and restrictions, then locate supporting text in the frozen corpus. Keep correct alternatives possible, including evidence from an article not in the original ID list. An article may contain scraped text with damaged heading/list spacing; label transformation failures and compare a separately declared formatting-preserving adapter rather than quietly repairing only hard cases.

### MuSiQue-Full: missing a bridge must matter

The [original paper](https://aclanthology.org/2022.tacl-1.31/) targets compositional multi-hop reasoning. The [author repository](https://github.com/StonyBrookNLP/musique) declares CC BY 4.0 and explicitly warns that seed single-hop datasets can leak dev/test constituents. Retain its released constituent-ID exclusion list when acquiring the full dataset. Existing model pretraining exposure cannot be ruled out.

The [official evaluation source](https://raw.githubusercontent.com/StonyBrookNLP/musique/922ac98f19a201998dbdae6d7f2887a5258dbdeb/evaluate_v1.0.py) requires two rows per question ID for Full's grouped sufficiency evaluation. Give local records an instance suffix while keeping the shared pair/family ID. Never split a pair. Supporting paragraph recall and answer overlap remain diagnostics alongside the independent chain-completeness judgment.

The [official download script](https://raw.githubusercontent.com/StonyBrookNLP/musique/922ac98f19a201998dbdae6d7f2887a5258dbdeb/download_data.sh) points to Google Drive file `1tGdADlNjWFaHLeZZGShh2IRcpO6Lv24h`, named `musique_v1.0.zip`. Only metadata, license and evaluator source were acquired here. The archive's size/hash and data schema beyond the published evaluator are **not locally verified**. The script installs `gdown` and extracts automatically; do not execute it for bounded acquisition. Download to a new local path with an explicit cap/deadline, hash first, and inspect archive members without executing upstream code.

Most importantly, do not flatten Full into one global wiki: another instance can restore a removed bridge and invalidate the negative label. Use a disposable vault per instance, containing that instance's provided support/distractor paragraphs only. A separately curated global multi-source experiment needs a new answerability audit across its complete corpus. Question decomposition, bridge answers and `is_supporting` labels stay outside indexed content.

### ContractNLI: distinguish contradiction from absence

The [official project](https://stanfordnlp.github.io/contract-nli/) provides 607 NDAs, 17 fixed hypotheses and Entailment, Contradiction and NotMentioned labels under CC BY 4.0. Evidence exists for entailment/contradiction, with sentence/list-item character spans; the hypotheses repeat across contracts. This is a useful exception/negative control, not evidence of broad natural-question diversity.

The official archive includes JSON text plus original documents. Use JSON text only for this Markdown benchmark. Preserve its exact text and convert Unicode character endpoints to UTF-8 byte offsets. NotMentioned is not Contradiction, and returning no passage does not prove absence. Scope each task to its designated contract. Evidence annotations can include redundant supporting spans: report exact gold coverage separately from a critic's sufficient-evidence judgment. Contract/template families, not hypothesis rows, must determine heldout grouping.

### Developer and revision tasks: Cargo

[Cargo's repository](https://github.com/rust-lang/cargo) is primarily MIT OR Apache-2.0. Both license files and immutable source Markdown were retained for tags 0.84.0 and 0.85.0, resolved to commits `5ffbef3211a8c378857905775a15c5b32a174d3b` and `66221abdeca2002d318fde6efff516aab091df0e`. Five files per release cover configuration, features, resolution, build and test. This is source material for a new benchmark, **not an existing QA dataset**.

The pinned configuration files differ: the newer snapshot adds documentation for `resolver.incompatible-rust-versions`, including values, environment configuration and overriding options. Absence from the older documentation establishes only absence in that snapshot; it does not prove the executable lacked a feature. Have an independent author label documentation-scoped questions, combinations across files and realistic paraphrases. Include exact flags/identifiers, negation, ordered instructions and platform qualifiers. Many Markdown links leave this five-file subset; either acquire the referenced documents before freezing or treat the dependency as explicitly missing.

For freshness, first index the older bytes, replace a source with the newer bytes, and assert a new immutable revision plus current citations. Also test withdrawal and an intentionally retained historical source. Old and new releases under different source IDs test conflict/scope selection; replacing one source tests freshness. Keep those measurements separate. Author disagreement/precedence fixtures independently when natural release changes do not supply a genuine conflict.

## Adapter and scoring plan

1. Produce full source Markdown, a byte-level source map, provenance/notice files and external question/answer/evidence labels. Store all labels outside document directories. Validate every transformed span against exact source bytes. Record ambiguous repeated evidence as alternative locations, and retain all unmapped records with reasons.
2. Use the current evaluator's `required_evidence_all_of` groups across document IDs. It already accepts multi-document evidence, but `doc_id`, source rank and source hit describe one anchor. Add/declare all-required-source coverage rather than calling an arbitrary anchor hit multi-source success. Do not concatenate several sources into one artificial document to evade this distinction. `--paper-local`/`--source-id` cannot represent a multi-document allowlist.
3. ConditionalQA: render scenario plus question, map every evidence string and retain answer/condition associations. An adapter can initially retain HTML-like paragraph/list strings with blank-line boundaries; a cleaner Markdown transformation needs a separately verified map. Freeze alternate sufficient sets before scoring. ContractNLI: render a labeled documentation-scoped classification query and retain all three classes externally; preserve separate redundant-gold and semantic-support scores.
4. WixQA: article labels are insufficient for strict span metrics. Add independent proposition/span annotations before calling it a completeness benchmark. Until then, leave semantic/span completeness unavailable rather than using whole articles as fabricated gold. MuSiQue: preserve the reasoning chain and pair scope; each required hop must be supported in returned cited text.
5. Score citation/revision/budget correctness, location coverage, semantic proposition coverage and whole-task completion separately. For conditions and instructions, complete credit requires every requested step and qualifier. Penalize unsupported/contradicted facts even when answer overlap is high. Negative cases measure false support and misleading answerability claims; lwiki's context-only output currently has no abstention decision, so an empty result cannot earn an abstention score.
6. Use the same frozen binary, source revisions, query caches, modes and context limits in every paired comparison. All processing failures remain in denominators. Report candidate discovery, selection and final packing separately so discarded evidence is attributable to a stage. Model-assisted selection/answering requires its own predeclared protocol and accounting; no dataset justifies conflating that workflow with deterministic context.

## Retained development samples and limits

Ignored local artifacts live under `.artifacts/eval-datasets-additional/`. `acquisition.json` and `acquisition-supplement.json` record exact URLs, SHA256, sizes, timestamps, download caps and failures. The first ContractNLI attempt stopped at its 50 MB cap; a second attempt followed independent verification of its 65,362,913-byte size and used a 70 MB cap. Neither failure nor changed bound was discarded.

`development-samples/` contains external raw labels, source-only `.md` files, per-file hash manifests and notices. `prepare_samples.py` selects by SHA256 of `lwiki-additional-dev-v1:<record-key>` within structural strata, without retrieval outcomes. It is a local research helper, not a supported repository adapter. Samples contain only required documents; **they omit global distractors and cannot demonstrate global retrieval relevance**. The complete downloaded corpus remains available for later declared experiments.

| Retained sample | Questions/tasks | Full source documents | Verified mechanics |
| --- | --- | --- | --- |
| ConditionalQA train | 24: eight per declared stratum | 24 | All 74 evidence strings and 13 condition strings occur literally in retained sources |
| WixQA ExpertWritten | 22: eight single, eight two, six three-plus | 42 | All 200 upstream questions reference existing article IDs |
| ContractNLI train | 204: 96 entailment, 26 contradiction, 82 missing | 12 | All 826 sample character spans and annotation indices in bounds |
| Cargo | No QA labels prepared | Ten files across two releases | Source/license bytes hash-pinned; release tags resolved to commits |

These checks establish alignment and acquisition mechanics only. Semantic annotations, parser rendering, CLI retrieval, revision transitions and critic acceptance remain untested. No existing private critic questions were accessed.

## Reproduce essential acquisition

These immutable pins and hashes allow a fresh checkout to reacquire the essential inputs without the ignored reports. Download each direct URL into a new research directory using `curl --fail --location --max-time 60 --max-filesize <cap> --output <new-file> <url>`, then verify SHA256 before parsing. Use 10,000,000-byte caps for ConditionalQA, 60,000,000 for Wix corpus, 1,000,000 for Wix questions/Cargo and 70,000,000 for ContractNLI. Public downloads require no model API credentials. Metadata pages are moving snapshots; preserve their exact acquisition bytes and notices when reproducing a prior run.

| File / direct pinned download | Bytes | SHA256 |
| --- | ---: | --- |
| [ConditionalQA train](https://raw.githubusercontent.com/haitian-sun/ConditionalQA/77bd295952daf415548b3244db10880d3d55cfe0/v1_0/train.json) | 2,386,451 | `2976f448bad58efd747c5b7b4127fb3ad7a4834f760e9022c06a86536618bc2d` |
| [ConditionalQA documents](https://raw.githubusercontent.com/haitian-sun/ConditionalQA/77bd295952daf415548b3244db10880d3d55cfe0/v1_0/documents.json) | 6,286,258 | `1c977c1b14738b9ce1e53336c5abf3b3de34b72ee24985e7de6d8fae30856c59` |
| [WixQA expert questions](https://huggingface.co/datasets/Wix/WixQA/resolve/d662dc42479c14e202eccd832f8c4b66a035c4cc/wixqa_expertwritten/test.jsonl) | 255,510 | `a1bb6888a8394f980bc88f0d09b873fcd283ff4c9782600bc203f8103c2f1bad` |
| [WixQA corpus](https://huggingface.co/datasets/Wix/WixQA/resolve/d662dc42479c14e202eccd832f8c4b66a035c4cc/wix_kb_corpus/wix_kb_corpus.jsonl) | 53,743,863 | `46b852c60b85fb1828e7c6118bb56ba796eba99120a6b69c6b984c06e1c5e1fe` |
| [WixQA notice/card](https://huggingface.co/datasets/Wix/WixQA/resolve/d662dc42479c14e202eccd832f8c4b66a035c4cc/README.md) | 4,884 | `440f6c755676869bf57e206cbb455700c133ceda9cabbe228b570ff101d59dd5` |
| [ContractNLI archive](https://raw.githubusercontent.com/stanfordnlp/contract-nli/eced6528dd3c1d14d73f9a87df8f7bdbc03126f9/resources/contract-nli.zip) | 65,362,913 | `e03fc77bbf8b53e2976a250e81d8a294bc3d5e5fb014521e477dee9340d6287b` |
| [Cargo older configuration](https://raw.githubusercontent.com/rust-lang/cargo/5ffbef3211a8c378857905775a15c5b32a174d3b/src/doc/src/reference/config.md) | 49,530 | `2181a41b4cdafc0f70a12b11da5e1ce1f0036f5b8110e9f75ee255333539f500` |
| [Cargo newer configuration](https://raw.githubusercontent.com/rust-lang/cargo/66221abdeca2002d318fde6efff516aab091df0e/src/doc/src/reference/config.md) | 50,659 | `4e8db8d34f4022425ea5abf732fac585141acab9eace09ecd905a6168f208ddf` |

For each Cargo commit, the other source paths are `src/doc/src/reference/features.md`, `src/doc/src/reference/resolver.md`, `src/doc/src/commands/cargo-build.md` and `src/doc/src/commands/cargo-test.md`; also acquire `LICENSE-MIT`, `LICENSE-APACHE` and `README.md`. ConditionalQA's project license declaration and pinned `LISCENSE`, plus ContractNLI's pinned `LICENSE`, must accompany local copies. MuSiQue's metadata pin is `922ac98f19a201998dbdae6d7f2887a5258dbdeb`; its data archive still needs independent acquisition and a new hash record.

## Keep the next holdout unseen

Choose splits before running retrieval. Cluster by source/entity/template family and connected question-support graph; exact document hashes and near-duplicate source text must not cross development/acceptance. Keep answerable/unanswerable pairs together, all 17 hypotheses from a contract together, and Cargo historical/current revisions together. In WixQA, shared article IDs link question families; a row-random split can leak the same instructions. Preserve original upstream split names as provenance while labeling locally inspected public examples development.

Keep this research sample entirely development. The first public Wix row was inspected for schema in addition to the prepared sample; demote its connected source family too. Structural counts across the release are not secret acceptance questions. An independent custodian should select disjoint remaining families, write proposition labels, record hashes and retain questions privately until the binary/protocol/caches/budgets freeze. Public benchmarks are potentially pretrained; combine them with independently authored realistic questions over a new corpus. Opening a holdout to guide a fix consumes it as development data, requiring a replacement holdout and a new freeze.

The ignored `exposure.json` identifies inspected sample records, the first Wix schema row and its transitive shared-article closure. Use that ledger when selecting remaining questions; do not reconstruct exposure from memory. The current evaluator already accepts multiple document IDs in evidence groups. The required extension concerns multi-source task metrics and dataset adapters, not removing a nonexistent single-document evidence-schema restriction.

The retained Wix development exposure connects 47 questions through 46 article IDs. Acceptance families must exclude that closure, not just the 22 sampled rows. `provenance-manifest.json` inventories every local research artifact and all successful acquisitions while retaining the failed bounded attempt; downloaded upstream bytes total approximately 129 MB.

## Lower-priority alternatives

[HotpotQA](https://hotpotqa.github.io/) has sentence-level supporting facts, bridge/comparison questions and a roughly 44 MB distractor dev download under CC BY-SA 4.0. It is a practical fallback if MuSiQue acquisition is blocked, but lacks Full's paired missing-bridge protocol and overlaps the Wikipedia domain. Its ten-paragraph distractor setting is not a full-wiki retrieval result. No Hotpot archive was acquired here.

[TechQA's official repository](https://github.com/IBM/techqa) links a release with more than 800,000 technotes; the [paper](https://aclanthology.org/2020.acl-main.117/) uses actual developer-support questions. This is relevant but acquisition is substantially heavier than WixQA. The repository's Apache-2.0 code license does not establish technote/dataset redistribution terms; data-level terms and archive size were not verified here. Defer bulk acquisition until those are checked.

[FreshQA](https://github.com/freshllms/freshqa) supplies changing-fact questions and dated answer spreadsheets, with Apache-2.0 repository licensing. It does not supply the frozen evidence corpus needed for this local wiki benchmark. A new evidence collection would need timestamped provenance and scope-specific labels; do not replace local revision tests with live web freshness scores. Its spreadsheet-specific licensing was not separately verified, and no spreadsheet was downloaded.
