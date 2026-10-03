#!/usr/bin/env python3
"""Prepare train-only ContractNLI development fixtures from a pinned local ZIP.

No network, provider calls, credential reads, archive extraction or upstream code
execution. See docs/evaluating-contractnli.md for interpretation and reproduction.
"""
import argparse
from collections import Counter
from datetime import datetime, timezone
import hashlib
import io
import json
from pathlib import Path
import re
import stat
import sys
import zipfile

REPO = Path(__file__).resolve().parents[1]
DEFAULT_ROOT = REPO / ".artifacts" / "eval-datasets-additional"
ARCHIVE_SHA256 = "e03fc77bbf8b53e2976a250e81d8a294bc3d5e5fb014521e477dee9340d6287b"
UPSTREAM_COMMIT = "eced6528dd3c1d14d73f9a87df8f7bdbc03126f9"
ARCHIVE_URL = f"https://raw.githubusercontent.com/stanfordnlp/contract-nli/{UPSTREAM_COMMIT}/resources/contract-nli.zip"
MAX_ARCHIVE_BYTES = 70_000_000
MAX_MEMBER_BYTES = 12_000_000
MAX_MEMBERS = 2_000
MEMBERS = ("contract-nli/train.json", "contract-nli/README.md", "contract-nli/LICENSE", "contract-nli/TERMS")
SEED = "lwiki-additional-dev-v1"
LABELS = ("Entailment", "Contradiction", "NotMentioned")
EVIDENCE_SEMANTICS = "exhaustive annotated-span location coverage only; redundant spans are not required semantic facets"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(blob):
    return hashlib.sha256(blob).hexdigest()


def encoded(value):
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")


def char_to_byte(text):
    """Map every Unicode code-point boundary, including EOF, to UTF-8 bytes."""
    offsets = [0]
    for char in text:
        offsets.append(offsets[-1] + len(char.encode("utf-8")))
    return offsets


def location(text, offsets, span):
    require(isinstance(span, list) and len(span) == 2, "Expected two character endpoints")
    start, end = span
    require(type(start) is int and type(end) is int and 0 <= start < end <= len(text), "Evidence character span outside document")
    quote = text[start:end]
    return {"start_byte": offsets[start], "end_byte": offsets[end], "quote": quote,
            "quote_sha256": sha(quote.encode("utf-8")), "start_char": start, "end_char": end}


def read_archive(path):
    with Path(path).open("rb") as stream:
        raw = stream.read(MAX_ARCHIVE_BYTES + 1)
    require(len(raw) <= MAX_ARCHIVE_BYTES, "Archive exceeds bounded size")
    require(sha(raw) == ARCHIVE_SHA256, "Archive SHA256 mismatch; use the pinned official download")
    payloads = {}
    with zipfile.ZipFile(io.BytesIO(raw)) as bundle:
        infos = bundle.infolist()
        require(len(infos) <= MAX_MEMBERS, "Archive exceeds member-count bound")
        require(len({item.filename for item in infos}) == len(infos), "Duplicate ZIP member names")
        for name in MEMBERS:
            info = bundle.getinfo(name)
            mode = info.external_attr >> 16
            require(not info.is_dir() and (stat.S_IFMT(mode) in (0, stat.S_IFREG)), "Expected regular ZIP member")
            require(not info.flag_bits & 1 and info.file_size <= MAX_MEMBER_BYTES, "Expected unencrypted bounded ZIP member")
            with bundle.open(info) as stream:
                payload = stream.read(MAX_MEMBER_BYTES + 1)
            require(len(payload) == info.file_size and len(payload) <= MAX_MEMBER_BYTES, "ZIP member size mismatch")
            payloads[name] = payload
    return payloads, {"file": "contractnli.zip", "url": ARCHIVE_URL, "sha256": sha(raw), "bytes": len(raw)}


def validate_train(data, count):
    require(isinstance(data, dict) and isinstance(data.get("documents"), list) and isinstance(data.get("labels"), dict), "Invalid train JSON schema")
    docs, hypotheses = data["documents"], data["labels"]
    require(len(hypotheses) == 17, "Expected all 17 upstream hypotheses")
    require(all(re.fullmatch(r"nda-[0-9]+", key) and isinstance(value.get("hypothesis"), str) and value["hypothesis"] for key, value in hypotheses.items()), "Invalid hypothesis definition")
    require(count <= len(docs), "Requested more contracts than train contains")
    require(all(type(doc.get("id")) is int and doc["id"] >= 0 for doc in docs), "Invalid document ID")
    require(len({doc["id"] for doc in docs}) == len(docs), "Duplicate train document IDs")
    selected = sorted(docs, key=lambda doc: sha((SEED + ":" + str(doc["id"])).encode("utf-8")))[:count]
    for doc in selected:
        require(isinstance(doc.get("text"), str) and doc["text"], "Missing full contract text")
        require(isinstance(doc.get("spans"), list), "Missing upstream spans")
        require(isinstance(doc.get("annotation_sets"), list) and doc["annotation_sets"], "Missing annotation sets")
        for annotation in doc["annotation_sets"]:
            require(set(annotation["annotations"]) == set(hypotheses), "Annotation lacks the complete hypothesis set")
            for answer in annotation["annotations"].values():
                require(answer.get("choice") in LABELS and isinstance(answer.get("spans"), list), "Invalid three-way label or annotation span list")
    return selected


