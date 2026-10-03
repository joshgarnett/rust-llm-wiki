#!/usr/bin/env python3
"""Prepare train-only ConditionalQA development data from pinned local inputs.

No network, providers, credentials, product commands or upstream code execution.
Any selected-record alignment error retains an audit and fails without a manifest.
"""
import argparse
from collections import Counter
from datetime import datetime, timezone
import hashlib
from html.parser import HTMLParser
import json
from pathlib import Path
import re
import sys
from urllib.parse import quote as url_quote

REPO = Path(__file__).resolve().parents[1]
UPSTREAM = REPO / ".artifacts/eval-datasets-additional/upstream"
COMMIT = "77bd295952daf415548b3244db10880d3d55cfe0"
URL_ROOT = "https://raw.githubusercontent.com/haitian-sun/ConditionalQA/" + COMMIT + "/"
PROJECT_URL = "https://haitian-sun.github.io/conditionalqa/"
SEED = "lwiki-additional-dev-v1"
STRATA = ("with_conditions", "without_conditions", "unanswerable")
MAX_INPUT_BYTES = 10_000_000
MAX_NOTICE_BYTES = 1_000_000
MAX_HTML_DEPTH = 64
PINS = {
    "conditionalqa-v1_0-train.json": "2976f448bad58efd747c5b7b4127fb3ad7a4834f760e9022c06a86536618bc2d",
    "conditionalqa-v1_0-documents.json": "1c977c1b14738b9ce1e53336c5abf3b3de34b72ee24985e7de6d8fae30856c59",
    "conditionalqa-README.md": "2c128ba7e4a50c3481f65d63536b1a8f7ab09d7135573df38e5085faa5323d06",
    "conditionalqa-LISCENSE": "62a47f90ddb8fda1bbf1e731a5861d827c109a3570b86b371f9d927cbc75aa14",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(blob):
    return hashlib.sha256(blob).hexdigest()


def encoded(value):
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")


def read_bounded(path, cap, expected=None):
    with Path(path).open("rb") as stream:
        blob = stream.read(cap + 1)
    require(len(blob) <= cap, "Input exceeds bounded size: " + Path(path).name)
    require(expected is None or sha(blob) == expected, "Input SHA256 mismatch: " + Path(path).name)
    return blob


def strict_json(blob):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "Duplicate JSON object key: " + key)
            result[key] = value
        return result
    return json.loads(blob, object_pairs_hook=unique)


def category(question):
    if question["not_answerable"]:
        return "unanswerable"
    return "with_conditions" if any(pair[1] for pair in question["answers"]) else "without_conditions"


def locate(blob, quote):
    """Find all overlapping exact UTF-8 occurrences, including repeated list items."""
    raw = quote.encode("utf-8")
    cursor, spans = 0, []
    while raw:
        start = blob.find(raw, cursor)
        if start < 0:
            break
        spans.append({"start_byte": start, "end_byte": start + len(raw), "quote": quote, "quote_sha256": sha(raw)})
        cursor = start + 1
    return spans


class FragmentParser(HTMLParser):
    """Strict supported markup: unknown/malformed tags are errors, never omissions."""
    TAGS = {"p", "li", "ul", "ol", "tr", "td", "th", "table", "tbody", "thead", "tfoot",
            "h1", "h2", "h3", "h4", "h5", "h6", "a", "strong", "b", "em", "i",
            "code", "pre", "span", "div", "blockquote", "br", "hr"}
    VOID = {"br", "hr"}

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.root = {"tag": "root", "attrs": {}, "children": []}
        self.stack = [self.root]

    def handle_starttag(self, tag, attrs):
        require(tag in self.TAGS, "Unsupported HTML fragment tag: " + tag)
        require(tag in self.VOID or len(self.stack) <= MAX_HTML_DEPTH, "HTML fragment exceeds bounded nesting depth")
        require(len(dict(attrs)) == len(attrs), "Duplicate HTML attribute: " + tag)
        node = {"tag": tag, "attrs": dict(attrs), "children": []}
        self.stack[-1]["children"].append(node)
        if tag not in self.VOID:
            self.stack.append(node)

    def handle_endtag(self, tag):
        require(len(self.stack) > 1 and self.stack[-1]["tag"] == tag, "Unbalanced HTML fragment end tag: " + tag)
        self.stack.pop()

    def handle_startendtag(self, tag, attrs):
        self.handle_starttag(tag, attrs)
        if tag not in self.VOID:
            self.handle_endtag(tag)

    def handle_data(self, data):
        self.stack[-1]["children"].append(data)

    def handle_comment(self, data):
        raise ValueError("Unsupported HTML fragment comment; cannot silently discard content")

    def handle_decl(self, decl):
        raise ValueError("Unsupported HTML fragment declaration")

    def handle_pi(self, data):
        raise ValueError("Unsupported HTML fragment processing instruction")

    def unknown_decl(self, data):
        raise ValueError("Unsupported HTML fragment marked declaration")


