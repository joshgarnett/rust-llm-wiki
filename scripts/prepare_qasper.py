#!/usr/bin/env python3
"""Prepare a deterministic text-only QASPER sample from pinned local archives.

No network, provider calls, credentials, lwiki commands or archive path extraction.
Existing output directories are refused. See docs/evaluating-context.md for downloads.
"""
import hashlib
import argparse
from datetime import datetime, timezone
import io
import json
from pathlib import Path
import re
import sys
import tarfile

REPO = Path(__file__).resolve().parents[1]
DEFAULT_ROOT = REPO / ".artifacts" / "eval-research"
SEED = "lwiki-qasper-text-v1"
MAX_ARCHIVE_BYTES = 50_000_000
MAX_MEMBER_BYTES = 50_000_000
ARCHIVES = {
    "qasper-train-dev-v0.3.tgz": "a28fdf966db827bcee3d873107d6b6669864fb7ca8fbf73a192f5e39191bdb5a",
    "qasper-test-and-evaluator-v0.3.tgz": "72a52a41193e2838b8074f80ac074b94f956b84886c36a61c58a7df4171bdd72",
}
MEMBERS = {
    "qasper-train-dev-v0.3.tgz": ("qasper-train-v0.3.json", "qasper-dev-v0.3.json", "README.md"),
    "qasper-test-and-evaluator-v0.3.tgz": ("qasper-test-v0.3.json", "README-test.md", "qasper_evaluator.py"),
}
CARD_URL = "https://huggingface.co/datasets/allenai/qasper"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def encoded(value):
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode()


def locate(blob, quote):
    raw = quote.encode()
    spans, cursor = [], 0
    while raw:
        start = blob.find(raw, cursor)
        if start < 0:
            break
        spans.append({"start_byte": start, "end_byte": start + len(raw), "quote": quote, "quote_sha256": sha(raw)})
        cursor = start + 1
    return spans


def render(paper):
    # Preserve paragraph strings exactly. Never insert answers/questions/evidence labels.
    chunks = ["# " + paper["title"] + "\n\n", "## Abstract\n\n", paper["abstract"] + "\n\n"]
    for section in paper["full_text"]:
        chunks.append("## " + (section["section_name"] or "Untitled section") + "\n\n")
        chunks.extend(paragraph + "\n\n" for paragraph in section["paragraphs"])
    if paper.get("figures_and_tables"):
        chunks.append("## Figure and table captions (images not included)\n\n")
        chunks.extend(item["caption"] + "\n\n" for item in paper["figures_and_tables"])
    return "".join(chunks).encode()


