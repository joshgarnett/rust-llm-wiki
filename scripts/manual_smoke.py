#!/usr/bin/env python3
"""Exercise lwiki's offline regressions in a new disposable vault.

No credentials, provider requests, host installation, or cleanup of existing data.
The retained JSON log and vault make any failure inspectable.
"""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", type=Path, help="New, nonexistent test directory; default: a temporary directory")
    args = parser.parse_args()
    binary = args.binary.expanduser().resolve(strict=True)
    if args.output:
        root = args.output.expanduser().resolve()
        root.mkdir(parents=True, exist_ok=False)
    else:
        root = Path(tempfile.mkdtemp(prefix="lwiki-manual-"))
    vault = root / "vault"
    log = root / "commands.jsonl"
    count = 0

    def run(*words, stdin=None, error=None):
        nonlocal count
        data = stdin if isinstance(stdin, str) else json.dumps(stdin) if stdin is not None else None
        command = [str(binary), "--offline", "--json", "--wiki", str(vault), *map(str, words)]
        process = subprocess.run(command, input=data, text=True, capture_output=True, check=False)
        try:
            result = json.loads(process.stdout)
        except ValueError as exc:
            raise AssertionError(f"Invalid JSON for {words}: {process.stdout!r} {process.stderr!r}") from exc
        with log.open("a", encoding="utf-8") as stream:
            stream.write(json.dumps({"args": words, "exit": process.returncode, "result": result}, default=str) + "\n")
        assert result["meta"]["network_used"] is False, words
        if error:
            assert process.returncode != 0 and result["error"]["code"] == error, (words, result)
        else:
            assert process.returncode == 0 and result["ok"], (words, result, process.stderr)
        count += 1
        return result.get("data")

    def apply(data):
        return run("changes", "apply", data["prepared"]["change_id"])

    def current_hash(record):
        return run("read", "--id", record)["hash"]

    run("init", vault)
    text = "Project Cedar uses Rust. Exact identifier CEDAR-731.\nMira owns the backup procedure. Backups run every Friday.\n"
    source = run("source", "add", "-", "--title", "Cedar notes", stdin=text)["allocated_ids"]["source"]
    run("source", "refresh", source, "--file", "-", stdin=text + "\nThe backup destination is ARCHIVE-928.\n")
    packet = run("graph", "extract", "--executor", "agent", "--source-id", source,
                 "--max-mentions", "4", "--max-assertions", "2", "--max-output-bytes", "16000")["packet"]
    mentions = [
        {"id": "m1", "window_id": "w1", "label": "Cedar", "type": "project", "quote": "Project Cedar", "span": {"start": 0, "end": 13}},
        {"id": "m2", "window_id": "w1", "label": "Rust", "type": "concept", "quote": "Rust", "span": {"start": 19, "end": 23}},
    ]
    extraction = {"schema": "lwiki.extraction.v1", "packet_id": packet["packet_id"], "packet_fingerprint": packet["packet_fingerprint"],
                  "mentions": mentions, "assertions": [{"id": "a1", "subject": "m1", "predicate": "uses", "object": {"kind": "mention", "mention_id": "m2"},
                  "negated": False, "modality": "asserted", "evidence": [{"window_id": "w1", "stance": "supports", "quote": "Project Cedar uses Rust."}]}], "unresolved": []}
    imported = run("graph", "import", "--file", "-", stdin=extraction)
    apply(imported)
    extraction_id = imported["extraction"]["record"]["record_id"]
    assertion = imported["allocations"]["assertions"]["a1"]
    evidence = imported["allocations"]["evidence"]["a1"][0]
    resolved = run("graph", "resolve", "--file", "-", stdin={"schema": "lwiki.graph-resolution.v1", "extraction_id": extraction_id,
                   "expected_hash": current_hash(extraction_id), "mappings": [{"operation": "CreateEntity", "mention_id": m["id"],
                   "reason": "Disposable regression fixture", "title": m["label"], "entity_type": m["type"]} for m in mentions]})
    apply(resolved)
    reviewed = run("graph", "review", "--file", "-", stdin={"schema": "lwiki.graph-review.v1", "decisions": [{"assertion_id": assertion,
                   "expected_hash": current_hash(assertion), "decision": "accept", "reason": "Checked exact current quotation",
                   "evidence_checks": [{"evidence_id": evidence, "expected_hash": current_hash(evidence), "assessment": "supports"}]}], "supersedes": []})
    apply(reviewed)
    assert len(run("graph", "query", "Cedar", "--strategy", "relationship")["assertions"]) == 1
    assert len(run("search", "CEDAR-731", "--mode", "literal")["hits"]) == 1

    planned = run("research", "plan", "What does Cedar use?", "--source-id", source)
    assert not planned["persisted"] and not planned["ready_to_import"]
    handoff = run("research", "run", "What does Cedar use?", "--source-id", source, "--max-rounds", "1", "--max-sources", "0")
    research_id = handoff["run_id"]
    task = handoff["packet"]
    assert task["scope"]["offline"] and handoff["ready_to_import"]
    assert run("research", "resume", research_id)["packet"]["packet_fingerprint"] == task["packet_fingerprint"]
    collection = {"schema": "lwiki.research-submission.v1", "run_id": research_id, "packet_fingerprint": task["packet_fingerprint"],
                  "response": {"stage": "collect_sources", "sources": [], "gaps": []}}
    answering = run("research", "import", "--file", "-", stdin=collection)
    assert run("research", "import", "--file", "-", stdin=collection)["reused"]
    answer_packet = answering["packet"]
    answer = {"schema": "lwiki.research-submission.v1", "run_id": research_id, "packet_fingerprint": answer_packet["packet_fingerprint"],
              "response": {"stage": "answer", "claims": [{"text": "Project Cedar uses Rust.", "passage_ids": [answer_packet["passages"][0]["passage_id"]]}],
              "gaps": [], "follow_up": None}}
    bad = json.loads(json.dumps(answer))
    bad["response"]["claims"][0]["passage_ids"] = [answer_packet["passages"][0]["citation"]["reference"]["quote_hash"]]
    run("research", "import", "--file", "-", stdin=bad, error="RECORD_INVALID")
    done = run("research", "import", "--file", "-", stdin=answer)
    assert done["status"] == "completed" and not done["ready_to_import"]
    report = run("research", "report", research_id)
    assert report["claims"][0]["assessment"] == "unassessed" and not report["partial"]
    assert run("research", "status", research_id)["imports"] == 2
    assert len(run("search", "CEDAR-731", "--mode", "literal")["hits"]) == 1

    run("source", "withdraw", source, "--reason", "Completed disposable regression fixture")
    assert run("search", "CEDAR-731", "--mode", "literal")["hits"] == []
    assert run("graph", "query", "Cedar", "--strategy", "relationship")["assertions"] == []
    historical = run("search", "CEDAR-731", "--mode", "literal", "--include-historical")["hits"]
    assert historical and all(hit["eligibility"] != "current" for hit in historical)
    assert run("check")["error_count"] == 0
    print(json.dumps({"ok": True, "commands_passed": count, "vault": str(vault), "log": str(log), "network_used": False}, indent=2))


if __name__ == "__main__":
    main()
