#!/usr/bin/env python3
"""One pinned real-editor attempt on a fresh GitHub-hosted macOS VM only."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import signal
import socket
import subprocess
import sys
import time
import urllib.request

VERSION = "1.10.6"
SIZE = 210410377
SHA256 = "a7fbd87353d7802933a13e3d90f55c39114c419ac48d459a356bdab73c128a8a"
URL = "https://github.com/obsidianmd/obsidian-releases/releases/download/v1.10.6/Obsidian-1.10.6.dmg"
POLICY = '(version 1)\n(allow default)\n(deny network-outbound (remote ip "*:*"))\n'


def digest(path):
    with Path(path).open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest() if hasattr(hashlib, "file_digest") else hashlib.sha256(f.read()).hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def group_alive(pid):
    try:
        os.killpg(pid, 0)
        return True
    except ProcessLookupError:
        return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--binary-sha256", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--archive", required=True)
    parser.add_argument("--archive-sha256", required=True)
    parser.add_argument("--evidence", required=True)
    args = parser.parse_args()
    # Refuse before creating any fixture or running any executable on a personal host.
    if sys.platform != "darwin" or os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        parser.error("real editor qualification requires a fresh GitHub-hosted macOS VM")
    root = Path(os.environ["RUNNER_TEMP"]).resolve(strict=True)
    evidence = Path(args.evidence).resolve()
    if root not in evidence.parents:
        parser.error("evidence must be a new directory below RUNNER_TEMP")
    binary = Path(args.binary).resolve(strict=True)
    archive = Path(args.archive).resolve(strict=True)
    if not re.fullmatch(r"[0-9a-f]{40}", args.source_commit) or any(not re.fullmatch(r"[0-9a-f]{64}", value) for value in [args.binary_sha256, args.archive_sha256]):
        parser.error("source commit and SHA-256 pins must be full lowercase hashes")
    if digest(binary) != args.binary_sha256:
        parser.error("binary differs from the pinned archive executable")
    if digest(archive) != args.archive_sha256:
        parser.error("archive differs from its pinned downloaded bytes")
    evidence.mkdir(mode=0o700)
    harness = Path(__file__).resolve().parent
    env = {"PATH": "/usr/bin:/bin", "HOME": str(evidence / "home"),
           "TMPDIR": str(evidence / "temp") + "/", "LC_ALL": "en_US.UTF-8", "TZ": "UTC"}
    controller_env = dict(env, GITHUB_ACTIONS="true", RUNNER_ENVIRONMENT="github-hosted")
    for directory in ["home", "temp", "profile", "cache", "mount"]:
        (evidence / directory).mkdir(mode=0o700)
    info = {"status": "running", "source_commit": args.source_commit,
            "python_version": sys.version, "python_executable": sys.executable,
            "binary_sha256": args.binary_sha256, "obsidian_version": VERSION,
            "archive_sha256": args.archive_sha256,
            "download_url": URL, "download_sha256": SHA256, "download_bytes": SIZE,
            "harness_sha256": {p.name: digest(p) for p in [Path(__file__).resolve(), harness / "roundtrip.mjs"]},
            "runner": {key: os.environ.get(key) for key in ["RUNNER_ENVIRONMENT", "RUNNER_OS", "RUNNER_ARCH", "ImageOS", "ImageVersion", "GITHUB_SHA", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT"]}}
    save(evidence / "qualification.json", info)
    mounted = False
    started = time.monotonic()

    def command(name, argv, timeout=30):
        before = time.monotonic()
        p = subprocess.run(list(map(str, argv)), env=env, capture_output=True, timeout=timeout)
        (evidence / (name + ".stdout")).write_bytes(p.stdout)
        (evidence / (name + ".stderr")).write_bytes(p.stderr)
        with (evidence / "preparation.jsonl").open("a") as f:
            f.write(json.dumps({"name": name, "argv": list(map(str, argv)), "exit_code": p.returncode, "elapsed_seconds": time.monotonic() - before}) + "\n")
        if p.returncode:
            raise RuntimeError(name + " failed; inspect retained stderr")
        return p.stdout

    try:
        info["macos_version"] = command("macos-version", ["/usr/bin/sw_vers", "-productVersion"]).decode().strip()
        dmg = evidence / "Obsidian-1.10.6.dmg"
        before = time.monotonic()
        count = 0
        with urllib.request.urlopen(URL, timeout=30) as response, dmg.open("xb") as out:
            if response.geturl().split("/")[2] not in {"github.com", "release-assets.githubusercontent.com", "objects.githubusercontent.com"}:
                raise RuntimeError("download redirected outside official GitHub release assets")
            while True:
                block = response.read(1024 * 1024)
                if not block:
                    break
                count += len(block)
                if count > SIZE or time.monotonic() - before > 180:
                    raise RuntimeError("official app download exceeded pinned size/time bound")
                out.write(block)
        if count != SIZE or digest(dmg) != SHA256:
            raise RuntimeError("official app download differs from pinned bytes")
        command("attach", ["/usr/bin/hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", evidence / "mount", dmg], 60)
        mounted = True
        app = evidence / "Obsidian.app"
        command("copy", ["/usr/bin/ditto", evidence / "mount" / "Obsidian.app", app], 60)
        command("detach", ["/usr/bin/hdiutil", "detach", evidence / "mount"], 30)
        mounted = False
        command("codesign", ["/usr/bin/codesign", "--verify", "--deep", "--strict", "--verbose=2", app], 30)
        with (app / "Contents" / "Info.plist").open("rb") as f:
            installed = plistlib.load(f)["CFBundleShortVersionString"]
        if installed != VERSION:
            raise RuntimeError("download installer version differs from pinned version")
        # This is a new disposable app setting, not a fabricated vault registry.
        save(evidence / "profile" / "obsidian.json", {"updateDisabled": True})
        (evidence / "network-only.sb").write_text(POLICY)
        tcp = socket.socket()
        tcp.bind(("127.0.0.1", 0))
        tcp.listen()
        unix = socket.socket(socket.AF_UNIX)
        endpoint = str(evidence / "probe.sock")
        unix.bind(endpoint)
        unix.listen()
        probe = '''import socket, json, sys
out = {}
for kind, endpoint in [("tcp", ("127.0.0.1", PORT)), ("unix", ENDPOINT)]:
    s = socket.socket(socket.AF_INET if kind == "tcp" else socket.AF_UNIX)
    s.settimeout(1)
    try:
        s.connect(endpoint)
        out[kind] = "allowed"
    except OSError as e:
        out[kind] = e.errno
    s.close()
print(json.dumps(out))
sys.exit(0 if out == {"tcp": 1, "unix": "allowed"} else 1)
'''.replace("PORT", str(tcp.getsockname()[1])).replace("ENDPOINT", repr(endpoint))
        try:
            command("network-preflight", ["/usr/bin/sandbox-exec", "-f", evidence / "network-only.sb", "/usr/bin/python3", "-c", probe], 10)
        finally:
            tcp.close()
            unix.close()
            Path(endpoint).unlink()
        import shutil
        node = shutil.which("node")
        if not node:
            raise RuntimeError("Node.js is required for the inherited CDP pipe controller")
        info["node_version"] = command("node-version", [node, "--version"]).decode().strip()
        info["node_executable"] = node
        argv = [node, str(harness / "roundtrip.mjs"), str(binary), str(evidence), args.binary_sha256]
        with (evidence / "controller.stdout").open("wb") as out, (evidence / "controller.stderr").open("wb") as err:
            p = subprocess.Popen(argv, env=controller_env, stdout=out, stderr=err)
            controller_started = time.monotonic()
            app_observed = None
            while p.poll() is None:
                now = time.monotonic()
                if app_observed is None and (evidence / "app-pid.json").exists():
                    app_observed = now
                if now - controller_started > 330 or app_observed is not None and now - app_observed > 295:
                    # Independent watchdog covers a hung or unexpectedly failed controller.
                    if (evidence / "app-pid.json").exists():
                        pid = json.loads((evidence / "app-pid.json").read_text())["pid"]
                        if group_alive(pid):
                            os.killpg(pid, signal.SIGKILL)
                    p.kill()
                    p.wait(timeout=5)
                    raise RuntimeError("independent controller/app watchdog expired")
                time.sleep(0.2)
        if p.returncode:
            raise RuntimeError("real editor roundtrip failed; inspect result.json and controller logs")
        if digest(binary) != args.binary_sha256:
            raise RuntimeError("pinned lwiki executable changed")
        info["status"] = "passed"
    except Exception as error:
        info["status"] = "failed"
        info["error"] = str(error)
    finally:
        pidfile = evidence / "app-pid.json"
        if pidfile.exists():
            pid = json.loads(pidfile.read_text())["pid"]
            if group_alive(pid):
                os.killpg(pid, signal.SIGKILL)
                time.sleep(0.2)
                info["status"] = "failed"
                info["error"] = "controller left an owned process group; supervisor killed it"
            info["owned_process_group_gone"] = not group_alive(pid)
            if not info["owned_process_group_gone"]:
                info["status"] = "failed"
                info["error"] = "owned process group survived supervisor cleanup"
        info["binary_final_sha256"] = digest(binary)
        info["archive_final_sha256"] = digest(archive)
        if info["binary_final_sha256"] != args.binary_sha256:
            info["status"] = "failed"
            info["error"] = "pinned lwiki executable changed"
        if info["archive_final_sha256"] != args.archive_sha256:
            info["status"] = "failed"
            info["error"] = "pinned archive changed"
        if mounted:
            try:
                command("detach-finally", ["/usr/bin/hdiutil", "detach", evidence / "mount"], 15)
            except Exception as error:
                info["cleanup_error"] = str(error)
                info["status"] = "failed"
        info["elapsed_seconds"] = time.monotonic() - started
        save(evidence / "qualification.json", info)
    print(json.dumps({"status": info["status"], "evidence": str(evidence)}))
    return 0 if info["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