def escape_markdown(text):
    # Protect literal source punctuation from becoming Markdown/HTML structure.
    escaped = re.sub(r"([\\`*_{}\[\]<>!#])", r"\\\1", text)
    escaped = re.sub(r"(?m)^([ ]{0,3})([-+])(?=\s|[-+])", r"\1\\\2", escaped)
    return re.sub(r"(?m)^([ ]{0,3}[0-9]+)([.)])(?=\s)", r"\1\\\2", escaped)


def plain_text(node):
    if isinstance(node, str):
        return node
    return "".join(plain_text(child) for child in node["children"])


def markdown_node(node, marker="- "):
    if isinstance(node, str):
        return escape_markdown(node)
    tag = node["tag"]
    if tag in ("ul", "ol"):
        children = []
        index = 0
        for child in node["children"]:
            if isinstance(child, dict) and child["tag"] == "li":
                index += 1
            children.append(markdown_node(child, str(index) + ". " if tag == "ol" else "- "))
        return "\n" + "".join(children).strip() + "\n\n"
    if tag in ("code", "pre"):
        text = plain_text(node)
        width = max((len(run) for run in re.findall(r"`+", text)), default=0) + 1
        if tag == "pre":
            fence = "`" * max(3, width)
            return fence + "\n" + text + ("" if text.endswith("\n") else "\n") + fence + "\n\n"
        fence = "`" * width
        pad = " " if text.startswith(("`", " ")) or text.endswith(("`", " ")) else ""
        return fence + pad + text + pad + fence
    text = "".join(markdown_node(child) for child in node["children"])
    if tag == "a":
        href = node["attrs"].get("href")
        return text if href is None else "[" + text + "](" + url_quote(href, safe="/:#?&=@~+-._%") + ")"
    if tag in ("strong", "b"):
        return "**" + text + "**"
    if tag in ("em", "i"):
        return "*" + text + "*"
    if re.fullmatch(r"h[1-6]", tag):
        return "#" * int(tag[1]) + " " + text.strip() + "\n\n"
    if tag == "li":
        return marker + text.strip().replace("\n", "\n  ") + "\n"
    if tag in ("p", "div", "table", "tbody", "thead", "tfoot", "tr"):
        if tag == "tr" and any(isinstance(child, dict) and child["tag"] in ("td", "th") for child in node["children"]):
            text = text.removesuffix(" | ")
        return text.strip() + "\n\n"
    if tag in ("td", "th"):
        return text.strip() + " | "
    if tag == "blockquote":
        return "\n".join("> " + line for line in text.strip().splitlines()) + "\n\n"
    if tag == "br":
        return "  \n"
    if tag == "hr":
        return "\n\n---\n\n"
    return text  # root/span are transparent containers, with all text retained.


def render_fragment(fragment):
    parser = FragmentParser()
    parser.feed(fragment)
    parser.close()
    require(len(parser.stack) == 1, "Unclosed HTML fragment tag: " + parser.stack[-1]["tag"])
    return markdown_node(parser.root).strip()


def raw_document_bytes(document):
    return ("\n\n".join(document["contents"]) + "\n").encode("utf-8")


