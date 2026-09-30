#!/usr/bin/env python3
"""Disposable CLI handoff and guarded-page exercise; no network or real vaults."""

import argparse
import hashlib
import json
import shutil
import subprocess
import tempfile
from pathlib import Path


def run(binary, vault, *args, payload=None, error=None):
    raw = None if payload is None else (payload if isinstance(payload, str) else json.dumps(payload))
    completed = subprocess.run(
        [str(binary), "--json", "--offline", "--wiki", str(vault), *args],
        input=raw, text=True, capture_output=True, check=False,
    )
    try:
        result = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise AssertionError(f"{args}: invalid JSON: {completed.stdout} {completed.stderr}") from exc
    if error:
        assert not result["ok"] and result["error"]["code"] == error, (args, result)
    else:
        assert completed.returncode == 0 and result["ok"], (args, result, completed.stderr)
    return result["data"] if result["ok"] else result


def note(identity, title, body):
    return (
        '---\nwiki_schema: "1"\n'
        f'wiki_id: {identity}\nwiki_kind: page\nwiki_status: draft\n'
        f'title: "{title}"\n---\n{body}'
    )


def physical_snapshot(vault):
    files = [p for p in vault.rglob("*") if p.is_file()]
    lengths = {}
    for path in files:
        data = path.read_bytes()
        key = hashlib.sha256(data).digest()
        lengths.setdefault(key, [len(data), 0])[1] += 1
    return {
        "files": len(files),
        "bytes": sum(p.stat().st_size for p in files),
        "unique_content_bytes": sum(size for size, _ in lengths.values()),
        "duplicate_bytes": sum(size * (count - 1) for size, count in lengths.values()),
        "visible_markdown": sum(p.suffix == ".md" and ".wiki" not in p.parts for p in files),
    }


