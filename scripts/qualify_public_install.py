#!/usr/bin/env python3
"""Verify public release bytes and install into an isolated disposable home.

No GitHub authentication or agent login. It retains evidence and exits on failure.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import sys
import time
from urllib.parse import quote

import verify_release

REPO = "joshgarnett/rust-llm-wiki"
COMMON_NAMES = {
    "install.sh", "release-manifest.txt", "release-manifest.json",
    "native-qualification.json",
}


def expected_release(root, source):
    manifest = verify_release.strict_json((root / "release-manifest.json").read_bytes())
    version = manifest["package_version"]
    verify_release.validate_identity(source, version)
    assert manifest["source_commit"] == source
    assert {t["target"] for t in manifest["targets"]} == set(verify_release.TARGETS)
    assert len(manifest["targets"]) == len(verify_release.TARGETS)
    names = COMMON_NAMES.copy()
    for target in verify_release.TARGETS:
        name = "lwiki-" + version + "-" + target + ".tar.gz"
        names.update((name, name + ".sha256"))
        archive, checksum = root / name, root / (name + ".sha256")
        row = next(t for t in manifest["targets"] if t["target"] == target)
        assert digest(archive) == row["archive_sha256"]
        assert archive.stat().st_size == row["archive_bytes"]
        assert checksum.read_text() == row["archive_sha256"] + "  " + name + "\n"
        # These Actions assets already passed release/platform qualification.
        # Recheck their complete bytes without requiring cross-platform tooling.
        # The native installer below inspects the actual host's executable.
        members = verify_release.read_ustar(archive)
        assert {member: {"sha256": verify_release.digest(data), "bytes": len(data)}
                for member, data in members.items()} == row["files"]
        assert verify_release.digest(members["lwiki"]) == row["binary_sha256"]
        assert len(members["lwiki"]) == row["binary_bytes"]
        assert members["artifact-info.txt"] == verify_release.artifact_text(members, source, version, target).encode("ascii")
        assert members["version.txt"] == ("lwiki " + version + "\n").encode()
        info = verify_release.strict_json(members["build-info.json"])
        assert info["source"]["commit"] == source and info["source"]["dirty"] is False
        assert info["target"] == target and info["host"] == target
        assert info["package_version"] == version
    assert {p.name for p in root.iterdir()} == names
    assert all(p.is_file() and not p.is_symlink() for p in root.iterdir())
    assert (root / "release-manifest.txt").read_text() == verify_release.manifest_text(manifest)
    qualified = verify_release.strict_json((root / "native-qualification.json").read_bytes())
    assert qualified["source_commit"] == source and qualified["package_version"] == version
    assert qualified["status"] == "passed"
    return manifest, names


def inventory(root):
    result = {}
    for path in sorted(root.rglob("*")):
        assert not path.is_symlink(), "fixture wiki contains a symlink"
        relative = path.relative_to(root).as_posix()
        if path.is_file():
            result[relative] = {"kind": "file", "sha256": digest(path), "bytes": path.stat().st_size}
        else:
            assert path.is_dir(), "fixture wiki contains a special file"
            result[relative] = {"kind": "directory"}
    return result


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_captured_passage(passage, captured, read, body):
    """Authenticate a public logical locator through an ordinary public read."""
    source, revision = (captured["allocated_ids"][key] for key in ("source", "revision"))
    path = "sources/" + source + "/revisions/" + revision + "/content.md"
    operation = next(op for op in captured["plan"]["operations"] if op["path"] == path)
    expected_hash = operation["after"]["hash"]
    span = {"start": 0, "end": len(body)}
    citation = {"kind": "source", "reference": {
        "source_id": source, "source_revision": revision, "span": span,
        "quote_hash": expected_hash}}
    assert operation["after"]["state"] == "hash" and operation["byte_len"] == len(body)
    assert passage["locator"]["path"] == read["path"] == path
    assert passage["locator"]["observed_hash"] == read["hash"] == expected_hash
    assert passage["locator"]["record"]["record_id"] == revision
    assert passage["locator"]["record"]["expected_kind"] == "revision"
    assert passage["span"] == read["range"] == span
    assert passage["text"].encode() == read["body"].encode() == body
    assert read["truncated"] is False and read["continuation"] is None
    assert passage["eligibility"] == "current"
    assert citation in passage["citations"]
    assert read["source_citation"] == {"citation": citation, "eligibility": "current"}


def verify_restored_rendering(context, restored_context, wiki, restored):
    # Only a Markdown link's absolute wiki root changes after a folder restore.
    old_root = "<" + quote(str(wiki), safe="/") + "/"
    new_root = "<" + quote(str(restored), safe="/") + "/"
    assert old_root in context["text"], "original context omitted its saved-note link"
    assert restored_context["text"] == context["text"].replace(old_root, new_root)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--expected-dir", type=Path, required=True,
                        help="frozen eight original Actions release files")
    parser.add_argument("--source-commit", required=True, help="full tested product commit")
    parser.add_argument("--evidence-dir", type=Path, required=True,
                        help="new nonexistent output directory")
    args = parser.parse_args()
    expected = args.expected_dir.resolve(strict=True)
    manifest, names = expected_release(expected, args.source_commit)
    version, tag = manifest["package_version"], "v" + manifest["package_version"]
    evidence = args.evidence_dir.absolute()
    evidence.mkdir(mode=0o700, parents=False, exist_ok=False)
    home, downloads = evidence / "home", evidence / "public-downloads"
    home.mkdir(mode=0o700)
    downloads.mkdir(mode=0o700)
    prefix, agent_home = home / ".local", home / "agent-home"
    agent_home.mkdir(mode=0o700)
    # Pass a private child-process environment; do not modify the running host.
    env = {"HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config"),
           "PATH": str(prefix / "bin") + ":/usr/bin:/bin:/usr/sbin:/sbin",
           "LC_ALL": "C", "TZ": "UTC"}
    result = {"status": "running", "source_commit": args.source_commit,
              "package_version": version, "tag": tag,
              "platform": platform.platform(), "machine": platform.machine(),
              "scope": "public HTTPS downloads; native installed workflow; agent files only",
              "agent_account_invoked": False, "commands": [], "downloads": []}
    start = time.monotonic()

    def save():
        (evidence / "result.json").write_text(json.dumps(result, indent=2) + "\n")

    def run(label, command, timeout=120):
        before = time.monotonic()
        timed_out = False
        with (evidence / (label + ".stdout")).open("wb") as out, (evidence / (label + ".stderr")).open("wb") as err:
            child = subprocess.Popen(command, env=env, cwd=evidence, stdout=out, stderr=err,
                                     start_new_session=True)
            try:
                code = child.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                timed_out = True
                os.killpg(child.pid, signal.SIGKILL)
                code = child.wait(timeout=5)
        result["commands"].append({"label": label, "command": command,
                                   "exit_code": code, "timed_out": timed_out,
                                   "elapsed_seconds": time.monotonic() - before})
        save()
        if timed_out or code:
            raise RuntimeError(label + " failed; inspect retained stdout/stderr")
        return (evidence / (label + ".stdout")).read_bytes()

    def download(label, url, destination, maximum):
        run(label, ["/usr/bin/curl", "-q", "--fail", "--silent", "--show-error",
                    "--location", "--proto", "=https", "--proto-redir", "=https",
                    "--connect-timeout", "15", "--max-time", "90",
                    "--max-filesize", str(maximum), url, "-o", str(destination)])

    try:
        metadata = evidence / "public-release.json"
        download("release-metadata", "https://api.github.com/repos/" + REPO +
                 "/releases/tags/" + tag, metadata, 262144)
        public = json.loads(metadata.read_text())
        assert public["tag_name"] == tag and public["draft"] is False
        assert public["prerelease"] is False
        assert public["html_url"] == "https://github.com/" + REPO + "/releases/tag/" + tag
        assert {a["name"] for a in public["assets"]} == names
        assert len(public["assets"]) == len(names)
        latest_path = evidence / "public-latest-release.json"
        download("latest-release", "https://api.github.com/repos/" + REPO + "/releases/latest", latest_path, 262144)
        latest = json.loads(latest_path.read_text())
        assert latest["id"] == public["id"] and latest["tag_name"] == tag
        ref_path = evidence / "public-tag-ref.json"
        download("tag-ref", "https://api.github.com/repos/" + REPO + "/git/ref/tags/" + tag, ref_path, 262144)
        ref = json.loads(ref_path.read_text())
        assert ref["ref"] == "refs/tags/" + tag
        obj = ref["object"]
        if obj["type"] == "tag":
            peeled_path = evidence / "public-annotated-tag.json"
            download("annotated-tag", "https://api.github.com/repos/" + REPO + "/git/tags/" + obj["sha"], peeled_path, 262144)
            annotated = json.loads(peeled_path.read_text())
            assert annotated["tag"] == tag
            obj = annotated["object"]
        assert obj["type"] == "commit" and obj["sha"] == args.source_commit
        result["public_tag_commit"] = obj["sha"]
        for name in sorted(names):
            path = downloads / name
            download("download-" + name, "https://github.com/" + REPO +
                     "/releases/download/" + tag + "/" + name, path, 134217728)
            assert path.stat().st_size == (expected / name).stat().st_size
            assert digest(path) == digest(expected / name)
            result["downloads"].append({"name": name, "sha256": digest(path),
                                         "bytes": path.stat().st_size})
            save()
        public_manifest, public_names = expected_release(downloads, args.source_commit)
        assert public_manifest == manifest and public_names == names
        target = {("Darwin", "arm64"): "aarch64-apple-darwin",
                  ("Linux", "x86_64"): "x86_64-unknown-linux-gnu"}.get(
                      (platform.system(), platform.machine()))
        assert target, "native smoke requires a supported native host"
        # Deliberately omit --from-dir: this exercises the installer's public URLs.
        run("public-install", ["/bin/sh", str(downloads / "install.sh"),
                               "--version", version, "--prefix", str(prefix)], 240)
        executable = prefix / "bin/lwiki"
        expected_binary = next(t["binary_sha256"] for t in manifest["targets"]
                               if t["target"] == target)
        assert digest(executable) == expected_binary
        assert run("new-shell-version", ["/bin/sh", "-c", "exec lwiki --version"]) == ("lwiki " + version + "\n").encode()
        wiki, note = home / "My wiki", home / "My note.md"
        body = b"# My note\n\nThe cedar room meeting starts at 14:35 UTC. PUBLIC-INSTALL-482.\n"
        note.write_bytes(body)

        def cli(label, arguments):
            envelope = json.loads(run(label, [str(executable), "--offline", "--json"] + arguments))
            assert envelope["ok"] is True, envelope
            assert envelope.get("meta", {}).get("network_used") is False, envelope
            return envelope["data"]

        cli("setup", ["setup", str(wiki), "--title", "My wiki", "--non-interactive",
                      "--agent", "codex", "--agent-directory", str(agent_home)])
        cli("wiki-show", ["wiki", "show"])
        cli("config-show", ["config", "show"])
        assert (wiki / "WIKI.md").is_file()
        assert (home / ".config/lwiki/config.toml").is_file()
        assert (agent_home / ".agents/skills/llm-wiki/SKILL.md").is_file()
        cli("skill-status", ["skill", "status", "--target", "codex", "--directory", str(agent_home)])
        captured = cli("capture", ["source", "add", str(note), "--title", "My note"])
        hits = cli("literal-search", ["search", "PUBLIC-INSTALL-482", "--mode", "literal"])
        assert any("PUBLIC-INSTALL-482" in h.get("excerpt", {}).get("text", "") for h in hits["hits"])
        context = cli("context", ["context", "PUBLIC-INSTALL-482", "--max-bytes", "6000", "--max-tokens", "1500"])
        assert "14:35 UTC" in context["text"]
        facts = []
        for passage in context["passages"]:
            if "PUBLIC-INSTALL-482" not in passage["text"]:
                continue
            label = "original-read" if not facts else "original-read-" + str(len(facts))
            read = cli(label, ["read", "--path", passage["locator"]["path"], "--max-bytes", "6000"])
            verify_captured_passage(passage, captured, read, body)
            facts.append((passage, read))
        assert facts and note.read_bytes() == body
        fact, original_read = facts[0]
        # All commands have exited; copy the complete folder exactly as documented.
        original_inventory = inventory(wiki)
        assert any(p.startswith(".wiki/") for p in original_inventory)
        assert any("/revisions/" in p and p.endswith("/content.md") for p in original_inventory)
        backup, restored = home / "Wiki backup", home / "Restored wiki"
        run("backup-complete-folder", ["/bin/cp", "-a", str(wiki), str(backup)])
        run("restore-complete-folder", ["/bin/cp", "-a", str(backup), str(restored)])
        assert inventory(backup) == original_inventory == inventory(restored)
        (evidence / "backup-inventory.json").write_text(json.dumps(original_inventory, indent=2) + "\n")
        restored_prefix = ["--no-user-config", "--wiki", str(restored)]
        cli("restored-index-rebuild", restored_prefix + ["index", "rebuild"])
        cli("restored-check", restored_prefix + ["check"])
        restored_read = cli("restored-read", restored_prefix + ["read", "--path", fact["locator"]["path"], "--max-bytes", "6000"])
        assert restored_read["body"] == original_read["body"]
        restored_context = cli("restored-context", restored_prefix + ["context", "PUBLIC-INSTALL-482", "--max-bytes", "6000", "--max-tokens", "1500"])
        verify_restored_rendering(context, restored_context, wiki, restored)
        restored_fact = next(p for p in restored_context["passages"] if "PUBLIC-INSTALL-482" in p["text"])
        for key in ("text", "span", "locator", "citations"):
            assert restored_fact[key] == fact[key], "restored evidence changed: " + key
        verify_captured_passage(restored_fact, captured, restored_read, body)
        assert inventory(wiki) == original_inventory == inventory(backup)
        assert note.read_bytes() == body
        result["backup_restore"] = {"status": "passed", "inventory_entries": len(original_inventory),
                                   "includes_hidden_state": True, "citation_unchanged": True}
        assert digest(executable) == expected_binary
        result.update(status="passed", target=target, installed_binary_sha256=expected_binary)
    except Exception as error:
        result.update(status="failed", error=str(error))
    finally:
        result["elapsed_seconds"] = time.monotonic() - start
        save()
    print(json.dumps({"status": result["status"], "evidence": str(evidence)}))
    return 0 if result["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