def render(document):
    """Render every source fragment identically to its evidence; retain raw map."""
    chunks, entries, offset = [], [], 0
    for index, text in enumerate(document["contents"]):
        original = text.encode("utf-8")
        rendered = render_fragment(text)
        raw = rendered.encode("utf-8")
        entries.append({"content_index": index, "start_byte": offset, "end_byte": offset + len(raw),
                        "text_sha256": sha(raw), "utf8_bytes": len(raw), "rendered_text": rendered,
                        "upstream_text": text, "upstream_text_sha256": sha(original), "upstream_utf8_bytes": len(original)})
        chunks.append(raw)
        offset += len(raw) + 2
    return b"\n\n".join(chunks) + b"\n", entries


def validate_inputs(questions, documents):
    require(isinstance(questions, list) and isinstance(documents, list), "Expected upstream JSON lists")
    require(all(isinstance(q, dict) for q in questions) and all(isinstance(d, dict) for d in documents), "Expected object records")
    for document in documents:
        require(all(isinstance(document.get(key), str) and document[key] for key in ("url", "title")), "Document lacks URL/title")
        require(isinstance(document.get("contents"), list) and document["contents"] and all(isinstance(t, str) for t in document["contents"]), "Invalid document contents")
    require(len({d["url"] for d in documents}) == len(documents), "Duplicate upstream source URL")
    for question in questions:
        require(all(isinstance(question.get(key), str) and question[key] for key in ("id", "url", "question")), "Question lacks ID/URL/question")
        require(isinstance(question.get("scenario"), str), "Question lacks scenario")
        require(type(question.get("not_answerable")) is bool, "Expected Boolean upstream answerability")
        require(isinstance(question.get("evidences"), list) and all(isinstance(t, str) for t in question["evidences"]), "Invalid evidence list")
        require(isinstance(question.get("answers"), list), "Invalid answer list")
        for pair in question["answers"]:
            require(isinstance(pair, list) and len(pair) == 2 and isinstance(pair[0], str) and isinstance(pair[1], list)
                    and all(isinstance(c, str) for c in pair[1]), "Invalid answer/condition association")
    require(len({q["id"] for q in questions}) == len(questions), "Duplicate upstream question ID")


def adapt_question(question, blob, dataset):
    """One annotation holds the complete upstream answer set, never answer alternatives."""
    doc_id = "conditionalqa:" + question["url"]
    groups, issues = [], []
    all_quotes = list(dict.fromkeys(question["evidences"] + [c for _, conditions in question["answers"] for c in conditions]))
    mapped = {}
    for quote in all_quotes:
        roles = (["evidence"] if quote in question["evidences"] else []) + (["condition"] if any(quote in c for _, c in question["answers"]) else [])
        try:
            rendered = render_fragment(quote)
            locations = locate(blob, rendered)
        except ValueError as error:
            rendered, locations = "", []
            issues.append({"reason": "unsupported_or_malformed_evidence_markup", "quote": quote, "roles": roles, "error": str(error)})
        mapped[quote] = (rendered, locations)
        group = {"doc_id": doc_id, "quote": rendered, "upstream_quote": quote, "roles": roles, "locations_any_of": locations}
        groups.append(group)
        if not locations:
            issues.append({"reason": "empty_or_unmapped_text", "quote": quote, "roles": roles})
    associations = []
    for index, (answer, conditions) in enumerate(question["answers"]):
        associations.append({"answer_index": index, "answer_text": answer, "conditions_all_of": [
            {"doc_id": doc_id, "quote": mapped[condition][0], "upstream_quote": condition,
             "locations_any_of": mapped[condition][1]} for condition in conditions]})
    negative = question["not_answerable"]
    if negative and (question["answers"] or question["evidences"]):
        issues.append({"reason": "unanswerable_with_positive_annotations"})
    if not negative and (not question["answers"] or not question["evidences"]):
        issues.append({"reason": "answerable_without_answers_or_evidence"})
    task = "Scenario: " + question["scenario"] + "\n\nQuestion: " + question["question"]
    row = {"schema_version": 1, "dataset": dataset, "split": "train", "upstream_split": "train",
           "purpose": "development_only", "question_id": question["id"], "doc_id": doc_id,
           "original_question": task, "upstream_question": question["question"], "scenario": question["scenario"],
           "query": task, "query_adapter": "scenario-question-v1", "scope": "designated_document",
           "query_input": {"utf8_bytes": len(task.encode()), "whitespace_terms": len(task.split()), "truncated": False},
           "answerability": "unanswerable" if negative else "answerable", "stratum": category(question),
           "source_family": question["url"], "upstream_record": question,
           "reference_answers_any_of": [{"annotation_id": question["id"] + ":answer-set",
               "answer": {"unanswerable": negative, "upstream_answers": question["answers"]},
               "answer_condition_associations": associations,
               "required_evidence_all_of": groups, "highlighted_evidence": []}],
           "expected_propositions": None, "proposition_status": "independent_semantic_annotation_required",
           "categories": ["natural_question", "scenario_dependent", category(question)]}
    return row, issues


