#!/usr/bin/env python3
"""Reproducible offline source and evidence-location evaluation, never an answer judge."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import statistics
import subprocess
import sys
import time

SELECTOR_INPUT_MAX_BYTES = 130048
SELECTOR_WRAPPER_RESERVED_BYTES = 1024
SELECTION_REPLY_MAX_BYTES = 4096


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n", encoding="utf-8")


def within(root, relative):
    require(not Path(relative).is_absolute(), "Expected a relative artifact path")
    path = (root / relative).resolve()
    require(path.is_relative_to(root.resolve()), "Artifact path escapes its root")
    return path


def jsonl(path):
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line]


def union(intervals):
    merged = []
    for start, end in sorted(intervals):
        require(isinstance(start, int) and isinstance(end, int) and 0 <= start <= end, "Invalid byte interval")
        if start == end:
            continue
        if merged and start <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(end, merged[-1][1]))
        else:
            merged.append((start, end))
    return merged


def covered(start, end, intervals):
    return sum(max(0, min(end, right) - max(start, left)) for left, right in union(intervals))


def validate_location(blob, location):
    start, end = location["start_byte"], location["end_byte"]
    require(isinstance(start, int) and isinstance(end, int) and 0 <= start < end <= len(blob), "Gold span outside document")
    quote = location["quote"].encode("utf-8")
    require(blob[start:end] == quote, "Gold quote differs from document UTF-8 bytes")
    require(sha(quote) == location["quote_sha256"], "Gold quote hash mismatch")


def load_dataset(root, splits):
    root = root.resolve()
    manifest_path = root / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    require(manifest.get("schema_version") == 1, "Unsupported dataset schema")
    files = {item["path"]: item for item in manifest["artifacts"]}
    require(len(files) == len(manifest["artifacts"]), "Duplicate artifact manifest paths")
    docs, questions, hashes = {}, [], {"manifest.json": sha(manifest_path.read_bytes())}
    for split in splits:
        require(split in manifest["splits"], "Split missing from manifest")
        prefix = split + "/"
        require(prefix + "documents.jsonl" in files and prefix + "questions.jsonl" in files, "Split label files missing from artifact manifest")
        for relative, item in files.items():
            if relative.startswith(prefix):
                path = within(root, relative)
                require(path.is_file() and sha(path.read_bytes()) == item["sha256"], f"Dataset hash mismatch: {relative}")
                hashes[relative] = item["sha256"]
        for doc in jsonl(root / split / "documents.jsonl"):
            require(doc["doc_id"] not in docs, "Duplicate document ID or overlapping splits")
            relative = prefix + doc["path"]
            require(relative in files, "Document missing from artifact manifest")
            blob = within(root, relative).read_bytes()
            require(sha(blob) == doc["sha256"] and len(blob) == doc["bytes"], "Document metadata mismatch")
            blob.decode("utf-8")
            docs[doc["doc_id"]] = dict(doc, blob=blob, input_path=within(root, relative), split=split)
        questions.extend(jsonl(root / split / "questions.jsonl"))
    require(len({q["question_id"] for q in questions}) == len(questions), "Duplicate question IDs")
    require(docs and questions, "Selected dataset has no documents or questions")
    for question in questions:
        require(question["answerability"] in ("answerable", "unanswerable"), "Invalid answerability")
        require(question["doc_id"] in docs, "Question document missing")
        refs = question["reference_answers_any_of"]
        require(refs, "Question has no reference annotations")
        for ref in refs:
            require(type(ref["answer"]["unanswerable"]) is bool and ref["answer"]["unanswerable"] == (question["answerability"] == "unanswerable"), "Answerability disagreement or non-Boolean label")
            evidence = ref["required_evidence_all_of"]
            require(question["answerability"] == "unanswerable" or evidence, "Answerable question lacks evidence")
            for group in evidence + ref["highlighted_evidence"]:
                require(group["doc_id"] in docs, "Evidence document missing")
                if group in evidence:
                    require(group["locations_any_of"], "Gold evidence has no mapped location")
                for location in group["locations_any_of"]:
                    validate_location(docs[group["doc_id"]]["blob"], location)
    return manifest, docs, questions, hashes


def invoke(binary, wiki, args, output, label, timeout):
    command = [str(binary), "--wiki", str(wiki), "--offline", "--json", *args]
    start = time.monotonic()
    try:
        result = subprocess.run(command, capture_output=True, timeout=timeout, check=False)
        stdout, stderr, code = result.stdout, result.stderr, result.returncode
    except subprocess.TimeoutExpired as error:
        stdout, stderr, code = error.stdout or b"", error.stderr or b"", None
    elapsed = time.monotonic() - start
    (output / (label + ".stdout")).write_bytes(stdout)
    (output / (label + ".stderr")).write_bytes(stderr)
    save(output / (label + ".invocation.json"), {"argv": command, "exit_code": code, "seconds": elapsed,
                                                "stdout_sha256": sha(stdout), "stderr_sha256": sha(stderr)})
    require(code == 0, f"Command failed/timed out: {label}; retained stdout/stderr")
    envelope = json.loads(stdout)
    require(envelope.get("ok") is True, f"Command returned an error: {label}")
    require(envelope.get("meta", {}).get("network_used") is False, "Offline command did not confirm no network")
    return envelope, elapsed


def check_mapping(mapping, wiki, docs):
    require(mapping["wiki"] == str(wiki.resolve()), "Mapping belongs to another wiki")
    rows = mapping["documents"]
    require(set(rows) == set(docs), "Mapping document set differs from selected dataset splits")
    require(len({row["source_id"] for row in rows.values()}) == len(rows)
            and len({row["revision_id"] for row in rows.values()}) == len(rows), "Duplicate mapped source/revision identity")
    for doc_id, doc in docs.items():
        row = rows[doc_id]
        content = within(wiki, row["vault_content_path"])
        require(content.read_bytes() == doc["blob"], "Mapped source differs from dataset bytes")
        source = within(wiki, "sources/" + row["source_id"] + "/source.md")
        match = re.search(r'^wiki_current_revision: "([^"\n]+)"$', source.read_text(encoding="utf-8"), re.M)
        require(match and match.group(1) == row["revision_id"], "Mapped revision is no longer current")


def group_coverage(groups, intervals):
    total, found, complete, unmapped = 0, 0, 0, 0
    for group in groups:
        options = group["locations_any_of"]
        if not options:
            unmapped += 1
            continue
        # Repeated occurrences are alternative locations; do not require every copy.
        scores = [(covered(loc["start_byte"], loc["end_byte"], intervals.get(group["doc_id"], [])),
                   loc["end_byte"] - loc["start_byte"]) for loc in options]
        count, size = max(scores, key=lambda pair: pair[0] / pair[1])
        total += size
        found += count
        complete += count == size
    return {"byte_coverage": found / total if total else None, "covered_bytes": found, "gold_bytes": total,
            "complete_groups": complete, "groups": len(groups), "unmapped_groups": unmapped,
            "all_groups_complete": bool(groups) and complete == len(groups)}


def citation_intervals(data, mapping, docs, hash_binary, output, label, timeout):
    by_revision = {row["revision_id"]: (doc_id, row) for doc_id, row in mapping["documents"].items()}
    by_path = {row["vault_content_path"]: doc_id for doc_id, row in mapping["documents"].items()}
    intervals, count = {}, 0
    for passage in data["passages"]:
        require(passage["text"] in data["text"], "Passage text absent from rendered context")
        require(passage["citations"], "Passage has no exact citation")
        owner = by_path.get(passage["locator"]["path"])
        require(owner is not None, "Passage locator absent from frozen source mapping")
        owner_blob = docs[owner]["blob"]
        passage_start, passage_end = passage["span"]["start"], passage["span"]["end"]
        require(type(passage_start) is int and type(passage_end) is int and 0 <= passage_start < passage_end <= len(owner_blob), "Passage span out of bounds")
        passage_bytes = passage["text"].encode("utf-8")
        require(owner_blob[passage_start:passage_end] == passage_bytes, "Whole passage differs from original source bytes")
        for citation in passage["citations"]:
            require(citation["kind"] == "source", "Unexpected non-source citation in source-only benchmark")
            ref = citation["reference"]
            require(ref["source_revision"] in by_revision, "Citation revision not in frozen mapping")
            doc_id, row = by_revision[ref["source_revision"]]
            require(ref["source_id"] == row["source_id"], "Citation source/revision mismatch")
            span = ref["span"]
            start, end = span["start"], span["end"]
            blob = docs[doc_id]["blob"]
            require(type(start) is int and type(end) is int and 0 <= start < end <= len(blob), "Citation out of bounds")
            require(passage_start <= start < end <= passage_end, "Citation span outside containing passage")
            quote = blob[start:end]
            require(quote == passage_bytes[start - passage_start:end - passage_start], "Citation quote differs from original UTF-8 bytes")
            helper_label = f"{label}-citation-{count:04d}"
            helper_start = time.monotonic()
            try:
                result = subprocess.run([str(hash_binary)], input=quote, capture_output=True, timeout=timeout, check=False)
                helper_stdout, helper_stderr, helper_code = result.stdout, result.stderr, result.returncode
            except subprocess.TimeoutExpired as error:
                helper_stdout, helper_stderr, helper_code = error.stdout or b"", error.stderr or b"", None
            (output / (helper_label + ".stdout")).write_bytes(helper_stdout)
            (output / (helper_label + ".stderr")).write_bytes(helper_stderr)
            save(output / (helper_label + ".invocation.json"), {"argv": [str(hash_binary)], "exit_code": helper_code,
                                                               "seconds": time.monotonic() - helper_start,
                                                               "stdin_quote_sha256": sha(quote), "stdout_sha256": sha(helper_stdout),
                                                               "stderr_sha256": sha(helper_stderr)})
            require(helper_code == 0 and helper_stdout.decode().strip() == ref["quote_hash"], "BLAKE3 citation hash mismatch or helper failure")
            intervals.setdefault(doc_id, []).append((start, end))
            count += 1
    return {doc_id: union(spans) for doc_id, spans in intervals.items()}, count


def provenance(args, manifest, hashes):
    return {"schema_version": 1, "dataset": manifest["dataset"], "license": manifest["license"],
            "splits": args.split, "binary": str(args.binary), "binary_sha256": sha(args.binary.read_bytes()),
            "dataset_input_sha256": hashes, "network_policy": "every lwiki invocation has --offline; no provider setup or credential reads",
            "semantic_completeness": "not assessed; byte-location coverage is not entailment"}


def strict_json(blob):
    def pairs(items):
        result = {}
        for key, value in items:
            require(key not in result, "Duplicate JSON key")
            result[key] = value
        return result
    text = blob.decode("utf-8") if isinstance(blob, bytes) else blob
    return json.loads(text, object_pairs_hook=pairs)


def validate_selection_reply(blob):
    require(len(blob) <= SELECTION_REPLY_MAX_BYTES, "Selection reply exceeds 4096 bytes")
    reply = strict_json(blob)
    require(type(reply) is dict and set(reply) == {"packet_fingerprint", "ordered_ids"}, "Selection reply fields differ from strict schema")
    require(isinstance(reply["packet_fingerprint"], str) and re.fullmatch(r"blake3:[0-9a-f]{64}", reply["packet_fingerprint"]), "Invalid packet fingerprint")
    ids = reply["ordered_ids"]
    require(type(ids) is list and len(ids) <= 20, "Selection reply exceeds 20 IDs or lacks ID array")
    require(all(isinstance(item, str) and re.fullmatch(r"[A-Za-z0-9_-]{1,128}", item) for item in ids), "Invalid selection ID")
    require(len(set(ids)) == len(ids), "Duplicate selection IDs")
    return reply


def read_selection(path):
    # Bound even rejected input; preserve the exact prefix instead of reading an
    # arbitrary external file into memory or silently trimming it into a reply.
    with path.open("rb") as stream:
        blob = stream.read(SELECTION_REPLY_MAX_BYTES + 1)
    return blob


def context_plan(args, mapping, question):
    query = question["original_question"] if args.query_style == "original" else question["query"]
    plan = ["--mode", args.mode, "--limit", str(args.limit), "--candidates", str(args.candidates),
            "--excerpt-bytes", str(args.excerpt_bytes)]
    if args.paper_local:
        plan += ["--source-id", mapping["documents"][question["doc_id"]]["source_id"]]
    context = ["context", query, *plan, "--max-bytes", str(args.max_bytes), "--max-tokens", str(args.max_tokens),
               "--verification-max-entries", str(args.verification_max_entries)]
    if getattr(args, "verification_max_elapsed_ms", None) is not None:
        context += ["--verification-max-elapsed-ms", str(args.verification_max_elapsed_ms)]
    return query, plan, context


def selection_metadata(args):
    return {"workflow": "host-assisted-selection-replay", "selector_invocations": None,
            "selector_actual_tokens": None, "selector_cost": None, "end_to_end_seconds": None,
            "unavailable_reason": "external selector execution/usage is not observed by this runner; join separately retained orchestration records",
            "local_context_latency_scope": "final local validation/packing subprocess only; excludes preparation, external selection and runner citation checks",
            "cost": "embedding acquisition and external selector cost are recorded separately; unavailable in this runner"}


def evaluation_metadata(args):
    budgets = {"bytes": args.max_bytes, "tokens": args.max_tokens, "limit": args.limit,
               "candidates": args.candidates, "excerpt_bytes": args.excerpt_bytes,
               "verification_max_entries": args.verification_max_entries}
    if getattr(args, "verification_max_elapsed_ms", None) is not None:
        budgets["verification_max_elapsed_ms"] = args.verification_max_elapsed_ms
    return {"mode": args.mode, "query_style": args.query_style, "paper_local": args.paper_local, "budgets": budgets}


def load_mapping(args, docs, hashes):
    mapping_bytes = args.mapping.read_bytes()
    mapping = json.loads(mapping_bytes)
    require(mapping["dataset_manifest_sha256"] == hashes["manifest.json"], "Mapping dataset hash mismatch")
    check_mapping(mapping, args.wiki, docs)
    return mapping, sha(mapping_bytes)


def verify_inputs(args, metadata, docs, mapping, hashes):
    check_mapping(mapping, args.wiki, docs)
    require(sha(args.binary.read_bytes()) == metadata["binary_sha256"], "Binary changed during evaluation")
    require(sha(args.hash_binary.read_bytes()) == metadata["hash_binary_sha256"], "Hash helper changed during evaluation")
    require(sha(args.mapping.read_bytes()) == metadata["mapping_sha256"], "Mapping changed during evaluation")
    require(load_dataset(args.dataset, args.split)[3] == hashes, "Dataset changed during evaluation")


def validate_compact_cards(payload, args, mapping, docs):
    """Audit displayed v2 evidence, not the unavailable full citation authority."""
    commitment = payload["authority_commitment"]
    require(isinstance(commitment, str) and re.fullmatch(r"blake3:[0-9a-f]{64}", commitment),
            "Invalid selector authority commitment")
    sources = payload["sources"]
    require(type(sources) is list, "Selector sources must be an array")
    require(len(sources) <= 80, "Selector source count exceeds bound")
    by_path = {row["vault_content_path"]: doc_id for doc_id, row in mapping["documents"].items()}
    by_source, owner_content, path_owner = {}, {}, {}
    for source in sources:
        require(type(source) is dict and set(source) == {"id", "owner", "title", "path", "label", "eligibility"},
                "Compact selector source fields differ from v2 schema")
        for field in ("id", "owner"):
            require(isinstance(source[field], str) and re.fullmatch(r"[A-Za-z0-9_-]{1,128}", source[field]),
                    "Invalid selector source or owner ID")
        require(source["id"] not in by_source, "Duplicate selector source ID")
        require(isinstance(source["title"], str) and len(source["title"].encode("utf-8")) <= 4096,
                "Selector source title exceeds UTF-8 byte bound")
        require(isinstance(source["path"], str) and source["path"] in by_path,
                "Selector source path absent from frozen mapping")
        require(source["label"] == "captured_source" and source["eligibility"] == "current",
                "Selector source is not a current captured source")
        blob = docs[by_path[source["path"]]]["blob"]
        owner = source["owner"]
        # Identical captured content can share a canonical owner across paths.
        # Opaque display aliases must not merge unrelated documents to evade
        # the owner bound, or split one path into multiple owners.
        require(owner not in owner_content or owner_content[owner] == blob,
                "Selector owner merges different frozen content")
        require(source["path"] not in path_owner or path_owner[source["path"]] == owner,
                "Selector source path has inconsistent owners")
        owner_content[owner] = blob
        path_owner[source["path"]] = owner
        by_source[source["id"]] = blob
    require(len(owner_content) <= args.limit, "Selector owner bound exceeded")
    referenced = set()
    for card in payload["cards"]:
        require(set(card) == {"id", "source", "span", "child_span", "text", "rendered_bytes"},
                "Compact selector card fields differ from v2 schema")
        require(isinstance(card["source"], str) and card["source"] in by_source,
                "Selector card refers to an unknown source")
        referenced.add(card["source"])
        blob = by_source[card["source"]]
        span = card["span"]
        require(type(span) is dict and set(span) == {"start", "end"}, "Invalid selector passage span")
        start, end = span["start"], span["end"]
        require(type(start) is int and type(end) is int and 0 <= start < end <= len(blob),
                "Selector passage span out of bounds")
        require(isinstance(card["text"], str), "Selector passage text is not a string")
        text = card["text"].encode("utf-8")
        require(len(text) <= args.excerpt_bytes and len(text) <= 2048,
                "Selector passage exceeds excerpt bound")
        require(blob[start:end] == text, "Selector passage differs from original UTF-8 bytes")
        require(type(card["rendered_bytes"]) is int and len(text) <= card["rendered_bytes"] <= SELECTOR_INPUT_MAX_BYTES,
                "Invalid selector standalone rendered byte estimate")
        child = card["child_span"]
        if child is not None:
            require(type(child) is dict and set(child) == {"start", "end"}, "Invalid selector child span")
            left, right = child["start"], child["end"]
            require(type(left) is int and type(right) is int and start <= left < right <= end,
                    "Selector child span outside passage")
            try:
                blob[left:right].decode("utf-8")
            except UnicodeDecodeError as error:
                raise ValueError("Selector child span splits a UTF-8 code point") from error
    require(referenced == set(by_source), "Selector source table contains unused sources")


def validate_preparation(envelope, query, args, mapping, docs, label):
    data = envelope["data"]
    require(data["network_used"] is False and envelope["meta"]["freshness"] == "verified_snapshot"
            and data["verification"]["mode"] == "verified_snapshot", "Selection preparation is not verified offline")
    require(not data["passages"] and data["text"] == "", "Preparation unexpectedly emitted final context")
    packet = data["selection_packet"]
    require(isinstance(packet["selector_input"], str), "Selector input is not UTF-8 task text")
    task = packet["selector_input"].encode("utf-8")
    require(type(packet["input_bytes"]) is int and packet["input_bytes"] == len(task) <= SELECTOR_INPUT_MAX_BYTES, "Selector input byte bound mismatch")
    require(type(packet["estimated_tokens"]) is int and packet["estimated_tokens"] == (len(task) + 3) // 4, "Selector estimated token mismatch")
    require(type(packet["candidate_count"]) is int and 0 <= packet["candidate_count"] <= 80, "Selector candidate count mismatch")
    require(type(packet["omitted_candidates"]) is int and packet["omitted_candidates"] >= 0, "Invalid omitted candidate count")
    parsed = strict_json(task)
    require(type(parsed) is dict and type(parsed.get("payload")) is dict, "Invalid selector task payload")
    require(parsed["packet_fingerprint"] == packet["fingerprint"] and isinstance(packet["fingerprint"], str)
            and re.fullmatch(r"blake3:[0-9a-f]{64}", packet["fingerprint"]), "Selector packet fingerprint mismatch")
    payload = parsed["payload"]
    version = payload.get("version")
    require(version in ("lwiki.context-selection.v1", "lwiki.context-selection.v2"), "Unsupported selector payload version")
    require(payload["binding"]["query"] == query, "Selector task belongs to another query")
    cards = payload["cards"]
    require(type(cards) is list and all(type(card) is dict and isinstance(card.get("id"), str)
            and re.fullmatch(r"[A-Za-z0-9_-]{1,128}", card["id"]) for card in cards), "Invalid selector card IDs")
    require(len(cards) == packet["candidate_count"] and len({card["id"] for card in cards}) == len(cards), "Selector card count or ID mismatch")
    if version == "lwiki.context-selection.v2":
        validate_compact_cards(payload, args, mapping, docs)
        return packet, task
    passages = [card["passage"] for card in cards]
    require(len({p["locator"]["path"] for p in passages}) <= args.limit, "Selector owner bound exceeded")
    for passage in passages:
        require(len(passage["text"].encode("utf-8")) <= args.excerpt_bytes, "Selector passage exceeds excerpt bound")
    citation_intervals({"passages": passages, "text": "\n".join(p["text"] for p in passages)},
                       mapping, docs, args.hash_binary, args.output, label + "-packet", args.timeout)
    return packet, task


def run_prepare_selection(args, manifest, docs, questions, hashes):
    require(args.hash_binary.is_file(), "A prebuilt fixture_hash helper is required")
    mapping, mapping_hash = load_mapping(args, docs, hashes)
    args.output.mkdir(parents=True, exist_ok=False)
    task_dir = args.output / "selector-inputs"
    task_dir.mkdir()
    metadata = provenance(args, manifest, hashes)
    metadata.update(evaluation_metadata(args))
    metadata.update({"mapping_sha256": mapping_hash, "hash_binary_sha256": sha(args.hash_binary.read_bytes()),
                     "workflow": "host-selection-preparation-only", "selector_task_max_bytes": SELECTOR_INPUT_MAX_BYTES,
                     "transport_wrapper_reserved_bytes": SELECTOR_WRAPPER_RESERVED_BYTES,
                     "selector_usage": "not observed; this command does not dispatch selectors"})
    save(args.output / "run.json", metadata)
    tasks = []
    for index, question in enumerate(questions):
        started = time.monotonic()
        label = f"question-{index:04d}"
        row = {"question_id": question["question_id"], "reply_filename": label + ".json"}
        try:
            query, _, context = context_plan(args, mapping, question)
            prepared, seconds = invoke(args.binary, args.wiki, [*context, "--prepare-selection"], args.output, label + "-prepare", args.timeout)
            packet, task = validate_preparation(prepared, query, args, mapping, docs, label)
            task_path = task_dir / (label + ".txt")
            task_path.write_bytes(task)  # Exact product task only; no labels or runner wrapper.
            row.update({"status": "ok", "task_path": str(task_path.relative_to(args.output)), "task_sha256": sha(task),
                        "packet_fingerprint": packet["fingerprint"], "candidate_count": packet["candidate_count"],
                        "input_bytes": packet["input_bytes"], "estimated_tokens": packet["estimated_tokens"],
                        "omitted_candidates": packet["omitted_candidates"], "preparation_cli_seconds": seconds})
        except (ValueError, KeyError, TypeError, OSError, subprocess.SubprocessError) as error:
            row.update({"status": "error", "error": str(error)})
        row["preparation_seconds"] = time.monotonic() - started
        tasks.append(row)
        save(args.output / "tasks.json", tasks)
    verify_inputs(args, metadata, docs, mapping, hashes)
    summary = {"queries": len(tasks), "prepared": sum(row["status"] == "ok" for row in tasks),
               "errors": sum(row["status"] == "error" for row in tasks), "selectors_dispatched_by_runner": 0,
               "application_task_bytes": sum(row.get("input_bytes", 0) for row in tasks),
               "application_task_estimated_tokens": sum(row.get("estimated_tokens", 0) for row in tasks),
               "actual_model_tokens": None, "model_cost": None, "end_to_end_seconds": None}
    save(args.output / "summary.json", summary)
    output_hashes(args.output)
    print(json.dumps(summary))
    return 1 if summary["errors"] else 0


def output_hashes(output):
    files = [p for p in output.rglob("*") if p.is_file() and p != output / "output-hashes.json"]
    save(output / "output-hashes.json", {str(p.relative_to(output)): sha(p.read_bytes()) for p in sorted(files)})


def summarize(rows):
    positive = [row for row in rows if row["answerability"] == "answerable"]
    success = [row for row in rows if row["status"] == "ok"]
    highlighted = [row for row in positive if row.get("best_answer_highlight_byte_coverage") is not None]
    eligible = [row for row in positive if row.get("answer_highlight_eligible") is True]
    n = len(positive)
    return {"queries": len(rows), "answerable": n, "unanswerable": len(rows) - n,
            "errors": len(rows) - len(success), "source_hit_at_1": sum(r.get("source_rank") == 1 for r in positive) / n if n else None,
            "source_hit_at_limit": sum(r.get("source_rank") is not None for r in positive) / n if n else None,
            "strict_gold_span_complete_rate": sum(r.get("strict_gold_span_complete") is True for r in positive) / n if n else None,
            "mean_gold_span_byte_coverage_errors_zero": sum(r.get("best_gold_span_byte_coverage", 0) for r in positive) / n if n else None,
            "answer_highlight_eligible_queries": len(eligible), "answer_highlight_scored_queries": len(highlighted),
            "mean_answer_highlight_byte_coverage_errors_zero": sum(r.get("best_answer_highlight_byte_coverage") or 0 for r in eligible) / len(eligible) if eligible else None,
            "strict_answer_highlight_complete_rate_errors_zero": sum(r.get("strict_answer_highlight_complete") is True for r in eligible) / len(eligible) if eligible else None,
            "mean_answer_highlight_byte_coverage_scored_queries": statistics.mean(r["best_answer_highlight_byte_coverage"] for r in highlighted) if highlighted else None,
            "strict_answer_highlight_complete_scored_queries": sum(r["strict_answer_highlight_complete"] for r in highlighted) / len(highlighted) if highlighted else None,
            "context_latency_median_seconds": statistics.median(r["context_seconds"] for r in success) if success else None,
            "verified_source_citations": sum(r["citation_count"] for r in success),
            "semantic_complete_rate": None, "semantic_complete_rate_reason": "requires independent proposition/entailment assessment",
            "cost": "no provider dispatch; remote embedding acquisition cost is separately recorded, not inferred as zero"}


def run_import(args, manifest, docs, hashes):
    require(not args.wiki.exists(), "Import wiki must be a new path")
    args.output.mkdir(parents=True, exist_ok=False)
    metadata = provenance(args, manifest, hashes)
    save(args.output / "run.json", metadata)
    invoke(args.binary, args.wiki, ["init", str(args.wiki)], args.output, "init", args.timeout)
    rows = {}
    for index, (doc_id, doc) in enumerate(sorted(docs.items())):
        envelope, _ = invoke(args.binary, args.wiki, ["source", "add", str(doc["input_path"]), "--title", doc["title"]],
                             args.output, f"import-{index:04d}", args.timeout)
        ids = envelope["data"]["allocated_ids"]
        source_id, revision_id = ids["source"], ids["revision"]
        rows[doc_id] = {"source_id": source_id, "revision_id": revision_id,
                       "vault_content_path": f"sources/{source_id}/revisions/{revision_id}/content.md",
                       "sha256": doc["sha256"]}
    mapping = {"schema_version": 1, "dataset": manifest["dataset"], "wiki": str(args.wiki),
               "dataset_manifest_sha256": hashes["manifest.json"], "splits": args.split, "documents": rows}
    check_mapping(mapping, args.wiki, docs)
    save(args.output / "mapping.json", mapping)
    require(sha(args.binary.read_bytes()) == metadata["binary_sha256"], "Binary changed during import")
    output_hashes(args.output)
    print(json.dumps({"documents_imported": len(rows), "mapping": str(args.output / "mapping.json")}))


def run_evaluate(args, manifest, docs, questions, hashes):
    require(args.hash_binary.is_file(), "A prebuilt fixture_hash helper is required")
    mapping, mapping_hash = load_mapping(args, docs, hashes)
    args.output.mkdir(parents=True, exist_ok=False)
    metadata = provenance(args, manifest, hashes)
    metadata.update({"mapping_sha256": mapping_hash, "hash_binary_sha256": sha(args.hash_binary.read_bytes())})
    metadata.update(evaluation_metadata(args))
    selection_dir = getattr(args, "selection_dir", None)
    selections = []
    if selection_dir is not None:
        metadata.update(selection_metadata(args))
        metadata["selection_dir"] = str(selection_dir)
        for index, question in enumerate(questions):
            label = f"question-{index:04d}"
            info = {"question_id": question["question_id"], "filename": label + ".json"}
            blob, error = None, None
            try:
                path = within(selection_dir, info["filename"])
                blob = read_selection(path)
                retained = args.output / (label + "-selection.json")
                retained.write_bytes(blob)
                info.update({"sha256": sha(blob), "retained_path": retained.name, "retained_bytes": len(blob),
                             "hash_scope": "exact_reply" if len(blob) <= SELECTION_REPLY_MAX_BYTES else "oversize_prefix_only"})
                reply = validate_selection_reply(blob)
                info.update({"packet_fingerprint": reply["packet_fingerprint"], "selected_ids": len(reply["ordered_ids"])})
            except (ValueError, TypeError, OSError, UnicodeError) as failure:
                error = str(failure)
                info["error"] = error
            selections.append((blob, error, info))
        metadata["selection_inputs"] = [item[2] for item in selections]
    save(args.output / "run.json", metadata)
    rows = []
    for index, question in enumerate(questions):
        row = {"question_id": question["question_id"], "split": question["split"], "answerability": question["answerability"],
               "answer_highlight_eligible": question["answerability"] == "answerable" and any(
                   group["locations_any_of"] for ref in question["reference_answers_any_of"] for group in ref["highlighted_evidence"])}
        label = f"question-{index:04d}"
        query, plan, context_command = context_plan(args, mapping, question)
        try:
            if selection_dir is not None:
                _, error, info = selections[index]
                row["selection_input"] = info
                require(error is None, "Selection input rejected: " + str(error))
                context_command += ["--selection", str(args.output / info["retained_path"])]
            search, search_seconds = invoke(args.binary, args.wiki, ["search", query, *plan], args.output, label + "-search", args.timeout)
            context, seconds = invoke(args.binary, args.wiki, context_command,
                                       args.output, label + "-context", args.timeout)
            data = context["data"]
            require(data["network_used"] is False, "Context used network")
            require(context["meta"]["freshness"] == "verified_snapshot" and data["verification"]["mode"] == "verified_snapshot", "Context freshness unverified")
            rendered = len(data["text"].encode("utf-8"))
            usage = data["usage"]
            require(rendered == usage["rendered_bytes"] and rendered <= args.max_bytes, "Rendered byte budget mismatch")
            require(usage["token_accounting"] == "estimated_utf8_bytes_div4_ceil" and usage["estimated_tokens"] == (rendered + 3) // 4
                    and usage["estimated_tokens"] <= args.max_tokens, "Estimated token budget mismatch")
            intervals, citations = citation_intervals(data, mapping, docs, args.hash_binary, args.output, label, args.timeout)
            source = mapping["documents"][question["doc_id"]]["source_id"]
            found = [hit.get("source_id") for hit in search["data"]["hits"]]
            rank = next((i + 1 for i, sid in enumerate(found) if sid == source), None)
            answerable = question["answerability"] == "answerable"
            ref_scores = [{"annotation_id": ref["annotation_id"],
                           "gold_evidence": group_coverage(ref["required_evidence_all_of"], intervals),
                           "answer_highlights": group_coverage(ref["highlighted_evidence"], intervals)}
                          for ref in question["reference_answers_any_of"]] if answerable else []
            row.update({"status": "ok", "query": query, "source_rank": rank if answerable else None,
                        "context_source_hit": question["doc_id"] in intervals if answerable else None,
                        "strict_gold_span_complete": any(r["gold_evidence"]["all_groups_complete"] for r in ref_scores) if answerable else None,
                        "best_gold_span_byte_coverage": max(r["gold_evidence"]["byte_coverage"] for r in ref_scores) if answerable else None,
                        "best_answer_highlight_byte_coverage": max((r["answer_highlights"]["byte_coverage"] for r in ref_scores
                                                                   if r["answer_highlights"]["byte_coverage"] is not None), default=None),
                        "strict_answer_highlight_complete": any(r["answer_highlights"]["all_groups_complete"] for r in ref_scores) if answerable else None,
                        "reference_location_scores": ref_scores, "citation_count": citations, "context_bytes": rendered,
                        "estimated_tokens": usage["estimated_tokens"], "search_seconds": search_seconds, "context_seconds": seconds,
                        "omissions": data["omissions"], "warnings": data["warnings"], "truncated": data["truncated"],
                        "negative_control": "no answer-support judgment; returned neighbors are not evidence of answerability" if not answerable else None})
        except (ValueError, KeyError, TypeError, OSError, subprocess.SubprocessError, json.JSONDecodeError) as error:
            row.update({"status": "error", "error": str(error)})
        rows.append(row)
        save(args.output / "results.json", rows)
    verify_inputs(args, metadata, docs, mapping, hashes)
    if selection_dir is not None:
        for blob, _, info in selections:
            try:
                current = read_selection(within(selection_dir, info["filename"]))
            except (OSError, ValueError):
                current = None
            require(current == blob, "External selection input changed during evaluation")
            if "retained_path" in info:
                require(sha((args.output / info["retained_path"]).read_bytes()) == info["sha256"], "Retained selection input changed during evaluation")
    summary = summarize(rows)
    if selection_dir is not None:
        summary.update(selection_metadata(args))
    save(args.output / "summary.json", summary)
    output_hashes(args.output)
    print(json.dumps(summary))
    return 1 if summary["errors"] else 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("import", "evaluate", "prepare-selection"):
        command = commands.add_parser(name)
        command.add_argument("--binary", type=Path, required=True)
        command.add_argument("--dataset", type=Path, required=True, help="Root containing manifest.json and split directories")
        command.add_argument("--split", action="append", choices=("train", "dev", "test"), help="Repeat to select disjoint splits; default dev")
        command.add_argument("--wiki", type=Path, required=True)
        command.add_argument("--output", type=Path, required=True, help="New output directory")
        command.add_argument("--timeout", type=float, default=60)
        if name != "import":
            command.add_argument("--mapping", type=Path, required=True)
            command.add_argument("--hash-binary", type=Path, required=True)
            command.add_argument("--mode", choices=("lexical", "hybrid", "semantic"), default="lexical")
            command.add_argument("--query-style", choices=("prefixed", "original"), default="prefixed")
            command.add_argument("--paper-local", action="store_true", help="Restrict to designated paper; measure passage selection separately")
            command.add_argument("--verification-max-elapsed-ms", type=int, help="Explicit proof deadline; use identically in preparation and replay; otherwise binary default")
            if name == "evaluate":
                command.add_argument("--selection-dir", type=Path, help="External strict replies named question-0000.json etc; missing/invalid replies count as failures")
            for flag, default in (("max-bytes", 6000), ("max-tokens", 1500), ("limit", 5), ("candidates", 80),
                                  ("excerpt-bytes", 1024), ("verification-max-entries", 65536)):
                command.add_argument("--" + flag, type=int, default=default)
    args = parser.parse_args(argv)
    args.split = args.split or ["dev"]
    require(len(set(args.split)) == len(args.split), "Repeated split")
    for name in ("binary", "wiki", "output", "dataset", "mapping", "hash_binary", "selection_dir"):
        if getattr(args, name, None) is not None:
            setattr(args, name, getattr(args, name).resolve())
    require(args.binary.is_file(), "CLI binary missing")
    require(args.timeout > 0, "Timeout must be positive")
    manifest, docs, questions, hashes = load_dataset(args.dataset, args.split)
    if args.command == "import":
        run_import(args, manifest, docs, hashes)
        return 0
    if args.command == "prepare-selection":
        return run_prepare_selection(args, manifest, docs, questions, hashes)
    return run_evaluate(args, manifest, docs, questions, hashes)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, KeyError, OSError, json.JSONDecodeError) as error:
        print(f"context_eval: {error}", file=sys.stderr)
        sys.exit(1)