def baseline(binary, root, long_text, correction):
    """Measure the overlapping 0.1.2 capture/page/refresh operations."""
    root.mkdir()
    (root / "WIKI.md").write_text(
        '---\nwiki_schema: "1"\nwiki_id: Vault.OldSharedRecipe\n'
        'wiki_kind: vault\ntitle: "Old shared recipe"\n---\n', encoding="utf-8"
    )
    call = lambda *argv, payload=None: run(binary, root, *argv, payload=payload)
    empty = physical_snapshot(root)
    first = call("source", "add", "-", "--title", "Wixom dispatch roundup", payload=long_text)
    other = call("source", "add", "-", "--title", "Wixom counter-note", payload=correction)
    source = first["allocated_ids"]["source"]
    revision = first["allocated_ids"]["revision"]
    repeated = call("source", "refresh", source, "--file", "-", payload=long_text)
    assert repeated["allocated_ids"]["revision"] == revision
    topic = note("Page.WixomCedar", "Wixom Cedar backup",
                 f"# Wixom Cedar backup\n\nSource [[{source}]] and counter-note "
                 f"[[{other['allocated_ids']['source']}]] conflict.\n")
    guide = note("Page.WixomGuide", "Wixom guide",
                 "# Wixom guide\n\n[[pages/wixom-cedar.md|Cedar backup]]\n")
    call("page", "put", "--file", "-", "--path", "pages/wixom-cedar.md", payload=topic)
    call("page", "put", "--file", "-", "--path", "pages/guide.md", payload=guide)
    captured = physical_snapshot(root)
    for date, finding in [
        ("2025-04-14", "Gate hours require an independent check."),
        ("2025-04-15", "Two Morgan identities remain separate."),
        ("2025-04-16", "A closure claim conflicts with the recommendation."),
        ("2025-04-17", "Routing should await a current shelter notice."),
    ]:
        old_hash = call("read", "--id", "Page.WixomCedar")["hash"]
        topic += f"\n{date}: {finding}\n"
        call("page", "put", "--file", "-", "--path", "pages/wixom-cedar.md",
             "--if-match", old_hash, payload=topic)
    call("source", "refresh", source, "--file", "-",
         payload=long_text.replace("east shelter", "north shelter"))
    call("source", "withdraw", other["allocated_ids"]["source"],
         "--reason", "Counter-note withdrawn by fixture")
    return {"empty": empty, "after_capture": captured, "after_updates": physical_snapshot(root)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--baseline-binary", type=Path,
                        help="Packaged 0.1.2 binary for overlapping-operation measurements")
    parser.add_argument("--keep", type=Path, help="Keep the disposable vault here")
    args = parser.parse_args()
    binary = args.binary.resolve()
    assert binary.is_file(), binary
    with tempfile.TemporaryDirectory(prefix="lwiki-shared-") as scratch:
        # Bazel's output symlink can be replaced by a concurrent root build.
        # Freeze the two binaries for this one read-only fixture invocation.
        current_binary = Path(scratch) / "lwiki-current"
        shutil.copy2(binary, current_binary)
        binary = current_binary
        if args.baseline_binary:
            packaged_binary = Path(scratch) / "lwiki-012"
            shutil.copy2(args.baseline_binary.resolve(), packaged_binary)
        else:
            packaged_binary = None
        vault = Path(scratch) / "vault"
        vault.mkdir()
        (vault / "WIKI.md").write_text(
            '---\nwiki_schema: "1"\nwiki_id: Vault.SharedRecipe\n'
            'wiki_kind: vault\ntitle: "Shared recipe"\n---\n', encoding="utf-8"
        )
        call = lambda *argv, payload=None, error=None: run(binary, vault, *argv, payload=payload, error=error)
        # The recommendation occurs after the old 4096-byte prefix. Two people
        # named Morgan and a dated contradiction force the host to preserve gaps.
        long_text = (
            "Wixom dispatch roundup, 2025-04-03. Morgan Lee and Morgan Ortiz "
            "are different dispatchers. Routine shift notes and repeated names.\n" * 65
            + "2025-04-12: Morgan Lee recommends east shelter for Cedar backups. "
            "The recommendation supersedes the west shelter note from March.\n"
        )
        correction = (
            "Wixom counter-note, 2025-04-13. Morgan Ortiz says the east "
            "shelter was closed on 2025-04-12. This account conflicts with "
            "Morgan Lee's recommendation and has not been reconciled.\n"
        )
        baseline_measurements = None
        if packaged_binary:
            baseline_measurements = baseline(packaged_binary,
                                             Path(scratch) / "baseline-012",
                                             long_text, correction)
        empty = physical_snapshot(vault)
        first = call("source", "add", "-", "--title", "Wixom dispatch roundup", payload=long_text)
        second = call("source", "add", "-", "--title", "Wixom counter-note", payload=correction)
        source = first["allocated_ids"]["source"]
        other = second["allocated_ids"]["source"]
        revision = first["allocated_ids"]["revision"]
        repeated = call("source", "refresh", source, "--file", "-", payload=long_text)
        assert repeated["allocated_ids"]["revision"] == revision

        started = call("research", "run", "Where should Cedar backups go near Wixom?",
                       "--source-id", source, "--source-id", other,
                       "--max-rounds", "1", "--max-sources", "0")
        run_id = started["run_id"]
        collected = call("research", "import", "--file", "-", payload={
            "schema": "lwiki.research-submission.v1", "run_id": run_id,
            "packet_fingerprint": started["packet"]["packet_fingerprint"],
            "response": {"stage": "collect_sources", "sources": [], "gaps": []},
        })
        packet = collected["packet"]
        passages = packet["passages"]
        assert passages, packet
        recommendation = next((p for p in passages if "east shelter" in p["quote"]), None)
        assert recommendation is not None, passages
        response = {
            "schema": "lwiki.research-submission.v1", "run_id": run_id,
            "packet_fingerprint": packet["packet_fingerprint"],
            "response": {"stage": "answer", "claims": [{
                "text": "The April 12 roundup recommends the east shelter for Cedar backups.",
                "passage_ids": [recommendation["passage_id"]],
            }], "gaps": ["The April 13 closure counter-note is unresolved."], "follow_up": None},
        }
        answered = call("research", "import", "--file", "-", payload=response)
        assert answered["report"]["claims"] and answered["report"]["gaps"]
        repeated_answer = call("research", "import", "--file", "-", payload=response)
        assert repeated_answer["reused"] is True, repeated_answer
        report = call("research", "report", run_id)
        assert report["claims"] and report["gaps"]

        topic = note("Page.WixomCedar", "Wixom Cedar backup",
                     "# Wixom Cedar backup\n\n"
                     f"The April 12 roundup recommends the east shelter [[{source}]]. "
                     f"The April 13 account disputes access [[{other}]].\n\n"
                     "## Open question\nConfirm whether the shelter was open before routing backups.\n")
        guide = note("Page.WixomGuide", "Wixom guide",
                     "# Wixom guide\n\n[[pages/wixom-cedar.md|Cedar backup and open question]]\n")
        proposal = {"title": "File cited Wixom research and guide", "pages": [
            {"path": "pages/wixom-cedar.md", "markdown": topic},
            {"path": "pages/guide.md", "markdown": guide},
        ]}
        staged = run(binary, vault, "--stage", "page", "batch", "--file", "-", payload=proposal)
        change = staged["change"]["change_id"]
        call("changes", "show", change)
        call("changes", "apply", change)
        topic_path = vault / "pages/wixom-cedar.md"
        guide_path = vault / "pages/guide.md"
        assert "Open question" in topic_path.read_text() and "wixom-cedar.md" in guide_path.read_text()

        old_hash = call("read", "--id", "Page.WixomCedar")["hash"]
        with topic_path.open("a", encoding="utf-8") as file:
            file.write("\nHuman observation: check gate hours.\n")
        proposal["pages"][0]["if_match"] = old_hash
        proposal["pages"][0]["markdown"] = topic + "\nAgent follow-up.\n"
        proposal["pages"][1]["if_match"] = call("read", "--id", "Page.WixomGuide")["hash"]
        call("page", "batch", "--file", "-", payload=proposal, error="CONTENT_CONFLICT")
        assert "Human observation" in topic_path.read_text()

        for date, finding in [
            ("2025-04-14", "Gate hours require an independent check."),
            ("2025-04-15", "Two Morgan identities remain separate."),
            ("2025-04-16", "A closure claim conflicts with the recommendation."),
            ("2025-04-17", "Routing should await a current shelter notice."),
        ]:
            proposal["pages"][0]["if_match"] = call("read", "--id", "Page.WixomCedar")["hash"]
            proposal["pages"][1]["if_match"] = call("read", "--id", "Page.WixomGuide")["hash"]
            proposal["pages"][0]["markdown"] = topic_path.read_text() + f"\n{date}: {finding}\n"
            call("page", "batch", "--file", "-", payload=proposal)
        proposal["pages"][0]["if_match"] = call("read", "--id", "Page.WixomCedar")["hash"]
        proposal["pages"][0]["markdown"] = topic_path.read_text() + "\nAgent follow-up.\n"
        call("page", "batch", "--file", "-", payload=proposal)
        assert "Human observation" in topic_path.read_text()

        changed = long_text.replace("east shelter", "north shelter")
        newer = call("source", "refresh", source, "--file", "-", payload=changed)
        assert newer["allocated_ids"]["revision"] != revision
        status = call("research", "status", run_id)
        report_after = call("research", "report", run_id)
        maintenance = call("research", "maintenance", run_id)
        assert status["report_freshness"] == "retained"
        assert report_after["freshness"] == "retained"
        call("source", "withdraw", other, "--reason", "Counter-note withdrawn by fixture")
        # Reassess the authored prose after source lifecycle changes. The new
        # revision points north; the old report and closure claim stay historical.
        proposal["pages"][0]["if_match"] = call("read", "--id", "Page.WixomCedar")["hash"]
        proposal["pages"][1]["if_match"] = call("read", "--id", "Page.WixomGuide")["hash"]
        historical_text = topic_path.read_text().replace(
            f"The April 12 roundup recommends the east shelter [[{source}]]. "
            f"The April 13 account disputes access [[{other}]].",
            f"Historical: the superseded April 12 roundup recommended the east shelter [[{source}]]. "
            f"The withdrawn April 13 account disputed access [[{other}]].",
        )
        assert "Historical: the superseded" in historical_text
        proposal["pages"][0]["markdown"] = (
            historical_text + f"\n## Current review\nThe newer roundup [[{source}]] "
            "mentions north shelter. Verify availability before routing; "
            "the withdrawn counter-note no longer provides Current support.\n"
        )
        proposal["pages"][1]["markdown"] = guide_path.read_text() + (
            "\nThe Cedar page records the newer source and unresolved availability gap.\n"
        )
        last_page_change = call("page", "batch", "--file", "-", payload=proposal)["change"]["change_id"]
        assert "north shelter" in topic_path.read_text()
        inventory_before = call("storage", "inventory")
        physical_before = physical_snapshot(vault)
        plan = call("storage", "plan", "--retain-undo-changes", "1")
        backup = Path(scratch) / "complete-backup"
        shutil.copytree(vault, backup)
        replanned = call("storage", "plan", "--retain-undo-changes", "1")
        assert replanned["plan_hash"] == plan["plan_hash"], (plan, replanned)
        cleanup = call("storage", "cleanup", "--retain-undo-changes", "1",
                       "--expected-plan", plan["plan_hash"])
        inventory_after = call("storage", "inventory")
        physical_after = physical_snapshot(vault)
        assert cleanup["after"]["logical_bytes"] == inventory_after["totals"]["logical_bytes"]
        assert (backup / "WIKI.md").exists() and (backup / ".wiki").exists()
        assert "Human observation" in topic_path.read_text()
        reports = list((vault / "runs").glob("*/outputs/report_*.md"))
        assert len(reports) == 1
        report_text = reports[0].read_text(encoding="utf-8")
        assert "## Claims" in report_text and "## Unresolved gaps" in report_text
        assert report_text.index("## Claims") < report_text.index("```"), "report prose must be visible"
        restored = Path(scratch) / "complete-restore"
        shutil.copytree(backup, restored)
        restored_page = run(binary, restored, "read", "--id", "Page.WixomCedar")
        restored_report = run(binary, restored, "research", "report", run_id)
        assert "Human observation" in restored_page["body"] and restored_report["claims"]
        post_cleanup_copy = Path(scratch) / "post-cleanup-undo"
        shutil.copytree(vault, post_cleanup_copy)
        inverse = run(binary, post_cleanup_copy, "changes", "rollback", last_page_change)
        run(binary, post_cleanup_copy, "changes", "apply", inverse["change"]["change_id"])
        assert "Human observation" in (post_cleanup_copy / "pages/wixom-cedar.md").read_text()
        result = {
            "source": source, "research_run": run_id,
            "later_recommendation_offset": long_text.index("2025-04-12:"),
            "stale_report": status["report_freshness"],
            "maintenance_tasks": len(maintenance.get("tasks", [])),
            "files_before": inventory_before["totals"]["files"],
            "files_after": inventory_after["totals"]["files"],
            "bytes_before": inventory_before["totals"]["logical_bytes"],
            "bytes_after": inventory_after["totals"]["logical_bytes"],
            "cleanup_deleted_files": cleanup["deleted_files"],
            "backup_complete": True,
            "restored_authority": True,
            "post_cleanup_undo": True,
            "human_report_readable": True,
            "physical": {"empty": empty, "before_cleanup": physical_before,
                         "after_cleanup": physical_after},
            "baseline_012": baseline_measurements,
        }
        if args.keep:
            assert not args.keep.exists(), args.keep
            shutil.copytree(vault, args.keep)
            shutil.copytree(backup, args.keep.with_name(args.keep.name + "-before-cleanup"))
        print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
