#!/usr/bin/env python3
"""Check one native released install, reinstall, removal and restoration privately."""
import argparse
import json
import os
from pathlib import Path
import platform
import re
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import qualify_artifact as qualification
import verify_release as verifier


def inventory(directory):
    rows = {}
    for path in sorted(directory.rglob("*")):
        relative = str(path.relative_to(directory))
        if path.is_symlink():
            rows[relative] = {"kind": "symlink", "target": os.readlink(path)}
        elif path.is_file():
            rows[relative] = {"kind": "file", "sha256": qualification.digest(path), "bytes": path.stat().st_size}
        elif path.is_dir():
            rows[relative] = {"kind": "directory"}
        else:
            raise RuntimeError("unsupported fixture file type: " + str(path))
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-directory", required=True)
    parser.add_argument("--binary-sha256", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--evidence", required=True)
    args = parser.parse_args()
    if os.getuid() == 0:
        parser.error("run the installer qualification as the native nonroot owner")
    if not re.fullmatch(r"[0-9a-f]{64}", args.binary_sha256):
        parser.error("binary SHA-256 must be a full lowercase hash")
    release = Path(args.release_directory).resolve(strict=True)
    evidence = Path(args.evidence).resolve()
    if release == evidence or release in evidence.parents or evidence in release.parents:
        parser.error("evidence must be separate from the release asset directory")
    manifest = verifier.strict_json((release / "release-manifest.json").read_bytes())
    version = manifest.get("package_version") if isinstance(manifest, dict) else None
    verifier.validate_identity(args.source_commit, version)
    target = {("Darwin", "arm64"): "aarch64-apple-darwin", ("Linux", "x86_64"): "x86_64-unknown-linux-gnu"}.get((platform.system(), platform.machine()))
    if not target:
        parser.error("requires a supported native release host")
    installer = release / "install.sh"
    repo = Path(__file__).resolve().parents[2]
    if installer.is_symlink() or installer.read_bytes() != (repo / "scripts/install.sh").read_bytes():
        parser.error("released installer differs from the unchanged reviewed installer")
    host = qualification.host_information(target, os.environ.get("RUNNER_OS", "local-native"))
    qualification.read_manifest(release, args.source_commit, version)
    archive = release / ("lwiki-{}-{}.tar.gz".format(version, target))
    record = verifier.verify_archive(archive, Path(str(archive) + ".sha256"), args.source_commit, version, target)
    if record["binary_sha256"] != args.binary_sha256:
        parser.error("native archive binary differs from the required final executable")
    evidence.mkdir(mode=0o700)
    for directory in ["home", "temp", "elsewhere", "archive-verification"]:
        (evidence / directory).mkdir(mode=0o700)
    home = evidence / "home"
    prefix, config_home = home / ".local", home / ".config"
    env = {"PATH": str(prefix / "bin") + ":/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(home),
           "XDG_CONFIG_HOME": str(config_home), "TMPDIR": str(evidence / "temp") + "/", "LC_ALL": "C", "TZ": "UTC"}
    result = {"status": "running", "source_commit": args.source_commit, "target": target,
              "version": version, "binary_sha256": args.binary_sha256, "archive_sha256": record["archive_sha256"],
              "installer_sha256": qualification.digest(installer), "harness_sha256": qualification.digest(Path(__file__)),
              "host": host, "python_version": sys.version, "env": env, "checks": [], "operations": []}
    started = time.monotonic()
    binary = installed = None

    def save(name, value):
        qualification.write_json(evidence / name, value)

    def check(name, passed, **details):
        result["checks"].append({"name": name, "passed": bool(passed), **details})
        save("installer-result.json", result)
        if not passed:
            raise RuntimeError(name)

    def run(name, argv, expected=0, timeout=45, cwd=None):
        before = time.monotonic()
        timed_out = False
        with (evidence / (name + ".stdout")).open("wb") as stdout, (evidence / (name + ".stderr")).open("wb") as stderr:
            child = subprocess.Popen(list(map(str, argv)), cwd=cwd or home, env=env, stdout=stdout, stderr=stderr, start_new_session=True)
            try:
                code = child.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                timed_out = True
                os.killpg(child.pid, signal.SIGKILL)
                code = child.wait(timeout=5)
        result["operations"].append({"name": name, "argv": list(map(str, argv)), "cwd": str(cwd or home), "exit_code": code,
                                     "timed_out": timed_out, "elapsed_seconds": time.monotonic() - before})
        save("installer-result.json", result)
        if timed_out or code != expected:
            raise RuntimeError(name + " failed; inspect retained stdout/stderr")
        return (evidence / (name + ".stdout")).read_bytes()

    def shell(name, script, args=(), expected=0, cwd=None):
        return run(name, ["/bin/sh", "-c", script, "lwiki-lifecycle", *args], expected=expected, cwd=cwd)

    def cli(name, args, cwd=None):
        raw = shell(name, 'exec lwiki --offline --json "$@"', args, cwd=cwd)
        value = verifier.strict_json(raw)
        check(name + ": successful offline PATH command", value.get("ok") is True and value.get("meta", {}).get("network_used") is False)
        save(name + ".json", value)
        return value["data"]

    def path_version(name, cwd=None):
        resolved = shell(name + "-command-v", "command -v lwiki", cwd=cwd).decode("utf-8").strip()
        check(name + ": shell resolves owned launch", resolved == str(prefix / "bin/lwiki"), resolved=resolved)
        reported = shell(name + "-version", "exec lwiki --version", cwd=cwd)
        check(name + ": new PATH process reports exact version", reported == ("lwiki " + version + "\n").encode("ascii"))
        check(name + ": active executable equals final archive", qualification.digest(prefix / "bin/lwiki") == args.binary_sha256)

    try:
        save("installer-result.json", result)
        archive, binary, record = qualification.prepare_archive(archive, evidence / "archive-verification", args.source_commit, version, target, host, release)
        sentinel = prefix / "share/unrelated-owner-file.bin"
        sentinel.parent.mkdir(parents=True, mode=0o700)
        sentinel_bytes = b"Keep this unrelated user file.\x00\r\n"
        sentinel.write_bytes(sentinel_bytes)
        install = ["/bin/sh", str(installer), "--version", version, "--from-dir", str(release)]
        shell("initial-command-absent", "command -v lwiki", expected=1)
        run("initial-install", install, timeout=180)
        installed = prefix / "bin/lwiki"
        path_version("initial")
        generation = installed.resolve(strict=True).parent
        check("installed generation stays inside owned prefix", prefix.resolve() in generation.parents)
        wiki, note = home / "My lasting wiki", home / "My lasting note.md"
        body = "# My lasting note\n\nThe cedar room meeting begins at 14:35 UTC. Identifier LIFECYCLE-482.\n"
        note.write_text(body, encoding="utf-8")
        cli("setup", ["setup", str(wiki), "--title", "My lasting wiki", "--non-interactive"])
        check("setup saves an owned lasting wiki and configuration", wiki.is_dir() and config_home.is_dir() and any(config_home.rglob("*.toml")))
        captured = cli("capture", ["source", "add", str(note), "--title", "My lasting note"])
        cli("config-show", ["config", "show"])
        cli("initial-context", ["context", "LIFECYCLE-482", "--max-bytes", "6000", "--max-tokens", "1500"])
        roots = {"wiki": wiki, "config": config_home, "generation": generation}

        def snapshot(name):
            out = {label: inventory(directory) for label, directory in roots.items()}
            out["author_note"] = {"sha256": qualification.digest(note), "bytes": note.stat().st_size}
            out["unrelated_sentinel"] = {"sha256": qualification.digest(sentinel), "bytes": sentinel.stat().st_size}
            save(name + ".json", out)
            return out

        preserved = snapshot("before-reinstall")
        run("same-version-reinstall", install, timeout=180)
        path_version("reinstalled")
        check("same-version reinstall preserves all user data and retained generation", snapshot("after-reinstall") == preserved)
        run("uninstall", ["/bin/sh", str(installer), "--uninstall"], timeout=180)
        shell("removed-command-v", "command -v lwiki", expected=1, cwd=evidence / "elsewhere")
        check("uninstall removes launch and current activation", not installed.exists() and not installed.is_symlink() and not (prefix / ".lwiki-install/current").exists() and not (prefix / ".lwiki-install/current").is_symlink())
        check("uninstall preserves all user data and retained generation", snapshot("after-uninstall") == preserved)
        installed = None
        run("restore-same-version", install, timeout=180)
        installed = prefix / "bin/lwiki"
        path_version("restored", cwd=evidence / "elsewhere")
        check("restoration preserves all user data and retained generation", snapshot("after-restoration") == preserved)
        searched = cli("restored-search", ["search", "LIFECYCLE-482", "--mode", "literal"], cwd=evidence / "elsewhere")
        check("new process discovers saved wiki and finds captured note", any("LIFECYCLE-482" in hit.get("excerpt", {}).get("text", "") for hit in searched["hits"]))
        context = cli("restored-context", ["context", "LIFECYCLE-482", "--max-bytes", "6000", "--max-tokens", "1500"], cwd=evidence / "elsewhere")
        ids = captured["allocated_ids"]
        checked = False
        for passage in context["passages"]:
            if "LIFECYCLE-482" not in passage["text"]:
                continue
            located = (wiki / passage["locator"]["path"]).resolve(strict=True)
            check("restored context cites an owned immutable payload", wiki.resolve() in located.parents and located.name == "content.md" and "/revisions/" in str(located))
            payload = located.read_bytes()
            span = passage["span"]
            check("restored context physical span equals returned fact", payload == body.encode("utf-8") and payload[span["start"]:span["end"]].decode("utf-8") == passage["text"])
            checked |= any(citation.get("kind") == "source" and citation.get("reference", {}).get("source_id") == ids["source"] and citation["reference"].get("source_revision") == ids["revision"] and citation["reference"].get("span") == span for citation in passage["citations"])
        check("restored context contains original fact and exact captured citation", checked and "14:35 UTC" in context["text"] and "LIFECYCLE-482" in context["text"])
        check("unrelated sentinel and author note remain byte exact", sentinel.read_bytes() == sentinel_bytes and note.read_bytes() == body.encode("utf-8"))
        qualification.verify_unchanged(binary, archive, record, installed)
        result["status"] = "passed"
    except Exception as error:
        result["status"] = "failed"; result["error"] = str(error)
    finally:
        result["archive_final_sha256"] = qualification.digest(archive)
        result["installer_final_sha256"] = qualification.digest(installer)
        if binary is not None:
            result["binary_final_sha256"] = qualification.digest(binary)
        if installed is not None and installed.exists():
            result["installed_final_sha256"] = qualification.digest(installed)
        if result["archive_final_sha256"] != record["archive_sha256"] or result["installer_final_sha256"] != result["installer_sha256"]:
            result["status"] = "failed"; result["error"] = "released archive or installer changed"
        result["elapsed_seconds"] = time.monotonic() - started
        save("installer-result.json", result)
    print(json.dumps({"status": result["status"], "evidence": str(evidence)}))
    return 0 if result["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