def question(doc, key, hypothesis, dataset):
    doc_id = "contractnli:" + str(doc["id"])
    offsets = char_to_byte(doc["text"])
    references, labels = [], []
    for index, annotation in enumerate(doc["annotation_sets"]):
        answer = annotation["annotations"][key]
        label = answer["choice"]
        labels.append(label)
        span_ids = answer["spans"]
        require((label == "NotMentioned") == (not span_ids), "Label/evidence disagreement")
        require(len(set(span_ids)) == len(span_ids), "Duplicate annotation span indices")
        evidence = []
        for span_id in span_ids:
            require(type(span_id) is int and 0 <= span_id < len(doc["spans"]), "Annotation span index outside document")
            evidence.append({"doc_id": doc_id, "upstream_span_index": span_id,
                             "locations_any_of": [location(doc["text"], offsets, doc["spans"][span_id])]})
        references.append({"annotation_id": f"{doc_id}:annotation-{index}:{key}",
                           "answer": {"unanswerable": label == "NotMentioned", "classification": label,
                                      "hypothesis": hypothesis, "upstream_annotation": answer},
                           "required_evidence_all_of": evidence, "highlighted_evidence": [],
                           "evidence_semantics": EVIDENCE_SEMANTICS})
    require(len(set(labels)) == 1, "Annotation classification disagreement; manual adjudication required")
    original = "Does this contract entail or contradict the following hypothesis, or is it not mentioned? " + hypothesis
    return {"schema_version": 1, "dataset": dataset, "split": "train",
            "question_id": f"{doc_id}:{key}", "doc_id": doc_id, "hypothesis_id": key,
            "original_question": original,
            "query": f'In contract {doc["id"]} ("{doc["file_name"]}"), ' + original,
            "query_adapter": "contract-id-filename-classification-v1", "scope": "designated_contract",
            "classification": labels[0], "answerability": "unanswerable" if labels[0] == "NotMentioned" else "answerable",
            "reference_answers_any_of": references, "evidence_semantics": EVIDENCE_SEMANTICS,
            "expected_propositions": None, "proposition_status": "independent_sufficient_evidence_adjudication_required",
            "categories": ["contract_nli", "long_document", labels[0]]}