def prepare(train_path, documents_path, output, dataset="conditionalqa-dev-v2", counts=(8, 8, 8),
            acquired_date=None, notices_dir=UPSTREAM, corpus="selected"):
    output = Path(output).resolve()
    require(not output.exists(), "Output must be a new directory; choose another --output")
    require(re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.-]*", dataset), "Dataset name must be a simple identifier")
    require(len(counts) == 3 and all(type(n) is int and n > 0 for n in counts), "Three positive stratum counts are required")
    require(corpus in ("selected", "all"), "Corpus must be selected or all")
    acquired_date = acquired_date or datetime.now(timezone.utc).date().isoformat()
    require(datetime.strptime(acquired_date, "%Y-%m-%d").strftime("%Y-%m-%d") == acquired_date, "Acquired date must be YYYY-MM-DD")
    paths = {"conditionalqa-v1_0-train.json": Path(train_path), "conditionalqa-v1_0-documents.json": Path(documents_path),
             **{name: Path(notices_dir) / name for name in ("conditionalqa-README.md", "conditionalqa-LISCENSE")}}
    payloads = {name: read_bounded(path, MAX_INPUT_BYTES if name.endswith(".json") else MAX_NOTICE_BYTES, PINS[name]) for name, path in paths.items()}
    project = read_bounded(Path(notices_dir) / "conditionalqa-project.html", MAX_NOTICE_BYTES)
    require(b"creativecommons.org/licenses/by-sa/4.0/" in project and b"ConditionalQA" in project, "Project snapshot lacks dataset license declaration")
    questions = strict_json(payloads["conditionalqa-v1_0-train.json"])
    documents = strict_json(payloads["conditionalqa-v1_0-documents.json"])
    validate_inputs(questions, documents)
    selected = []
    for stratum, count in zip(STRATA, counts):
        candidates = [q for q in questions if category(q) == stratum]
        require(count <= len(candidates), "Requested more questions than stratum contains: " + stratum)
        selected.extend(sorted(candidates, key=lambda q: (sha((SEED + ":" + q["id"]).encode()), q["id"]))[:count])
    selected.sort(key=lambda q: q["id"])
    needed = {q["url"] for q in selected}
    by_url = {d["url"]: d for d in documents}
    document_indices = {d["url"]: i for i, d in enumerate(documents)}
    corpus_urls = set(by_url) if corpus == "all" else needed.intersection(by_url)
    output.mkdir(parents=True, exist_ok=False)
    for name in ("upstream", "notices", "train/documents"):
        (output / name).mkdir(parents=True)
    acquisition = []
    for name, blob in payloads.items():
        relative = ("upstream/" if name.endswith(".json") else "notices/") + name
        (output / relative).write_bytes(blob)
        url = URL_ROOT + ({"conditionalqa-v1_0-train.json": "v1_0/train.json", "conditionalqa-v1_0-documents.json": "v1_0/documents.json"}.get(name) or name.removeprefix("conditionalqa-"))
        acquisition.append({"file": relative, "sha256": sha(blob), "bytes": len(blob), "url": url, "immutable_commit": COMMIT})
    (output / "notices/conditionalqa-project.html").write_bytes(project)
    acquisition.append({"file": "notices/conditionalqa-project.html", "sha256": sha(project), "bytes": len(project), "url": PROJECT_URL, "immutable_commit": None})
    (output / "notices/ATTRIBUTION.md").write_text(
        "# ConditionalQA development adaptation\n\n"
        "Sun et al., ConditionalQA: A Complex Reading Comprehension Dataset with Conditional Answers. ACL 2022. "
        "https://aclanthology.org/2022.acl-long.253/\n\n"
        "Dataset declaration: CC BY-SA 4.0, https://haitian-sun.github.io/conditionalqa/ . "
        "License: https://creativecommons.org/licenses/by-sa/4.0/ . Preserve attribution and share-alike terms. "
        "The repository LISCENSE is BSD-2-Clause software text; both original notices are retained.\n\n"
        "Original source: GOV.UK; each document retains its source URL. These are historical research snapshots. "
        "The upstream README restricts use to NLP research and says answers are not legally verified.\n\n"
        "Adaptation renders HTML-like source fragments as readable Markdown, preserves every original fragment "
        "in a reversible external raw/rendered entry map and complete upstream JSON, and combines the original scenario and question. "
        "Answers, conditions and evidence annotations remain outside imported source documents.\n", encoding="utf-8")
    doc_rows, source_maps, blobs, families, source_errors = [], [], {}, [], []
    for url in sorted(corpus_urls):
        document = by_url[url]
        try:
            blob, entries = render(document)
        except ValueError as error:
            source_errors.append({"url": url, "upstream_document": document, "error": str(error)})
            continue
        blobs[url] = blob
        relative = "documents/" + sha(url.encode()) + ".md"
        (output / "train" / relative).write_bytes(blob)
        doc_id = "conditionalqa:" + url
        doc_rows.append({"doc_id": doc_id, "title": document["title"], "url": url, "path": relative,
                         "sha256": sha(blob), "bytes": len(blob), "source_family": url,
                         "license_notice": "ConditionalQA dataset: CC BY-SA 4.0; GOV.UK historical source snapshot; original URLs retained"})
        source_maps.append({"doc_id": doc_id, "upstream_document_index": document_indices[url],
                            "upstream_document_text_sha256": sha(raw_document_bytes(document)),
                            "contents": entries, "transform": "html-fragment-markdown-v2",
                            "reversible_scope": "entry-level raw fragments retained; output offsets are rendered UTF-8, not original HTML offsets"})
    # Exact duplicates link source families. Near-duplicate/entity/template clustering
    # remains a separate custodian task before choosing any unseen acceptance set.
    selected_hashes = {sha(raw_document_bytes(by_url[url])) for url in needed.intersection(by_url)}
    for document in documents:
        url = document["url"]
        digest = sha(raw_document_bytes(document))
        if url in corpus_urls or url in needed or digest in selected_hashes:
            families.append({"source_family": url, "upstream_document_text_sha256": digest,
                             "document_sha256": sha(blobs[url]) if url in blobs else None,
                             "selected_question_ids": [q["id"] for q in selected if q["url"] == url],
                             "all_train_question_ids": [q["id"] for q in questions if q["url"] == url],
                             "purpose": "development_family_exposure; never acceptance"})
    rows, excluded = [], []
    for question in selected:
        if question["url"] not in blobs:
            excluded.append({"question_id": question["id"], "stratum": category(question), "upstream_record": question,
                             "issues": [{"reason": "source_document_render_failed" if question["url"] in by_url else "source_document_missing"}]})
            continue
        row, issues = adapt_question(question, blobs[question["url"]], dataset)
        if issues:
            excluded.append({"question_id": question["id"], "stratum": category(question), "upstream_record": question,
                             "adapted_record": row, "issues": issues})
        else:
            rows.append(row)
    for name, records in (("documents.jsonl", doc_rows), ("questions.jsonl", rows), ("excluded.jsonl", excluded),
                          ("selected.raw.jsonl", selected), ("source-map.jsonl", source_maps)):
        (output / "train" / name).write_bytes(b"".join(encoded(row) for row in records))
    (output / "development-family-ledger.json").write_bytes(encoded(families))
    stats = {"selected_questions": len(selected), "questions": len(rows), "selected_strata": dict(Counter(category(q) for q in selected)),
             "upstream_train_questions": len(questions), "upstream_documents": len(documents),
             "upstream_strata": dict(Counter(category(q) for q in questions)), "documents": len(doc_rows),
             "answerable": sum(q["answerability"] == "answerable" for q in rows),
             "unanswerable": sum(q["answerability"] == "unanswerable" for q in rows), "excluded_questions": len(excluded),
             "document_bytes": sum(d["bytes"] for d in doc_rows), "source_render_errors": len(source_errors)}
    (output / "preparation-audit.json").write_bytes(encoded({"status": "failed" if excluded or source_errors else "ok", "stats": stats,
        "source_render_errors": source_errors,
        "selected_question_ids": [q["id"] for q in selected], "excluded_question_ids": [q["question_id"] for q in excluded],
        "replacement_questions": 0, "failed_records_remain_in_selected_denominator": True}))
    require(not excluded and not source_errors, "Selected-record mapping/annotation/source rendering failure; retained audit and labels, no usable manifest: " + str(output))
    artifacts = [{"path": str(p.relative_to(output)), "sha256": sha(p.read_bytes()), "bytes": p.stat().st_size}
                 for p in sorted(output.rglob("*")) if p.is_file()]
    manifest = {"schema_version": 1, "dataset": dataset, "upstream_version": "v1_0", "adapter_version": "conditionalqa-v2",
                "purpose": "development_only", "acquired_utc_date": acquired_date, "upstream_commit": COMMIT,
                "selection_seed": SEED, "stratum_counts": list(counts), "stratum_order": list(STRATA),
                "selection": "first N original train IDs per structural stratum sorted by SHA256(seed + ':' + id); no outcome filtering",
                "query_adapter": "scenario-question-v1", "corpus": corpus,
                "source_transform": "html-fragment-markdown-v2", "max_query_whitespace_terms": max(q["query_input"]["whitespace_terms"] for q in rows),
                "split_integrity": "original train only; no dev/test or acceptance partition manufactured",
                "license": "CC BY-SA 4.0 dataset declaration; repository BSD-2-Clause software notice separately retained",
                "license_url": "https://creativecommons.org/licenses/by-sa/4.0/", "upstream": acquisition,
                "splits": {"train": stats}, "artifacts": artifacts,
                "limitations": ["public development data; no unseen acceptance claim",
                    "all gold evidence plus condition strings are required location diagnostics, not semantic sufficiency",
                    "answer-condition associations retained; their applicability needs independent semantic assessment",
                    "unanswerability is scoped to designated document, never inferred from empty retrieval",
                    "selected corpus omits global distractors" if corpus == "selected" else "global corpus does not widen designated-document unanswerability",
                    "HTML-like tags become Markdown; exact originals and reversible entry maps retained externally",
                    "gold fragments use the same transform; nested context-dependent fragments may fail exact mapping and preparation",
                    "family ledger groups URLs and exact byte duplicates only; near-duplicate/entity/template audit remains required",
                    "no CLI retrieval, paid API calls, live-source verification or semantic completeness assessment"]}
    (output / "manifest.json").write_bytes(encoded(manifest))
    print(json.dumps(stats, sort_keys=True))
    return manifest


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--train", type=Path, default=UPSTREAM / "conditionalqa-v1_0-train.json")
    parser.add_argument("--documents", type=Path, default=UPSTREAM / "conditionalqa-v1_0-documents.json")
    parser.add_argument("--notices-dir", type=Path, default=UPSTREAM)
    parser.add_argument("--output", type=Path, default=REPO / ".artifacts/conditionalqa-adapter/dev-v2")
    parser.add_argument("--dataset", default="conditionalqa-dev-v2")
    parser.add_argument("--counts", nargs=3, type=int, default=[8, 8, 8], metavar=("CONDITIONED", "PLAIN", "UNANSWERABLE"))
    parser.add_argument("--corpus", choices=("selected", "all"), default="selected")
    parser.add_argument("--acquired-date", help="YYYY-MM-DD, default today; fix for byte-identical reproduction")
    args = parser.parse_args(argv)
    prepare(args.train, args.documents, args.output, args.dataset, args.counts, args.acquired_date, args.notices_dir, args.corpus)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, KeyError, TypeError, OSError) as error:
        print("prepare_conditionalqa: " + str(error), file=sys.stderr)
        sys.exit(1)
