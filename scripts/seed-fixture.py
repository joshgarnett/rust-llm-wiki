#!/usr/bin/env python3
"""Generate/check the synthetic P00 vault using a stdin BLAKE3 helper.

Build the root-owned example, then run:
  python3 scripts/seed-fixture.py --hash-command target/debug/examples/fixture_hash
  python3 scripts/seed-fixture.py --hash-command target/debug/examples/fixture_hash --check
No network access or third-party Python packages are used.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shlex
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "tests/fixtures/bootstrap"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hash-command", default="target/debug/examples/fixture_hash")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    command = shlex.split(args.hash_command)

    def digest(data):
        result = subprocess.run(command, input=data, capture_output=True, check=True)
        value = result.stdout.decode("ascii").strip()
        if not re.fullmatch(r"blake3:[0-9a-f]{64}", value):
            raise ValueError("hash helper must output blake3:<64 lowercase hexadecimal digits>")
        return value

    ids = {}
    files = {}
    records = []

    def identifier(name, kind):
        # Fixed fixture allocation, unrelated to title/path/source bytes.
        if name not in ids:
            number = len(ids) + 1
            ids[name] = f"{kind}_00000000-0000-7000-8000-{number:012x}"
        return ids[name]

    def note(path, name, kind, title, fields, body):
        rid = identifier(name, kind)
        common = {"wiki_schema": "1", "wiki_id": rid, "wiki_kind": kind, "title": title}
        common.update({"wiki_" + key: value for key, value in fields.items()})
        # JSON scalar/list syntax is valid flat YAML and forces quoted strings.
        envelope = "---\n" + "".join(key + ": " + json.dumps(value, ensure_ascii=False) + "\n" for key, value in common.items()) + "---\n"
        files["vault/" + path] = (envelope + body).encode("utf-8")
        records.append({"id": rid, "kind": kind, "path": path, "name": name, "fields": common})
        return rid

    for name, kind in [("alex_north", "entity"), ("alex_south", "entity"),
                       ("north_lab", "entity"), ("south_lab", "entity"),
                       ("architecture", "source"), ("report", "source"),
                       ("architecture_v1", "revision"), ("architecture_v2", "revision"),
                       ("report_v1", "revision")]:
        identifier(name, kind)

    entities = [
        ("alex_north", "Alex Kim", "person", "The North Lab engineer."),
        ("alex_south", "Alex Kim", "person", "The South Lab archivist; a distinct person."),
        ("north_lab", "North Lab", "organization", "Synthetic northern organization."),
        ("south_lab", "South Lab", "organization", "Synthetic southern organization."),
    ]
    for name, title, entity_type, body in entities:
        note(f"knowledge/entities/{name}.md", name, "entity", title,
             {"status": "active", "entity_type": entity_type}, "# " + title + "\n\n" + body + "\n")

    old = "# Architecture — obsolete\n\nNorth Lab uses South Lab.\nAlex Kim (North) maintains North Lab.\n"
    short = "# Architecture — current\n\nNorth Lab uses South Lab.\nSouth Lab uses North Lab.\nNorth Lab does not use South Lab.\nDesign note: café, 東京, 🦀; code token ``` stays literal.\n"
    long = "# Unicode field report\n\nAlex Kim (North) works for North Lab from 2024-01-01 until 2025-01-01.\nAlex Kim (South) does not work for North Lab.\nNorth Lab uses South Lab.\n" + "".join(f"Paragraph {n:03d}: naïve café; Ελληνικά; 東京; e\u0301; 🦀 — preserve every UTF-8 byte.\n" for n in range(160)) + "Final line includes an embedded fence: ``` and four ticks ````.\n"
    snapshots = {"architecture_v1": ("architecture", old),
                 "architecture_v2": ("architecture", short), "report_v1": ("report", long)}
    revisions = []
    for index, (name, (source, content)) in enumerate(snapshots.items()):
        data = content.encode("utf-8")
        directory = f"sources/{ids[source]}/revisions/{ids[name]}"
        files[f"vault/{directory}/original.md"] = data
        files[f"vault/{directory}/content.md"] = data
        snapshot_hash = digest(data)
        note(directory + "/revision.md", name, "revision", name,
             {"source_id": ids[source], "captured_at": f"2025-01-0{index+1}T00:00:00Z",
              "original_path": "original.md", "original_hash": snapshot_hash,
              "extractor": "utf8-preserve-v1", "extractor_fingerprint": digest(b"utf8-preserve-v1"),
              "extraction_status": "complete", "content_path": "content.md",
              "content_hash": snapshot_hash, "media_type": "text/markdown"},
             "Immutable synthetic capture.\n")
        revisions.append({"id": ids[name], "source_id": ids[source], "name": name,
                          "content_path": directory + "/content.md", "original_path": directory + "/original.md",
                          "original_hash": snapshot_hash, "content_hash": snapshot_hash,
                          "byte_length": len(data), "current": name != "architecture_v1"})
    for source, revision_names in [("architecture", ["architecture_v1", "architecture_v2"]), ("report", ["report_v1"])]:
        note(f"sources/{ids[source]}/source.md", source, "source", source,
             {"status": "active", "origin_kind": "local-file", "origin": f"synthetic/{source}.md",
              "current_revision": ids[revision_names[-1]], "revisions": [ids[n] for n in revision_names]},
             "Synthetic local source; no production locator.\n")

    propositions = [
        ("forward", "north_lab", "uses", "south_lab", {}),
        ("reverse", "south_lab", "uses", "north_lab", {}),
        ("negated", "north_lab", "uses", "south_lab", {"negated": True}),
        ("dated", "alex_north", "works_for", "north_lab", {"valid_from": "2024-01-01", "valid_until": "2025-01-01"}),
        ("homonym_negated", "alex_south", "works_for", "north_lab", {"negated": True}),
        ("stale", "alex_north", "maintains", "north_lab", {}),
    ]
    for name, subject, predicate, obj, qualifiers in propositions:
        note(f"knowledge/assertions/{name}.md", name, "assertion", name,
             {"status": "accepted", "subject_id": ids[subject], "predicate": predicate,
              "object_id": ids[obj], **qualifiers},
             f"Synthetic authored canonical assertion: {predicate}.\n")
    note("knowledge/assertions/unicode_property.md", "unicode_property", "assertion", "Unicode design property",
         {"status": "accepted", "subject_id": ids["north_lab"], "predicate": "has_property",
          "property": "design_note", "literal_type": "string",
          "literal_value": "café, 東京, 🦀; code token ``` stays literal."},
         "The design note retains Unicode and literal backticks.\n")
    note("knowledge/pages/architecture.md", "architecture_page", "page", "Architecture summary",
         {"status": "reviewed", "depends_on_ids": [ids["forward"]]},
         "# Architecture summary\n\nNorth Lab uses South Lab; a contradictory report remains visible.\n")

    evidence_specs = [
        ("forward_short", "forward", "architecture_v2", "supports", "North Lab uses South Lab.\n"),
        ("forward_long", "forward", "report_v1", "supports", "North Lab uses South Lab.\n"),
        ("forward_contradiction", "forward", "architecture_v2", "contradicts", "North Lab does not use South Lab.\n"),
        ("reverse_support", "reverse", "architecture_v2", "supports", "South Lab uses North Lab.\n"),
        ("negated_support", "negated", "architecture_v2", "supports", "North Lab does not use South Lab.\n"),
        ("dated_support", "dated", "report_v1", "supports", "Alex Kim (North) works for North Lab from 2024-01-01 until 2025-01-01.\n"),
        ("homonym_support", "homonym_negated", "report_v1", "supports", "Alex Kim (South) does not work for North Lab.\n"),
        ("stale_support", "stale", "architecture_v1", "supports", "Alex Kim (North) maintains North Lab.\n"),
        ("unicode_support", "unicode_property", "architecture_v2", "supports", "Design note: café, 東京, 🦀; code token ``` stays literal.\n"),
    ]
    evidence = []
    for name, assertion, revision, stance, quote in evidence_specs:
        source, content = snapshots[revision]
        data, quoted = content.encode("utf-8"), quote.encode("utf-8")
        assert data.count(quoted) == 1
        start = data.index(quoted)
        end = start + len(quoted)
        quote_hash = digest(quoted)
        fence = "`" * max(3, max((len(run) + 1 for run in re.findall(r"`+", quote)), default=3))
        fields = {"status": "active", "assertion_id": ids[assertion], "source_id": ids[source],
                  "source_revision": ids[revision], "stance": stance, "locator_kind": "utf8-bytes",
                  "span_start": start, "span_end": end, "quote_hash": quote_hash}
        rid = note(f"knowledge/evidence/{name}.md", name, "evidence", name, fields,
                   fence + "text\n" + quote + "\n" + fence + "\n\nVerified synthetic quotation.\n")
        evidence.append({"id": rid, "assertion_id": ids[assertion], "source_id": ids[source],
                         "revision_id": ids[revision], "stance": stance, "span_start": start,
                         "span_end": end, "quote_hash": quote_hash, "quote": quote,
                         "current": revision != "architecture_v1"})

    note("WIKI.md", "vault", "vault", "Synthetic bootstrap vault", {}, "# Synthetic bootstrap vault\n\nCanonical accepted fixtures; later model imports must begin as proposals.\n")
    expected = {
        "fixture_version": 1, "synthetic": True, "ids": ids,
        "records": records, "revisions": revisions, "evidence": evidence,
        "assertion_outcomes": {ids[name]: {"authored_status": "accepted", "eligibility": "Stale" if name == "stale" else "Current", "disputed": name == "forward"} for name, *_ in propositions},
        "expected_decisions": [
            {"action": "keep_distinct", "input_ids": [ids["alex_north"], ids["alex_south"]], "reason": "Equal names do not merge identities."},
            {"action": "revalidate", "input_ids": [ids["stale"]], "reason": "Only superseded-revision support exists."},
        ],
        "expected_semantics": {
            "forward_reverse_distinct": [ids["forward"], ids["reverse"]],
            "positive_negated_distinct": [ids["forward"], ids["negated"]],
            "dated_interval_half_open": ["2024-01-01", "2025-01-01"],
            "after_withdraw_architecture": {ids["forward"]: "Current", ids["reverse"]: "Unsupported", ids["negated"]: "Unsupported"},
            "after_withdraw_both_sources": {ids[name]: "Unsupported" for name, *_ in propositions},
            "rebuild_provider_calls": 0, "import_default_status": "proposed",
            "dependent_page_after_withdraw_architecture": "Current",
            "dependent_page_after_withdraw_both_sources": "Unsupported",
        },
        "files": [{"path": path, "hash": digest(data), "byte_length": len(data)} for path, data in sorted(files.items())],
    }
    expected["assertion_outcomes"][ids["unicode_property"]] = {"authored_status": "accepted", "eligibility": "Current", "disputed": False}
    expected["expected_semantics"]["after_withdraw_architecture"][ids["unicode_property"]] = "Unsupported"
    expected["expected_semantics"]["after_withdraw_both_sources"][ids["unicode_property"]] = "Unsupported"
    files["expected.json"] = (json.dumps(expected, ensure_ascii=False, indent=2, sort_keys=True) + "\n").encode("utf-8")
    for path, data in sorted(files.items()):
        destination = OUT / path
        if args.check:
            if not destination.exists() or destination.read_bytes() != data:
                raise SystemExit(f"fixture mismatch: {path}")
        else:
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(data)
    expected_paths = set(files) | {"README.md"}
    actual_paths = {str(path.relative_to(OUT)) for path in OUT.rglob("*") if path.is_file()}
    if args.check and actual_paths != expected_paths:
        raise SystemExit(f"unexpected fixture paths: {sorted(actual_paths - expected_paths)}")
    fingerprint = hashlib.sha256(files["expected.json"]).hexdigest()
    print(f"{'Verified' if args.check else 'Generated'} {len(files)} files; manifest sha256:{fingerprint}")


if __name__ == "__main__":
    main()