def prepare(archive, output, dataset="contractnli-dev-v1", contracts=12, acquired_date=None):
    output = Path(output).resolve()
    require(not output.exists(), "Output must be a new directory; choose another --output")
    require(re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.-]*", dataset), "Dataset name must be a simple identifier")
    require(type(contracts) is int and contracts > 0, "Contract count must be positive")
    acquired_date = acquired_date or datetime.now(timezone.utc).date().isoformat()
    require(datetime.strptime(acquired_date, "%Y-%m-%d").strftime("%Y-%m-%d") == acquired_date, "Acquired date must be YYYY-MM-DD")
    payloads, acquisition = read_archive(archive)
    data = json.loads(payloads[MEMBERS[0]])
    selected = validate_train(data, contracts)
    folder = output / "train"
    (folder / "documents").mkdir(parents=True, exist_ok=False)
    (output / "notices").mkdir()
    for name in MEMBERS[1:]:
        (output / "notices" / Path(name).name).write_bytes(payloads[name])
    (output / "notices" / "ATTRIBUTION.md").write_text(
        "# ContractNLI development sample\n\n"
        "Yuta Koreeda and Christopher D. Manning. ContractNLI: A Dataset for Document-level Natural Language "
        "Inference for Contracts. Findings of EMNLP 2021. https://aclanthology.org/2021.findings-emnlp.164/\n\n"
        "The upstream release declares CC BY 4.0. Original LICENSE, TERMS and README accompany this sample. "
        "Original contract URLs and filenames remain in external metadata.\n\n"
        "Changes: deterministic train-only subset; full original text encoded as UTF-8 .md without added headings "
        "or normalization; external queries/labels and character-to-byte evidence mapping. No label, query or "
        "hypothesis is appended to source text. Public development data, not an unseen acceptance set.\n",
        encoding="utf-8")
    documents, questions, excluded, records = [], [], [], []
    planned_labels = Counter()
    for doc in selected:
        doc_id = "contractnli:" + str(doc["id"])
        blob = doc["text"].encode("utf-8")
        relative = f'documents/{doc["id"]}.md'
        (folder / relative).write_bytes(blob)
        documents.append({"doc_id": doc_id, "contract_id": doc["id"], "title": doc["file_name"],
                          "path": relative, "sha256": sha(blob), "bytes": len(blob), "url": doc["url"],
                          "file_name": doc["file_name"], "document_type": doc["document_type"],
                          "license_notice": "CC BY 4.0 per upstream release; original LICENSE and TERMS retained"})
        for key, label_definition in sorted(data["labels"].items()):
            raw_answers = [ann["annotations"][key] for ann in doc["annotation_sets"]]
            labels = sorted({answer["choice"] for answer in raw_answers})
            planned_labels.update(labels if len(labels) == 1 else ["annotation_disagreement"])
            record = {"question_id": f"{doc_id}:{key}", "doc_id": doc_id, "hypothesis_id": key,
                      "hypothesis": label_definition, "annotations": raw_answers, "classifications": labels}
            try:
                questions.append(question(doc, key, label_definition["hypothesis"], dataset))
                record["mapping_status"] = "mapped"
            except (ValueError, KeyError, TypeError, UnicodeError) as error:
                record.update({"mapping_status": "failed", "reason": str(error)})
                excluded.append({"question_id": record["question_id"], "doc_id": doc_id, "reasons": [str(error)]})
            records.append(record)
    questions.sort(key=lambda row: row["question_id"])
    for name, rows in (("documents.jsonl", documents), ("questions.jsonl", questions),
                       ("excluded.jsonl", excluded), ("all-records.jsonl", records)):
        (folder / name).write_bytes(b"".join(encoded(row) for row in rows))
    (folder / "labels.raw.json").write_bytes(encoded({"labels": data["labels"], "documents": selected}))
    stats = {"selected_contracts": len(documents), "hypotheses_per_contract": len(data["labels"]),
             "planned_questions": len(records), "questions": len(questions), "failed_mapping_questions": len(excluded),
             "excluded_questions": len(excluded), "planned_classifications": dict(sorted(planned_labels.items())),
             "answerable": sum(q["answerability"] == "answerable" for q in questions),
             "unanswerable": sum(q["answerability"] == "unanswerable" for q in questions),
             "document_bytes": sum(doc["bytes"] for doc in documents)}
    audit = {"status": "failed" if excluded else "complete", "stats": stats,
             "selected_contract_ids": [doc["id"] for doc in selected],
             "denominator_policy": "every selected contract times all 17 hypotheses; no replacements or survivor-only result",
             "failed_questions": excluded}
    (output / "preparation-audit.json").write_bytes(encoded(audit))
    require(not excluded, f"{len(excluded)} selected questions failed mapping; retained audit, no usable manifest")
    artifacts = [{"path": str(path.relative_to(output)), "sha256": sha(path.read_bytes()), "bytes": path.stat().st_size}
                 for path in sorted(output.rglob("*")) if path.is_file()]
    manifest = {"schema_version": 1, "dataset": dataset, "purpose": "public train-only development; not acceptance",
                "adapter_version": "contractnli-markdown-v1", "upstream_commit": UPSTREAM_COMMIT,
                "upstream": [acquisition] + [{"archive_member": name, "sha256": sha(payload), "bytes": len(payload)}
                                            for name, payload in sorted(payloads.items())],
                "acquired_utc_date": acquired_date, "selection_seed": SEED, "contract_limit": contracts,
                "selection": "first N train document IDs sorted by SHA256(seed + ':' + decimal_id); all 17 hypotheses",
                "selected_contract_ids": audit["selected_contract_ids"], "upstream_train_contracts": len(data["documents"]),
                "source_rendering": "exact full upstream text UTF-8 bytes; no headings, separators or normalization",
                "query_adapter": "contract-id-filename-classification-v1", "evidence_semantics": EVIDENCE_SEMANTICS,
                "license": "CC BY 4.0 per original release LICENSE/README; accompanying TERMS retained",
                "license_url": "https://creativecommons.org/licenses/by/4.0/",
                "citation": "Koreeda and Manning (2021), Findings of EMNLP, DOI 10.18653/v1/2021.findings-emnlp.164",
                "splits": {"train": stats}, "artifacts": artifacts,
                "limitations": ["17 fixed hypotheses repeated across contracts; report contract/label strata separately",
                                "family holdout requires independent template/source-family grouping",
                                "NotMentioned scoped only to designated contract; returning no evidence does not prove absence",
                                "schema answerability merges Entailment and Contradiction only for location diagnostics",
                                "full redundant annotation coverage is not sufficient-evidence or classification accuracy",
                                "context_eval.py does not score classification; use --paper-local for scoped passage evaluation",
                                "no lwiki commands, paid providers or acceptance evaluation performed"]}
    (output / "manifest.json").write_bytes(encoded(manifest))
    print(json.dumps(stats, indent=2, sort_keys=True))
    return manifest


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, default=DEFAULT_ROOT / "upstream" / "contractnli.zip")
    parser.add_argument("--output", type=Path, default=REPO / ".artifacts" / "contractnli-adapter" / "dev-v1")
    parser.add_argument("--dataset", default="contractnli-dev-v1")
    parser.add_argument("--contracts", type=int, default=12)
    parser.add_argument("--acquired-date", help="YYYY-MM-DD; set to the retained acquisition date for identical manifests")
    args = parser.parse_args(argv)
    prepare(args.archive, args.output, args.dataset, args.contracts, args.acquired_date)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, KeyError, TypeError, OSError, zipfile.BadZipFile) as error:
        print(f"prepare_contractnli: {error}", file=sys.stderr)
        sys.exit(1)