def prepare(train_dev_archive, test_archive, output, dataset="qasper-mini-v1", limits=(4, 4, 8), acquired_date=None, dataset_card=None):
    output = Path(output).resolve()
    require(not output.exists(), "Output must be a new directory; choose another --output")
    require(re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.-]*", dataset), "Dataset name must be a simple identifier")
    require(len(limits) == 3 and all(type(value) is int and value > 0 for value in limits), "Three positive paper counts are required")
    acquired_date = acquired_date or datetime.now(timezone.utc).date().isoformat()
    require(datetime.strptime(acquired_date, "%Y-%m-%d").strftime("%Y-%m-%d") == acquired_date, "Acquired date must be YYYY-MM-DD")
    data, acquisition, notices = {}, [], {}
    paths = {"qasper-train-dev-v0.3.tgz": Path(train_dev_archive), "qasper-test-and-evaluator-v0.3.tgz": Path(test_archive)}
    for archive, expected in ARCHIVES.items():
        path = paths[archive]
        with path.open("rb") as stream:
            raw = stream.read(MAX_ARCHIVE_BYTES + 1)
        require(len(raw) <= MAX_ARCHIVE_BYTES, f"Archive exceeds bounded size: {archive}")
        digest = sha(raw)
        require(digest == expected, f"Archive SHA256 mismatch: {archive}; use the documented official v0.3 download")
        acquisition.append({"file": archive, "sha256": digest, "bytes": len(raw),
                            "url": "https://qasper-dataset.s3.us-west-2.amazonaws.com/" + archive})
        with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as bundle:
            require({member.name for member in bundle.getmembers()} == set(MEMBERS[archive]), "Unexpected archive members")
            require(len(bundle.getmembers()) == len(MEMBERS[archive]), "Duplicate archive member names")
            for name in MEMBERS[archive]:
                member = bundle.getmember(name)
                require(member.isfile() and member.size <= MAX_MEMBER_BYTES, "Expected a bounded regular archive member")
                payload = bundle.extractfile(member).read(MAX_MEMBER_BYTES + 1)
                require(len(payload) == member.size and len(payload) <= MAX_MEMBER_BYTES, "Archive member size mismatch")
                if member.name.endswith(".json"):
                    split = member.name.split("-")[1]
                    data[split] = json.loads(payload)
                    acquisition.append({"archive_member": member.name, "sha256": sha(payload), "bytes": len(payload)})
                elif member.name.startswith("README"):
                    notices[member.name] = payload
                # The answer-generation evaluator is retained in the downloaded archive,
                # not executed or placed inside the text dataset.
    card_hash = None
    if dataset_card is not None:
        with Path(dataset_card).open("rb") as stream:
            card = stream.read(1_000_001)
        require(len(card) <= 1_000_000, "Dataset card exceeds bounded size")
        require("cc-by-4.0" in card.decode("utf-8").lower(), "Official card lacks the expected CC BY 4.0 notice")
        notices["qasper-dataset-card.md"] = card
        card_hash = sha(card)

    ids = [set(value) for value in data.values()]
    require(all(not left.intersection(right) for i, left in enumerate(ids) for right in ids[i + 1:]), "Upstream paper IDs overlap between splits")
    require(all(limits[i] <= len(data[split]) for i, split in enumerate(("train", "dev", "test"))), "Requested more papers than a split contains")
    for papers in data.values():
        require(all(re.fullmatch(r"[0-9]{4}\.[0-9]{4,5}(?:v[0-9]+)?", paper_id) for paper_id in papers), "Unsafe or unsupported paper ID")
    output.mkdir(parents=True, exist_ok=False)
    (output / "notices").mkdir()
    for name, payload in notices.items():
        (output / "notices" / name).write_bytes(payload)
    (output / "notices" / "ATTRIBUTION.md").write_text(
        "# QASPER text sample attribution\n\n"
        "QASPER v0.3: Pradeep Dasigi, Kyle Lo, Iz Beltagy, Arman Cohan, Noah A. Smith, Matt Gardner. "
        "A Dataset of Information-Seeking Questions and Answers Anchored in Research Papers. NAACL 2021. "
        "DOI: 10.18653/v1/2021.naacl-main.365.\n\n"
        "The official allenai/qasper dataset card declares CC BY 4.0: https://huggingface.co/datasets/allenai/qasper\n"
        "License: https://creativecommons.org/licenses/by/4.0/\n\n"
        "Paper text was extracted from S2ORC; original paper authors retain their rights. "
        "This sample adds Markdown headings/separators and a paper-title query prefix, preserves paragraph bytes, "
        "and audits out questions needing figure/table images, conflicting answerability, or unmappable evidence.\n",
        encoding="utf-8")
    stats, artifacts = {}, []
    for split, limit in zip(("train", "dev", "test"), limits):
        folder = output / split
        (folder / "documents").mkdir(parents=True)
        selected = sorted(data[split], key=lambda paper_id: sha((SEED + ":" + paper_id).encode()))[:limit]
        docs, questions, excluded = [], [], []
        for paper_id in selected:
            paper = data[split][paper_id]
            doc_id = "qasper:" + paper_id
            blob = render(paper)
            relative_path = "documents/" + paper_id + ".md"
            (folder / relative_path).write_bytes(blob)
            docs.append({"doc_id": doc_id, "paper_id": paper_id, "title": paper["title"], "path": relative_path,
                         "sha256": sha(blob), "bytes": len(blob), "url": "https://arxiv.org/abs/" + paper_id,
                         "license_notice": "QASPER release card: CC BY 4.0; paper text extracted from S2ORC; original author rights retained"})
            for qa in paper["qas"]:
                annotations = [item["answer"] for item in qa["answers"]]
                require(all(type(answer["unanswerable"]) is bool for answer in annotations), "Expected Boolean answerability")
                flags = {answer["unanswerable"] for answer in annotations}
                reasons = []
                if not annotations or len(flags) != 1:
                    reasons.append("missing_answers_or_answerability_disagreement")
                supports = []
                for index, answer in enumerate(annotations):
                    evidence = []
                    highlights = []
                    if not answer["unanswerable"]:
                        if not answer["evidence"]:
                            reasons.append("answerable_without_evidence")
                        for quote in answer["evidence"]:
                            locations = locate(blob, quote)
                            if quote.startswith("FLOAT SELECTED"):
                                reasons.append("figure_or_table_evidence_requires_images")
                            elif not locations:
                                reasons.append("unaligned_textual_evidence")
                            else:
                                evidence.append({"doc_id": doc_id, "locations_any_of": locations})
                        for quote in answer.get("highlighted_evidence", []):
                            highlights.append({"doc_id": doc_id, "quote": quote, "locations_any_of": locate(blob, quote)})
                    supports.append({"annotation_id": qa["answers"][index]["annotation_id"],
                                     "answer": answer, "required_evidence_all_of": evidence,
                                     "highlighted_evidence": highlights})
                if reasons:
                    excluded.append({"question_id": qa["question_id"], "paper_id": paper_id, "reasons": sorted(set(reasons))})
                    continue
                questions.append({"schema_version": 1, "dataset": dataset, "split": split,
                                  "question_id": qa["question_id"], "original_question": qa["question"],
                                  "query": 'In the paper "' + paper["title"] + '", ' + qa["question"],
                                  "query_adapter": "paper-title-prefix-v1", "doc_id": doc_id,
                                  "answerability": "unanswerable" if True in flags else "answerable",
                                  "scope": "designated_paper", "reference_answers_any_of": supports,
                                  "expected_propositions": None,
                                  "proposition_status": "independent_human_or_critic_annotation_required",
                                  "categories": ["natural_question", "long_document"]})
        questions.sort(key=lambda row: row["question_id"])
        for name, rows in (("documents.jsonl", docs), ("questions.jsonl", questions), ("excluded.jsonl", excluded)):
            (folder / name).write_bytes(b"".join(encoded(row) for row in rows))
        stats[split] = {"selected_papers": len(docs), "questions": len(questions),
                        "answerable": sum(row["answerability"] == "answerable" for row in questions),
                        "unanswerable": sum(row["answerability"] == "unanswerable" for row in questions),
                        "excluded_questions": len(excluded), "document_bytes": sum(row["bytes"] for row in docs)}
    for path in sorted(output.rglob("*")):
        if path.is_file() and path.name != "manifest.json":
            artifacts.append({"path": str(path.relative_to(output)), "sha256": sha(path.read_bytes()), "bytes": path.stat().st_size})
    manifest = {"schema_version": 1, "dataset": dataset, "upstream_version": "0.3",
                "acquired_utc_date": acquired_date, "selection_seed": SEED,
                "adapter_version": "qasper-markdown-v2", "paper_limits": list(limits),
                "selection": "first N paper IDs sorted by SHA256(seed + ':' + paper_id); no outcome filtering",
                "query_adapter": "paper-title-prefix-v1", "split_integrity": "all original paper IDs disjoint",
                "license": "CC BY 4.0 per official allenai/qasper card; original papers retain author rights",
                "license_url": "https://creativecommons.org/licenses/by/4.0/",
                "card_url": CARD_URL, "card_path": "notices/qasper-dataset-card.md" if card_hash else None,
                "card_sha256": card_hash,
                "citation": "Dasigi, Lo, Beltagy, Cohan, Smith, Gardner. NAACL 2021. DOI 10.18653/v1/2021.naacl-main.365",
                "upstream": acquisition, "splits": stats, "artifacts": artifacts,
                "limitations": ["public benchmark, not a private unseen holdout", "figure/table and unmappable questions excluded with audit",
                                "unanswerable scoped to designated paper, not every document", "gold paragraph coverage is not entailment",
                                "no lwiki runs, live providers, or independent semantic labels performed"]}
    (output / "manifest.json").write_bytes(encoded(manifest))
    print(json.dumps(stats, indent=2))
    return manifest


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--train-dev-archive", type=Path, default=DEFAULT_ROOT / "upstream" / "qasper-train-dev-v0.3.tgz")
    parser.add_argument("--test-archive", type=Path, default=DEFAULT_ROOT / "upstream" / "qasper-test-and-evaluator-v0.3.tgz")
    parser.add_argument("--output", type=Path, default=DEFAULT_ROOT / "qasper-mini-v1", help="Must not already exist")
    parser.add_argument("--dataset", default="qasper-mini-v1", help="Logical dataset identifier, independent of output path")
    parser.add_argument("--papers", nargs=3, type=int, default=[4, 4, 8], metavar=("TRAIN", "DEV", "TEST"))
    parser.add_argument("--acquired-date", help="UTC acquisition date YYYY-MM-DD; default today; fix for byte-identical reproduction")
    parser.add_argument("--dataset-card", type=Path, help="Optional locally downloaded official license/provenance card; copied and hashed")
    args = parser.parse_args(argv)
    prepare(args.train_dev_archive, args.test_archive, args.output, args.dataset, args.papers, args.acquired_date, args.dataset_card)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, KeyError, OSError, tarfile.TarError) as error:
        print(f"prepare_qasper: {error}", file=sys.stderr)
        sys.exit(1)
