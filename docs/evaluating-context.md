# Evaluating retrieved context

The offline [context evaluator](../scripts/context_eval.py) measures whether retrieved citations cover annotated evidence locations. It does not generate answers or judge semantic completeness. A correct source hit and an exact citation can still omit the facts a user needs. Use an independent critic or human assessment alongside its scores, following [testing-usability.md](testing-usability.md).

The [complementary dataset plan](eval-datasets.md) adds conditional answers, multi-source technical support, missing reasoning steps, contradiction and versioned CLI documentation. It records pinned downloads, licensing, adapter requirements and which samples are development data. Downloaded samples do not count as executed or accepted evaluations.

For the user workflow and copyable commands, see [selecting context with a host agent](context-selection.md).

The [ContractNLI adapter](evaluating-contractnli.md) adds designated-document evidence tests for support, contradiction and missing information. Its exhaustive annotation coverage is separate from semantic completeness and three-way classification accuracy.

## Evidence sufficiency research and additional tests

Research checked on 2026-10-03 reinforces the distinction between relevant sources and enough evidence to answer. These methods inform prospective development tests; they do not change the frozen [acceptance gate](rag-quality-targets.md).

| Primary source | Useful evaluation idea | Limitation |
| --- | --- | --- |
| [RINSE, September 2026 preprint](https://arxiv.org/abs/2609.37469v1) | Remove or substitute required evidence while holding the question and topic fixed; assess sufficiency before generation | Paired ranking accuracy is not a calibrated probability; code availability was not verified |
| [Distribution-shape QPP, September 2026](https://arxiv.org/abs/2609.11646v1) | Compare retrieval-score diagnostics with a content judge, including calibration and domain transfer | A self-sufficient page hit does not establish complete multi-source evidence; transfer weakens the reported predictor |
| [SURE-RAG, July 2026 revision](https://arxiv.org/abs/2605.03534v2) | Separate supported, refuted and insufficient cases; audit counterfactual shortcuts | Requires a candidate answer, so it evaluates answer verification rather than pre-generation sufficiency |
| [S2G-RAG, ACL 2026](https://aclanthology.org/2026.acl-long.1185/) | Record missing facts explicitly and select original sentence indices | Supporting-document coverage is weaker than emitted-span sufficiency; iterative calls add cost |
| [TREC RAG 2026 / RAGDoll](https://trec-rag.github.io/) | Separate source relevance, vital-fact coverage and citation support | Current track results/judgments are pending; automatically generated fact rubrics need independent auditing |

For additional corpora, freeze required facts independently of the retrieved output, splitting by document or repository family. Construct complete, partial, missing and conflicting evidence with matched topic, length and metadata; shuffle evidence positions and add nearby versions or irrelevant padding. Numerical/table cases must preserve operands, row/column headers, units, entity and year. Correct arithmetic is a separate outcome from sufficient input evidence.

Report per-fact coverage and strict all-facts success, false sufficiency, unsupported cases, distracting content and exact citation integrity separately. Diagnose the first loss across discovery, packet construction, selection and final packing. Compare declared 6 KB, 12 KB and 16 KiB budgets with identical sources, caches and proof allowances; distinguish rendered evidence, full JSON and aggregate model input, and retain failures and unavailable usage.

Calibrate any automated judge against separate, independently adjudicated labels before using its scores as a gate. [ARES](https://aclanthology.org/2024.naacl-long.20/) motivates human-label calibration and corrected aggregate estimates; [Sufficient Context](https://research.google/pubs/sufficient-context-a-new-lens-on-retrieval-augmented-generation-systems/) separates evidence adequacy from answer correctness. Neither makes a selector's self-grade independent evidence. Keep discrimination, calibration and threshold-specific errors distinct; no paper or vendor percentage establishes a universal quality ceiling.

## Public baseline and retained inputs

The first public baseline is QASPER v0.3: natural questions over long NLP papers, reference answers, paragraph evidence, selected sentence highlights and unanswerable annotations. [The original paper](https://aclanthology.org/2021.naacl-main.365/) explains the collection; [the official dataset card](https://huggingface.co/datasets/allenai/qasper) specifies CC BY 4.0. Paper text was extracted from S2ORC; retain upstream notices and attribution when copying artifacts. Figure/table images are outside this text benchmark.

The research run retained official train/dev and test/evaluator archives under `.artifacts/eval-research/upstream`, with their hashes in `.artifacts/eval-research/qasper-mini-v1/manifest.json`. The smaller sample has four train papers and 13 questions, four dev papers and ten questions, and eight test papers and 26 questions. The train/dev set includes two natural unanswerables; the public test includes one. The dev sample has no unanswerables, so it cannot establish negative-control behavior. Full documents, questions, byte-aligned labels and explicit exclusions are retained. Large upstream archives and source samples stay outside Git.

Selection sorts original paper IDs by SHA256 of `lwiki-qasper-text-v1:<paper-id>` and takes fixed counts. It is independent of retrieval outcomes. The tracked [preparation script](../scripts/prepare_qasper.py) reproduces samples from locally downloaded, hash-pinned archives without network calls. The larger `qasper-text-v1` research sample has 12/24/24 papers for train/dev/test. Neither sample is the private critic holdout.

## Acquire and prepare from a fresh checkout

Download the original v0.3 releases with explicit size/deadline bounds. These are unauthenticated public dataset downloads; no embedding provider is involved. On a fresh checkout, the paths below are new. If retaining a previous acquisition or dataset, use a new directory and pass its paths explicitly; preparation refuses an existing output directory.

```sh
mkdir -p .artifacts/eval-research/upstream

curl --fail --location --max-time 60 --max-filesize 50000000 \
  --output .artifacts/eval-research/upstream/qasper-train-dev-v0.3.tgz \
  https://qasper-dataset.s3.us-west-2.amazonaws.com/qasper-train-dev-v0.3.tgz

curl --fail --location --max-time 60 --max-filesize 50000000 \
  --output .artifacts/eval-research/upstream/qasper-test-and-evaluator-v0.3.tgz \
  https://qasper-dataset.s3.us-west-2.amazonaws.com/qasper-test-and-evaluator-v0.3.tgz

curl --fail --location --max-time 30 --max-filesize 1000000 \
  --output .artifacts/eval-research/upstream/qasper-dataset-card.md \
  https://huggingface.co/datasets/allenai/qasper/resolve/main/README.md

python3 scripts/prepare_qasper.py \
  --train-dev-archive .artifacts/eval-research/upstream/qasper-train-dev-v0.3.tgz \
  --test-archive .artifacts/eval-research/upstream/qasper-test-and-evaluator-v0.3.tgz \
  --dataset-card .artifacts/eval-research/upstream/qasper-dataset-card.md \
  --dataset qasper-mini-v1 --papers 4 4 8 \
  --output .artifacts/eval-research/qasper-mini-v1
```

The preparation script verifies both archive hashes before creating output. These pins are part of the tracked script, so a fresh checkout does not depend on an ignored research manifest:

| Official archive | SHA256 |
| --- | --- |
| `qasper-train-dev-v0.3.tgz` | `a28fdf966db827bcee3d873107d6b6669864fb7ca8fbf73a192f5e39191bdb5a` |
| `qasper-test-and-evaluator-v0.3.tgz` | `72a52a41193e2838b8074f80ac074b94f956b84886c36a61c58a7df4171bdd72` |

The official dataset card uses a moving `main` URL; preparation retains the supplied bytes and their SHA256 rather than implying that URL is immutable. Its original research snapshot SHA256 was `da348d3f8277f8a93ca397941f6a525433e282858b1c0562aed3f667d8e55b05`. The card is optional for offline preparation; the manifest and generated `notices/ATTRIBUTION.md` always preserve the official CC BY 4.0 notice, license link, paper-text origin, citation and transformation description. Original archive READMEs are copied into `notices/`; the upstream answer-generation evaluator remains in its archive and is never executed.

Preparation defaults to four train, four dev and eight test papers, the conventional archive paths, and the actual current UTC acquisition date. Set `--acquired-date` to the recorded acquisition date when reconstructing retained inputs, and retain the same card snapshot for byte-identical manifests. Use `--papers 12 24 24 --dataset qasper-text-v1 --output .artifacts/eval-research/qasper-text-v1` for the larger sample. The tracked adapter adds self-contained notices/provenance to its manifest; source Markdown, questions, exclusions and evidence offsets are byte-identical to the original research sample for the same logical dataset name/counts. Old mappings remain bound to their original manifest; a newly prepared manifest requires a new import mapping.

## Run against an existing binary

Use absolute paths to a locally built CLI and its `fixture_hash` helper. The helper reads quote bytes from stdin and prints the prefixed BLAKE3 digest. The evaluator builds neither binary. Run from the repository root; choose new wiki/output paths for every import or report.

```sh
python3 scripts/context_eval.py import \
  --binary "$LWIKI" --dataset .artifacts/eval-research/qasper-mini-v1 \
  --split train --split dev --wiki .artifacts/eval-research/wiki-dev-001 \
  --output .artifacts/eval-research/import-dev-001

python3 scripts/context_eval.py evaluate \
  --binary "$LWIKI" --hash-binary "$HASH_BINARY" \
  --dataset .artifacts/eval-research/qasper-mini-v1 --split train --split dev \
  --wiki .artifacts/eval-research/wiki-dev-001 \
  --mapping .artifacts/eval-research/import-dev-001/mapping.json \
  --mode lexical --output .artifacts/eval-research/lexical-dev-001
```

Import refuses an existing wiki or output directory. It imports full documents without questions, answers or evidence labels, then records dataset document IDs against allocated source IDs, immutable revision IDs and vault content paths. Evaluation requires the same document set and unchanged source bytes/current revisions. A failed import retains its partial disposable wiki and logs; retry with fresh paths.

All CLI commands include `--offline`. The evaluator never reads credential files, configures providers, downloads data or dispatches embedding requests. `--mode semantic` and `--mode hybrid` use only existing compatible caches; missing vectors are retained failures or warnings. Inspect those warnings before comparing modes. An authorized operator acquires embeddings separately with explicit finite request/data/cost bounds, then freezes the same immutable corpus and caches for paired comparisons. Mock embeddings exercise mechanics, not real-model relevance.

Defaults are five results, 80 candidates, 1,024 excerpt bytes, 6,000 rendered bytes, 1,500 estimated tokens and 65,536 verification entries. Flags can override these bounds; keep them fixed across baseline/candidate and record changes as a new protocol. The runner records all passed limits. Other verification limits use the tested binary's defaults, whose identity is hashed. Latency is the local subprocess duration, including local sync/verification; cached offline measurements are distinct from cold remote-query latency. Token usage uses the CLI's UTF-8-byte estimate, not an exact model tokenizer.

QASPER assumes a designated paper. Global evaluation defaults to `In the paper "<title>", <original question>` and records this `paper-title-prefix-v1` transformation. For original paper-local evidence selection, add `--paper-local --query-style original`; this adds `--source-id`. Paper-local source hit is trivial and must not be used to claim global retrieval quality. Original question text remains in the labels. Unanswerability is scoped to the designated paper; it is not a claim about all documents or the world.

## Separate host-assisted selection workflow

The explicit `context --prepare-selection` → one external selector → `context --selection FILE` workflow adds a model stage. Declare its protocol before opening acceptance questions. Its score cannot establish that the original deterministic command passed. The evaluator supports preparation and replay; it never invokes a selector, reads credentials or calls a model.

Use a binary that implements these explicit product options. Prepare with the same query transformation, source scope, mode, discovery limits, final context budget and verification limits you will pass during replay:

```sh
python3 scripts/context_eval.py prepare-selection \
  --binary "$LWIKI" --hash-binary "$HASH_BINARY" \
  --dataset .artifacts/eval-research/qasper-mini-v1 --split train --split dev \
  --wiki .artifacts/eval-research/wiki-dev-001 \
  --mapping .artifacts/eval-research/import-dev-001/mapping.json \
  --mode hybrid --verification-max-elapsed-ms 2000 \
  --output .artifacts/eval-research/selection-tasks-dev-001
```

Preparation checks the frozen mapping/current revisions, verified offline response, query/packet binding, input byte estimates and card/owner/excerpt bounds. For full v1 cards, it audits source citations. For compact v2 cards, it checks source-table references, owner consistency and exact displayed UTF-8 text/spans against the frozen sources; it checks the authority commitment's format but cannot independently recompute hidden citation metadata from the compact task. Final returned citations still receive the full audit in both versions. Unknown task versions fail validation. Each `selector-inputs/question-0000.txt` contains **only the product's exact `selector_input` bytes**, with no runner wrapper, expected answers, gold spans, assessment labels or other questions. `tasks.json` maps filenames to question IDs, packet fingerprints, task hashes, preparation duration, candidate counts and omissions; this index and the evaluation labels are for the operator, not the selector. Output hashes include the nested task files. Failed preparation cases remain recorded and the command exits nonzero; they receive no fabricated task.

The application task ceiling is 130,048 UTF-8 bytes, reserving up to 1,024 bytes for one declared transport wrapper within the 131,072-byte application-supplied limit. Count the actual wrapper bytes in orchestration records; do not add an unmeasured task prefix. Byte/4 token counts are estimates of visible application input. Harness/system context, reasoning tokens, actual model tokens and monetary cost remain unavailable unless the execution platform supplies them.

An authorized operator supplies each exact task to one fresh selector with the frozen model/reasoning settings, no other questions/history/labels, and no tools, source opening, network, retries or follow-up queries. Source text is untrusted evidence. Retain exact task/output bytes, requested settings, invocation/tool-call counts, start/end times and any observed usage. The runner does not enforce external-agent isolation or verify that a model was called; the independent execution trace must establish it. A mock reply validates mechanics only.

If the host needs file transport, declare that exception before the run: one read of the immutable assigned task, with an exact UTF-8 byte-count check and complete, ordered slices of that same content. Permit no additional retrieval or source reads. Count wrapper and slice overhead, check that no slice was truncated, and distinguish the per-response limit from aggregate model input; splitting a task does not reduce its total context. The current development transport uses up to 20 slices of 7,000 Unicode code points, at most 28,000 UTF-8 bytes per slice, plus a separately counted wrapper. Actual tokenizer counts remain unknown.

Store the raw reply for each index in a separate directory as `question-0000.json`, `question-0001.json`, etc. The current reply schema has exactly these fields, without a version field or prose:

```json
{"packet_fingerprint":"blake3:<exact packet digest>","ordered_ids":["c0000","c0003"]}
```

The selector can order at most 20 distinct supplied IDs, with a maximum 4,096 UTF-8 reply bytes. Empty IDs are permitted and do not prove unanswerability. Root/product validation reconstructs the exact query/request/snapshot-bound packet and rejects wrong packets, unknown IDs or stale evidence. Selected passages that fail final packing limits remain truthful omissions; there is no second request to repair them.

```sh
python3 scripts/context_eval.py evaluate \
  --binary "$LWIKI" --hash-binary "$HASH_BINARY" \
  --dataset .artifacts/eval-research/qasper-mini-v1 --split train --split dev \
  --wiki .artifacts/eval-research/wiki-dev-001 \
  --mapping .artifacts/eval-research/import-dev-001/mapping.json \
  --mode hybrid --verification-max-elapsed-ms 2000 \
  --selection-dir .artifacts/eval-research/selector-replies-dev-001 \
  --output .artifacts/eval-research/host-selected-dev-001
```

Preparation and evaluation must use identical selected splits/order and all request settings, including the proof deadline. If `--verification-max-elapsed-ms` is absent, the binary default applies in both stages. Development may explicitly use another fixed deadline in both commands; declare it separately from acceptance. Freeze corpus/cache/query-vector acquisition before preparation, since sync/provider history can change snapshot bindings. Replaying replies on a changed packet fails rather than regenerating tasks or selections silently.

Replay freezes the supplied files, retains exact raw valid/malformed bytes and SHA256, and passes an unmodified retained copy to `context --selection`. For oversized inputs it retains only the first 4,097 bytes, labels the hash as a rejected prefix and never passes it as a trimmed reply. Missing, malformed, duplicate-key/ID, oversized, product-rejected and timed-out cases count as errors and zero positive completion/coverage, with no retry, repair, fallback or exclusion. Changes to supplied valid reply bytes during evaluation invalidate the run. Without `--selection-dir`, the evaluator retains its original deterministic behavior and report fields.

`context_seconds` and `context_latency_median_seconds` in replay measure **final local validation/packing only**. They exclude preparation and the external selector and must never be presented as end-to-end latency. Host replay reports the workflow explicitly and leaves selector invocations, actual tokens/cost and end-to-end duration null. Join the operator's per-question execution records with `tasks.json` and reply hashes to report preparation + selector + final validation + orchestration overhead, including failed cases. Record extra input/calls/cost alongside unchanged final 6 KB/1,500-token limits. Keep original-command, deterministic-packet and host-selected arms separate; independent critics grade the final returned evidence, not packet recall or selector confidence.

## Interpret the report

Every invocation retains argv, exit status, stdout/stderr, duration and hashes. `run.json` records dataset/manifest/input hashes, binary/helper hashes, mapping hash, mode, scope and bounds. `results.json` retains per-question scores, alternative annotations, warnings, omissions and context size; `summary.json` reports aggregates. `output-hashes.json` hashes report files. Inputs, binaries and mapped current revisions are checked again after evaluation. A process failure contributes zero to positive task rates and the command exits nonzero if any query failed.

Source hit@1 and hit@limit indicate where the designated source ranks; unanswerables are excluded from their denominators. Strict gold-span completeness requires every paragraph evidence group in at least one reference annotation to be fully covered by exact returned source citation intervals. Repeated identical paragraph locations are alternatives. Overlapping or adjacent citation intervals are merged before measuring byte coverage, so overlap cannot earn duplicate credit. Multiple reference annotations are alternatives, not fragments that can be combined to invent a complete reference.

Paragraph byte coverage is conservative: some marked paragraphs contain extra text unnecessary for the answer. Answer-highlight coverage narrows the location proxy but highlights may be incomplete, unmapped, or insufficient to entail the answer. The report exposes unmapped groups, highlight-eligible queries and successfully scored queries. The primary highlight rates use all questions with mapped gold highlights as their fixed denominator and give failed queries zero. Older `*_scored_queries` fields remain explicitly conditional diagnostic metrics; compare their denominator as well as their value. A fully retrieved paragraph or highlight is **not** a semantic-completeness label. The `semantic_complete_rate` field remains null with a reason. Negative controls record retrieved neighbors without calling them answers or pretending lwiki implements abstention.

Exact citation checks independently verify whole passage bytes from its mapped locator and each contained source-citation subspan against that citation's immutable revision and helper-computed BLAKE3 digest. Merged passages may retain smaller citations and mirrored-source citations can reference another content path; neither requires equality of every citation span/path to its containing passage. Only cited intervals earn strict location coverage. Current-revision mapping, verified-snapshot freshness and actual rendered byte/token bounds are checked separately. Retrieval quality errors remain distinct from citation/freshness/budget correctness blockers.

## Independent assessment and split discipline

Keep train for mechanical examples, dev for implementation choices, and test for a declared code/protocol freeze. Any set inspected while tuning becomes development data. Original paper IDs are disjoint; for additional corpora, split by document/entity family and deduplicate document text plus paraphrased question families. Public test labels and model pretraining cannot be made secret by naming a directory `test`; retain a separately curated unseen corpus and private questions for the critic.

For semantic assessment, independent reviewers first freeze each question's required propositions, acceptable alternative evidence sets and answerability scope. They then see the question and actual returned cited text, not implementation explanations or source-hit scores. Mark each proposition fully supported, partially supported, unsupported or contradicted; complete-task credit requires every requested proposition. The public critic rubric weights completeness 6/10, citation/freshness/budgets 2/10, reproducibility 1/10 and honest claims 1/10. HIGH additionally requires at least 90% complete fresh answerable tasks, at least 75% per declared broad category and no correctness blockers. Include exact missing facts and citations in the review. Agent expert scores are not recruited-human usability results.

Use two independent assessments for a stratified subset and adjudicate disagreements with evidence. A future LLM judge should be independent of the implementation/tuning model, use a versioned prompt, identify supporting citation spans and abstain on ambiguity; calibrate it against human/critic labels before making it a gate. Never hide a failing category in an average or improve recall by enlarging one candidate's context budget.

## Synthetic cases and automation

Fast CI uses disposable synthetic wikis and mock caches to cover UTF-8 boundaries, overlap/duplicate intervals, near-neighbor distractors, exact identifiers, distant facets, two-document hops, tight packing budgets, refresh/withdrawal and missing facts. Generate question/label pairs from fixed authored evidence first, with required propositions and byte offsets; freeze labels before retrieval runs. Include answer-bearing text far apart in one source and supporting chains across sources. Construct unanswerables by removing a necessary bridge or counterfactually changing a requested version/entity, then independently check the entire scoped corpus. Retrieving a neighboring answer string must fail support coverage. Freeze source family splits before synthesis and keep generated queries/answers out of indexed corpus content.

The public mini dataset is suitable for an offline nightly regression, and the larger retained sample for a broader scheduled comparison. Paid live-provider runs are separately authorized and record provider/model/vector fingerprints, acquisition receipts/usage, unknown monetary reservations, request/byte/deadline limits and cache reuse. Evaluation never silently turns a nightly job into a live request. Compare baseline and candidate on identical snapshots/caches, publish per-question evidence and failure counts, and keep historical failures.

Run the evaluator's isolated checks with:

```sh
python3 -m unittest discover -s scripts -p test_context_eval.py -v
python3 -m unittest discover -s scripts -p test_prepare_qasper.py -v
```

The [dataset research report](execution/reports/EVAL-RESEARCH.md) explains why QASPER is the first baseline and where multihop/relevance/generation datasets fit.
