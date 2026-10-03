# ContractNLI development adapter

Use ContractNLI to examine contract exceptions, explicit contradiction and missing information alongside the broader [dataset mix](eval-datasets.md). This train-only adapter prepares source bytes and external annotations for the [context evaluation protocol](evaluating-context.md). It performs no CLI evaluation, embedding acquisition or classification. Preparation and passing adapter tests establish format compatibility, not retrieval quality or acceptance.

## Reproduce the inputs

Acquire the [official archive at commit eced6528dd3c1d14d73f9a87df8f7bdbc03126f9](https://raw.githubusercontent.com/stanfordnlp/contract-nli/eced6528dd3c1d14d73f9a87df8f7bdbc03126f9/resources/contract-nli.zip) into a new local directory. Its SHA256 is `e03fc77bbf8b53e2976a250e81d8a294bc3d5e5fb014521e477dee9340d6287b` and size is 65,362,913 bytes. The adapter refuses a different hash, caps archive reads at 70,000,000 bytes, member reads at 12,000,000 bytes and member count at 2,000. It reads only train JSON, README, LICENSE and TERMS; it does not extract archive paths, read dev/test labels, execute upstream code or fetch anything.

```sh
python3 scripts/prepare_contractnli.py \
  --archive .artifacts/eval-datasets-additional/upstream/contractnli.zip \
  --output .artifacts/contractnli-adapter/dev-v1 \
  --dataset contractnli-dev-v1 --contracts 12 --acquired-date 2026-10-03

python3 -m unittest discover -s scripts -p test_prepare_contractnli.py
```

The output directory must be new. Use the recorded acquisition date when reproducing retained inputs; the default is today's UTC date. The logical dataset name, count and acquisition date must match for identical manifests. Selection orders train contract IDs by SHA256 of `lwiki-additional-dev-v1:<decimal_id>` and retains the first N contracts, then all 17 hypotheses for every selected contract. This matches the initial research sample selection for N=12. No selection uses prediction outcomes or drops a hypothesis by class.

The release's original LICENSE, TERMS and README accompany the prepared sample with attribution to Koreeda and Manning's [original paper](https://aclanthology.org/2021.findings-emnlp.164/). The dataset declares CC BY 4.0; preserve its notices and original contract source URLs. Derived artifacts are local development material under ignored `.artifacts/` paths.

## Source, labels and denominator

The adapter writes each complete upstream `text` field as UTF-8 `.md` bytes, without adding headings, normalizing Unicode, rewriting line endings or appending a newline. Only these Markdown documents enter the wiki. Hypotheses, classification labels, annotations, span indices, filenames and original URLs remain in external JSON metadata. A hypothesis that already occurs naturally in source text is preserved.

Each task has a designated `doc_id`, the untouched hypothesis, a document-specific query and the original three-way label: `Entailment`, `Contradiction` or `NotMentioned`. Schema-1 `answerability` is `answerable` for both Entailment and Contradiction, and `unanswerable` for NotMentioned. This compatibility projection is not a binary NLI classifier. The preserved `classification` field is the authoritative class label. Evidence offsets map upstream Unicode character boundaries to exact UTF-8 byte boundaries and include the exact quote and SHA256.

Every original annotation set becomes a distinct `reference_answers_any_of` alternative. All annotated span indices are retained within that alternative. The schema field `required_evidence_all_of` supports the existing runner's **exhaustive annotation-location diagnostic**: the original ContractNLI annotations include redundant evidence, so recovering every annotated span is not a requirement for sufficient semantic support. A lower exhaustive gold-span score can coexist with a correct, adequately supported classification. The adapter declares this interpretation in each reference, task and manifest. `highlighted_evidence` is empty; no independent answer-highlight or sufficient-evidence labels are invented.

`train/labels.raw.json` preserves all selected upstream records and definitions. `train/all-records.jsonl` contains every planned contract/hypothesis task and its mapping status. `train/excluded.jsonl` records any mapping failure; `preparation-audit.json` declares the complete planned denominator, selected IDs, class counts and failures. A selected-record mapping error fails preparation after retaining this audit and source material, without creating a usable `manifest.json`. No replacement contracts or survivor-only manifest are produced. Invalid archive pins or incompatible label schemas fail before writing output. Successful manifests hash every prepared artifact and retain original archive/member hashes.

## Evaluate designated-contract evidence

Use the existing runner with explicit `--split train`; its default split is dev. Use new disposable wiki and report paths and absolute paths to separately prepared binaries. Import reads only the manifest's Markdown documents; it does not index task labels or notices.

```sh
python3 scripts/context_eval.py import \
  --binary /absolute/path/to/lwiki \
  --dataset .artifacts/contractnli-adapter/dev-v1 --split train \
  --wiki .artifacts/contractnli-adapter/wiki-dev-001 \
  --output .artifacts/contractnli-adapter/import-dev-001

python3 scripts/context_eval.py evaluate \
  --binary /absolute/path/to/lwiki --hash-binary /absolute/path/to/fixture_hash \
  --dataset .artifacts/contractnli-adapter/dev-v1 --split train \
  --wiki .artifacts/contractnli-adapter/wiki-dev-001 \
  --mapping .artifacts/contractnli-adapter/import-dev-001/mapping.json \
  --output .artifacts/contractnli-adapter/context-dev-001 \
  --mode lexical --paper-local --query-style original
```

Despite its QASPER-oriented name, `--paper-local` restricts the query to this task's designated contract using `--source-id`. This is the primary passage-evidence experiment: evidence from another contract cannot establish Entailment, Contradiction or NotMentioned for the designated contract. Scoped source-hit metrics are trivial and should not be described as global retrieval quality. A separate global experiment can use the document-specific prefixed query without `--paper-local`; report contract retrieval and passage selection separately and inspect wrong-contract results. Absence of returned evidence never proves NotMentioned. The context runner does not score classification or negative-control correctness.

Report location diagnostics, citation correctness, sufficient evidence and three-way task accuracy separately. The latter two require independently adjudicated semantics under a declared protocol; do not treat exhaustive gold-span coverage or unit tests as a critic score. Break results out by contract and label and macro-average over contracts/classes: 204 repeated-hypothesis tasks do not provide 204 distinct natural user questions or compensate for failed multi-source categories.

This public development sample is already exposed and cannot be an unseen acceptance set. For later acceptance, freeze binary, protocol, budgets and thresholds before exposing questions, and group by independently established contract/template/source families. Splitting hypothesis rows from the same contract leaks nearly identical source material across partitions. The adapter does not infer family IDs or prepare a holdout; original upstream split membership alone does not certify template-family separation. Follow the [critic workflow](testing-usability.md), retain all errors and use independently authored varied tasks alongside this auxiliary NLI arm.
