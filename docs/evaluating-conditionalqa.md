# ConditionalQA development evaluation

[The offline adapter](../scripts/prepare_conditionalqa.py) produces evaluator schema v1 with adapter version v2 from the pinned ConditionalQA v1_0 **train** release. It renders the release's HTML-like fragments as readable Markdown. Its default is 24 development questions: eight with answer conditions, eight answerable without conditions and eight unanswerables. This broadens scenario/condition coverage; preparation and location coverage do not establish semantic completeness. Follow [the context evaluation protocol](evaluating-context.md) and [critic workflow](testing-usability.md) for actual retrieval assessment.

Selection uses the first eight original IDs per structural stratum, sorted by SHA256 of `lwiki-additional-dev-v1:<id>`. Counts and seed are recorded; selection uses no retrieval outcomes. These are the same 24 records as the retained additional-dataset research sample. There is one original `train` split, explicitly labeled development. No acceptance split is created from exposed examples. Preserve the [source-family discipline](eval-datasets.md#keep-the-next-holdout-unseen) when planning a separate unseen set.

## Reproduce offline

Inputs are local files; the adapter makes no network or provider calls. Acquire the following immutable release files if absent, using the bounds in [the acquisition guide](eval-datasets.md#reproduce-essential-acquisition). Each link is pinned to commit `77bd295952daf415548b3244db10880d3d55cfe0`. The adapter verifies all four hashes before creating output and caps data files at 10,000,000 bytes and notices at 1,000,000 bytes.

| Local input filename | Pinned download | SHA256 |
| --- | --- | --- |
| `conditionalqa-v1_0-train.json` | [Train JSON](https://raw.githubusercontent.com/haitian-sun/ConditionalQA/77bd295952daf415548b3244db10880d3d55cfe0/v1_0/train.json) | `2976f448bad58efd747c5b7b4127fb3ad7a4834f760e9022c06a86536618bc2d` |
| `conditionalqa-v1_0-documents.json` | [Documents JSON](https://raw.githubusercontent.com/haitian-sun/ConditionalQA/77bd295952daf415548b3244db10880d3d55cfe0/v1_0/documents.json) | `1c977c1b14738b9ce1e53336c5abf3b3de34b72ee24985e7de6d8fae30856c59` |
| `conditionalqa-README.md` | [Original README](https://raw.githubusercontent.com/haitian-sun/ConditionalQA/77bd295952daf415548b3244db10880d3d55cfe0/README.md) | `2c128ba7e4a50c3481f65d63536b1a8f7ab09d7135573df38e5085faa5323d06` |
| `conditionalqa-LISCENSE` | [Original software notice](https://raw.githubusercontent.com/haitian-sun/ConditionalQA/77bd295952daf415548b3244db10880d3d55cfe0/LISCENSE) | `62a47f90ddb8fda1bbf1e731a5861d827c109a3570b86b371f9d927cbc75aa14` |

Also retain [the project page](https://haitian-sun.github.io/conditionalqa/) as `conditionalqa-project.html` alongside the notices. It declares the dataset CC BY-SA 4.0. This moving page is checked for its license declaration, copied exactly and hashed; it is not represented as an immutable release. The research snapshot SHA256 is `4d6516d67e3adc79346e4e7d55e7ebb804c5ea1628fd6dc0eb9198569a84b539`. Retain that snapshot and fix the acquisition date for byte-identical output. The repository's BSD-2-Clause `LISCENSE` is software text; it does not replace the explicit dataset license. GOV.UK source URLs and the upstream research-use disclaimer accompany the adaptation.

```sh
python3 scripts/prepare_conditionalqa.py \
  --train .artifacts/eval-datasets-additional/upstream/conditionalqa-v1_0-train.json \
  --documents .artifacts/eval-datasets-additional/upstream/conditionalqa-v1_0-documents.json \
  --notices-dir .artifacts/eval-datasets-additional/upstream \
  --acquired-date 2026-10-03 \
  --output .artifacts/conditionalqa-adapter/dev-v2

python3 -m unittest discover -s scripts -p test_prepare_conditionalqa.py -v
```

Output must be new; the adapter refuses to overwrite it. Default paths match the example. `--counts CONDITIONED PLAIN UNANSWERABLE` changes the three declared positive counts, and `--dataset` changes the logical dataset identifier. Preserve a fixed acquisition date, dataset name, counts, corpus choice and project snapshot to reproduce identical bytes.

The default corpus contains only the 24 required documents, totaling 148,336 rendered UTF-8 bytes. It omits global distractors, so its source-hit scores cannot establish full-corpus relevance. `--corpus all` renders all 652 original documents (5,073,865 UTF-8 bytes) as development sources, with the same selected questions. This changes the corpus and requires a fresh wiki/mapping/protocol. It dedicates all included source families to development; do not then call questions over those families unseen acceptance data.

## Source and evidence representation

Every original `contents` string is retained exactly in complete upstream JSON and the external entry map. The imported `.md` source is a deterministic transformation: `h1`–`h6` become matching Markdown heading levels, paragraphs become prose, list items become bullets, and original `tr` text remains a row paragraph with its separators. Supported nested unordered/ordered lists preserve list structure, links retain their text/destination, inline emphasis/code become Markdown, and HTML entities are decoded once by Python's standard-library HTML parser. Literal Markdown/HTML punctuation in text is escaped to avoid inventing structure. Unknown tags, comments, declarations, malformed nesting and nesting beyond 64 elements fail explicitly. All 652 release documents render without those failures.

The source adds blank lines between rendered entries and a final newline. It does not add questions, answers, scenarios, evidence labels or condition labels. Original titles remain document metadata. Complete upstream JSON bytes are retained under `upstream/`; notices retain their exact supplied bytes. `train/source-map.jsonl` records each content entry's original array index, exact raw fragment/hash, exact rendered fragment/hash, and UTF-8 byte endpoints in the imported Markdown. Original fragments can be reconstructed exactly from this map. This is an entry-level reversible provenance map; it does not claim that rendered byte offsets are original HTML byte offsets. `train/documents.jsonl` records original source URLs and rendered content hashes.

Each query is exactly `Scenario: <original scenario>\n\nQuestion: <original question>`. Both original strings and the complete upstream record remain external. `original_question` contains this combined task so the evaluator's `--query-style original` also preserves decisive scenario facts; `upstream_question` preserves the short upstream question verbatim. `query` contains the same combined task. Neither query style adds a paper-title prefix for this adapter.

The dataset contains **answer sets**, not a choice between individually sufficient answers. The adapter therefore creates one `reference_answers_any_of` annotation containing the entire original answer/condition set. Its `answer_condition_associations` retains each answer index/text and every associated condition, with exact location alternatives. It does not convert the individual answer pairs into `any_of` annotations that would incorrectly reward retrieving just one answer.

`required_evidence_all_of` includes the union of upstream evidence strings and associated condition strings, each rendered with **the same fragment algorithm used for sources**. Gold `quote` and its byte hash refer to exact rendered Markdown; `upstream_quote` preserves the original HTML-like evidence. A string that serves both roles has both role labels. Duplicate equal strings share one required location group; all original occurrences/associations remain in the raw record. Every repeated exact rendered occurrence is an alternative location, including overlapping occurrences. No Unicode normalization, fuzzy match or semantic substitution is used. Nested fragments whose standalone transformation differs from their surrounding source context fail mapping rather than gaining invented spans. This is a strict annotation-location diagnostic: the gold union can contain redundant context, and coverage does not establish answer applicability or reasoning completeness. Highlight metrics are unavailable because this release supplies no separate gold answer highlights; condition strings are not fabricated as answer highlights.

Unanswerable records retain their upstream empty answer/evidence sets, explicit Boolean label and designated-document scope. The compatibility annotation is a wrapper around those empty annotations, not a fabricated answer. An empty context does not prove absence, and a retrieved neighbor does not prove answerability. Even with `--corpus all`, negatives remain scoped to their designated document.

Every selected ID appears in `train/selected.raw.jsonl` and `preparation-audit.json`. Any missing source, source-render failure, unaligned evidence/condition, empty evidence string, positive annotation on an unanswerable, or answerable without answers/evidence is retained in `train/excluded.jsonl`; source-render errors also retain their full original document in the audit. Preparation then exits nonzero **without `manifest.json`**. It never replaces the failed question or delivers a silently reduced scoring denominator. A failed unselected distractor in `--corpus all` also prevents a usable manifest. Re-run fixes in a new directory and retain the failed audit. Structural malformed input, hash mismatch and size-bound failures are rejected before output creation.

`development-family-ledger.json` records included source URLs, selected IDs, all train IDs sharing each included URL and exact-byte duplicate source families. It does not detect near duplicates, shared entities or document templates; a custodian must audit those before selecting unseen families. The full release inputs are public development/provenance material, not a private holdout.

## Run the existing evaluator

The current runner accepts these labels without changes. Select `train` explicitly; its default `dev` does not exist in this adapter. Supply absolute prebuilt CLI/helper paths as described in [the context evaluation guide](evaluating-context.md#run-against-an-existing-binary), with new wiki/output paths for every run.

```sh
python3 scripts/context_eval.py import \
  --binary "$LWIKI" --dataset .artifacts/conditionalqa-adapter/dev-v2 \
  --split train --wiki .artifacts/conditionalqa-adapter/wiki-dev-001 \
  --output .artifacts/conditionalqa-adapter/import-dev-001

python3 scripts/context_eval.py evaluate \
  --binary "$LWIKI" --hash-binary "$HASH_BINARY" \
  --dataset .artifacts/conditionalqa-adapter/dev-v2 --split train \
  --wiki .artifacts/conditionalqa-adapter/wiki-dev-001 \
  --mapping .artifacts/conditionalqa-adapter/import-dev-001/mapping.json \
  --mode lexical --query-style original \
  --output .artifacts/conditionalqa-adapter/lexical-dev-001
```

For designated-document passage selection, add `--paper-local`; its legacy name restricts to the designated source ID. Source hit becomes trivial under this restriction. Keep global and scoped scores separate. All runner invocations remain offline. Semantic/hybrid evaluation needs separately authorized, frozen compatible caches and paid-call limits; adapter preparation acquires none.

The v2 preparation check on 2026-10-03 loaded all 24 questions/24 sources with the existing `load_dataset`, verified 74 rendered evidence strings and 13 condition associations, retained one evidence group with repeated locations, and found zero exclusions/source-render errors. Full-corpus preparation rendered all 652 documents successfully. Fifteen focused tests check actual release fragment structure, Unicode/entity/list/link conversions, reversible source maps, condition/scenario preservation, negative annotations, repeated/unmapped evidence, complete answer sets, input/output tampering, finite bounds and identical preparation. These establish adapter mechanics. Actual CLI retrieval and semantic assessment are separate runs.

## Retain the failed v1 run

The first local adapter preserved raw `<p>`, `<li>` and heading tags in `.md` source files. The scoped lexical CLI run on the earlier prototype inspected nonzero source bytes but found zero Markdown blocks, returned zero passages/citations, and scored zero gold location coverage. The source-unit parser only collects supported Markdown paragraph/heading/list starts; raw HTML did not enter that candidate stage. A source hit therefore could not become cited evidence. V2 fixes the input representation generically, without changing labels or tuning individual questions. Original v1 dataset/import/run artifacts remain retained; a new v2 import/mapping is required because source bytes and gold offsets changed.

That run also retained one independent error: `train-211` exceeded the earlier product's 64-whitespace-term lexical query ceiling. Its full task is 75 terms/398 UTF-8 bytes. V2 retains every task query byte-identical to v1 and records term/byte counts without truncating scenario facts or dropping the record. The product-side query-bound change and its boundary tests are separate from this rendering fix; use a binary whose query limits cover the declared tasks, and retain any errors in the denominator. Adapter preparation itself does not establish a new product query limit.

## Remaining evaluation work

The existing runner scores citation correctness, source hits and gold location coverage. It does not interpret `answer_condition_associations`, judge whether all requested answers and their applicability conditions are supported, detect unsupported conclusions, or make an abstention decision. Independent semantic labels/critic assessment are required for those metrics. Report conditioned/plain/unanswerable strata separately and preserve every retrieval error in its declared denominator. The runner currently aggregates the whole selected split; per-stratum reporting and explicit condition-location diagnostics are useful extensions.

Any model-assisted selection or answer workflow needs its own declared protocol, isolation and additional calls/input/latency/cost accounting before opening a holdout. No location score, source hit, mock provider or official answer-string score can substitute for that gate. V2 rendering has independent byte maps and a distinct dataset version; any further source transformation requires new source hashes, import mappings and a declared protocol.
