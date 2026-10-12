#!/usr/bin/env python3
"""Qualify ordinary lookup with actual writes denied to disposable wiki folders.

macOS uses sandbox-exec. Linux uses sudo's nobody identity and files owned by
this runner, rather than chmod-only restrictions on the candidate's own files.
No provider, credentials, user configuration or production wiki is accessed.
"""
import argparse
import errno
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import stat
import subprocess
import sys
import tempfile
import time

MAX_CLI_CALLS = 10
TIMEOUT_SECONDS = 30
SOURCE = "source_00000000-0000-7000-8000-000000000005"
REVISION = "revision_00000000-0000-7000-8000-000000000008"
CONTENT = f"sources/{SOURCE}/revisions/{REVISION}/content.md"
CONTENT_SHA256 = "57b7c4c15cbd34cd9af8bdbe693045dda57f6146c33f6cc8e0bcf432dc9d97dc"
CONTENT_BLAKE3 = "blake3:c35d665a873789989333b21313e71940a044311bc72adabfbe06ea953d193a20"
FACT = "North Lab uses South Lab."
QUERY = "North Lab"
CONTROL = ".qualification-read-control"


class Unsupported(RuntimeError):
    """A required native enforcement mechanism is unavailable."""


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def inventory(root):
    found = {}
    for path in [root, *sorted(root.rglob("*"))]:
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode) or not (path.is_file() or path.is_dir()):
            raise RuntimeError(f"qualification fixture has an unsupported entry: {path}")
        row = {"kind": "file" if path.is_file() else "directory",
               "mode": stat.S_IMODE(info.st_mode), "mtime_ns": info.st_mtime_ns,
               "ctime_ns": info.st_ctime_ns,
               "uid": info.st_uid, "gid": info.st_gid}
        if path.is_file():
            row.update(sha256=digest(path), bytes=info.st_size)
        found[path.relative_to(root).as_posix()] = row
    return found


def make_readable(root):
    """Keep ownership; grant reads/traversal but never group/other writes."""
    for path in [root, *root.rglob("*")]:
        if path.is_symlink():
            raise RuntimeError("qualification input must not contain symlinks")
        path.chmod(0o755 if path.is_dir() else 0o644)


def isolated_environment(area):
    return {"HOME": str(area / "home"), "XDG_CONFIG_HOME": str(area / "config"),
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LC_ALL": "C", "TZ": "UTC"}


def sandbox_profile(protected):
    return "(version 1)\n(allow default)\n(deny network*)\n" + "".join(
        f"(deny file-write* (subpath {json.dumps(str(path.resolve()))}))\n"
        for path in protected)


def enforcement_command(system, command, env, *, sandbox=None, profile=None,
                        sudo=None, timeout=None):
    if system == "Darwin":
        if not sandbox or not profile:
            raise Unsupported("macOS sandbox-exec is required")
        return [sandbox, "-f", str(profile), *map(str, command)]
    if system == "Linux":
        if not sudo or not timeout:
            raise Unsupported("Linux requires sudo -n -u nobody and the timeout utility")
        # Clear inherited credentials/configuration again after crossing sudo.
        return [sudo, "-n", "-u", "nobody", "--", "/usr/bin/env", "-i",
                *[f"{key}={value}" for key, value in sorted(env.items())],
                timeout, "--signal=TERM", "--kill-after=2s", f"{TIMEOUT_SECONDS}s",
                *map(str, command)]
    raise Unsupported(f"native read-only lookup qualification is unsupported on {system}")


# The child tests creation, existing-data writes, deletion and permission repair.
# All targets are disposable control files; the captured Source stays immutable.
CONTROL_PROGRAM = r'''
import hashlib, json, os, pathlib, sys
root = pathlib.Path(sys.argv[1])
control = root / ".qualification-read-control"
result = {"uid": os.geteuid(), "read_sha256": hashlib.sha256(control.read_bytes()).hexdigest(), "denied": {}}
operations = {
    "create": lambda: (root / ".qualification-created").write_bytes(b"unexpected write"),
    "modify": lambda: control.write_bytes(b"unexpected write"),
    "chmod": lambda: control.chmod(0o666),
    "delete": lambda: control.unlink(),
}
for name, operation in operations.items():
    try:
        operation()
        result["denied"][name] = None
    except OSError as error:
        result["denied"][name] = error.errno
print(json.dumps(result))
'''


def validate_control(record, expected_uid, expected_hash):
    if record["exit"] != 0 or record["stderr"]:
        raise Unsupported("native denial/read control could not execute cleanly")
    try:
        result = json.loads(record["stdout"])
    except (ValueError, TypeError) as error:
        raise Unsupported("native denial control did not return JSON") from error
    if result.get("uid") != expected_uid or result.get("read_sha256") != expected_hash:
        raise RuntimeError("denial control used the wrong identity or could not read the fixture")
    denied = result.get("denied", {})
    if set(denied) != {"create", "modify", "chmod", "delete"} or any(
            code not in (errno.EACCES, errno.EPERM) for code in denied.values()):
        raise RuntimeError("writes or permission repair were not actually denied; candidate not qualified")
    return result


def cli_command(binary, root, arguments, *, machine=True):
    return [str(binary), "--offline", *(["--json"] if machine else []),
            "--no-user-config", "--wiki", str(root), *arguments]


def json_result(record, normalized):
    if record["exit"] != 0 or record["stderr"]:
        raise RuntimeError(f"lookup failed or polluted JSON stderr: {record['label']}")
    result = json.loads(record["stdout"])
    if result.get("ok") is not True or result["meta"]["network_used"] is not False:
        raise RuntimeError("lookup did not succeed offline")
    if result["meta"]["freshness"] != "verified_snapshot" or result["meta"]["index_generation"] is not None:
        raise RuntimeError("ordinary lookup did not return verified evidence without publication")
    if normalized:
        view = result["meta"].get("read_view", {})
        if view.get("kind") != "current_observation" or view.get("published_generation", 0) is not None:
            raise RuntimeError("normalized lookup did not use current canonical observation")
        if not view.get("fingerprint") or not view.get("observed_at"):
            raise RuntimeError("normalized observation provenance is missing")
        snapshot = result["data"].get("snapshot")
        if snapshot and (snapshot.get("generation", 0) is not None or "publication" in snapshot):
            raise RuntimeError("private observation was reported as a published generation")
    return result


def validate_content(search, context, read, content):
    hits = search["data"]["hits"]
    captured_hits = [hit for hit in hits if hit["locator"]["path"] == CONTENT]
    if not captured_hits:
        raise RuntimeError("search did not discover the expected captured note")
    for hit in captured_hits:
        excerpt = hit["excerpt"]
        start, end = excerpt["span"]["start"], excerpt["span"]["end"]
        citation = excerpt["citation"]
        if (start < 0 or end <= start or end > len(content)
                or excerpt["text"] != content[start:end].decode("utf-8")
                or hit["eligibility"] != "current" or not citation
                or citation["kind"] != "source"
                or citation["reference"]["source_id"] != SOURCE
                or citation["reference"]["source_revision"] != REVISION
                or citation["reference"]["span"] != excerpt["span"]):
            raise RuntimeError("search excerpt does not match exact current captured evidence")
    passages = context["data"]["passages"]
    matching = [passage for passage in passages if passage["locator"]["path"] == CONTENT]
    if FACT not in context["data"]["text"] or not matching:
        raise RuntimeError("context omitted the required fact or captured evidence")
    citations = 0
    for passage in matching:
        start, end = passage["span"]["start"], passage["span"]["end"]
        quote = content[start:end].decode("utf-8")
        if (start < 0 or end <= start or end > len(content) or passage["text"] != quote
                or passage["eligibility"] != "current" or passage["label"] != "captured_source"):
            raise RuntimeError("context passage does not match exact captured UTF-8 bytes")
        for citation in passage["citations"]:
            reference = citation["reference"]
            if (citation["kind"] != "source" or reference["source_id"] != SOURCE
                    or reference["source_revision"] != REVISION
                    or reference["span"] != passage["span"]):
                raise RuntimeError("context citation identity or span is wrong")
            citations += 1
    if not citations:
        raise RuntimeError("context returned no Source citation")
    outcome = read["data"]
    citation = outcome["source_citation"]
    reference = citation["citation"]["reference"]
    if (outcome["path"] != CONTENT or outcome["body"] != content.decode("utf-8")
            or outcome["truncated"] is not False or citation["eligibility"] != "current"
            or citation["citation"]["kind"] != "source"
            or reference["source_id"] != SOURCE or reference["source_revision"] != REVISION
            or reference["span"] != {"start": 0, "end": len(content)}
            or reference["quote_hash"] != CONTENT_BLAKE3):
        raise RuntimeError("read did not return the exact current Source bytes and pinned citation")
    return {"required_fact": FACT, "context_exact_source_citations": citations,
            "full_read_pinned_blake3_verified": True,
            "partial_quote_blake3_independently_computed": False}


class Runner:
    def __init__(self, evidence, env, cwd):
        self.evidence, self.env, self.cwd = evidence, env, cwd
        self.records = []
        self.cli_calls = 0

    def run(self, label, command, *, cli=False):
        if cli:
            if self.cli_calls >= MAX_CLI_CALLS:
                raise RuntimeError("qualification CLI call allowance exceeded")
            self.cli_calls += 1
        start = time.monotonic()
        try:
            done = subprocess.run(command, cwd=self.cwd, env=self.env, capture_output=True,
                                  text=True, timeout=TIMEOUT_SECONDS + 5)
            row = {"exit": done.returncode, "stdout": done.stdout, "stderr": done.stderr}
        except subprocess.TimeoutExpired as error:
            def decoded(value):
                return value.decode(errors="replace") if isinstance(value, bytes) else (value or "")
            row = {"exit": None, "stdout": decoded(error.stdout),
                   "stderr": decoded(error.stderr), "timed_out": True}
        row.update(label=label, argv=list(map(str, command)),
                   elapsed_seconds=time.monotonic() - start)
        self.records.append(row)
        (self.evidence / (label + ".json")).write_text(json.dumps(row, indent=2) + "\n")
        return row


def qualify(binary, repo, evidence):
    system = platform.system()
    sandbox = shutil.which("sandbox-exec") if system == "Darwin" else None
    sudo = shutil.which("sudo") if system == "Linux" else None
    timeout = shutil.which("timeout") if system == "Linux" else None
    if system not in ("Darwin", "Linux"):
        raise Unsupported(f"unsupported native platform: {system}")
    target_uid = os.geteuid()
    if system == "Linux":
        import pwd
        try:
            target_uid = pwd.getpwnam("nobody").pw_uid
        except KeyError as error:
            raise Unsupported("Linux nobody identity is unavailable") from error
        if target_uid == os.geteuid():
            raise Unsupported("fixture runner must own its files independently of nobody")
    if (system == "Darwin" and not sandbox) or (system == "Linux" and (not sudo or not timeout)):
        raise Unsupported("required native sandbox/sudo/timeout mechanism is unavailable")
    seed = repo / "tests/fixtures/bootstrap/vault"
    if digest(seed / CONTENT) != CONTENT_SHA256:
        raise RuntimeError("captured fixture bytes no longer match the pinned citation")
    source_pin = digest(binary)
    with tempfile.TemporaryDirectory(prefix="lwiki-read-qualification-") as temporary:
        area = Path(temporary).resolve()
        area.chmod(0o755)
        for name in ("home", "config", "cwd", "bin"):
            (area / name).mkdir(mode=0o755)
        executable = area / "bin/lwiki"
        shutil.copyfile(binary, executable)
        executable.chmod(0o755)
        if digest(executable) != source_pin:
            raise RuntimeError("copied candidate does not match source pin")
        env = isolated_environment(area)
        runner = Runner(evidence, env, area / "cwd")
        report = {"status": "running", "platform": system, "binary_sha256": source_pin,
                  "cli_call_limit": MAX_CLI_CALLS, "command_timeout_seconds": TIMEOUT_SECONDS,
                  "enforcement": "sandbox-exec" if system == "Darwin" else "sudo-nobody",
                  "network_enforcement": "deny network*" if system == "Darwin" else
                  "offline commands and empty configuration; no kernel network isolation",
                  "fixtures": [], "scope": "fixed native lookup regression; independent UX acceptance is separate"}
        (evidence / "receipt.json").write_text(json.dumps(report, indent=2) + "\n")
        protected = [area / "legacy", area / "normalized", area / "home", area / "config"]
        profile = area / "readonly.sb"
        profile.write_text(sandbox_profile(protected))
        for normalized in (False, True):
            label = "normalized" if normalized else "legacy"
            root = area / label
            shutil.copytree(seed, root)
            make_readable(root)
            preparation = ["index", "rebuild", "--normalized"] if normalized else ["index", "sync"]
            prepared = runner.run(label + "-prepare", cli_command(executable, root, preparation), cli=True)
            if prepared["exit"] != 0 or prepared["stderr"] or not json.loads(prepared["stdout"])["ok"]:
                raise RuntimeError(f"could not prepare disposable {label} fixture")
            shutil.rmtree(root / ".wiki/cache")
            (root / CONTROL).write_bytes(b"readable denial control\n")
            make_readable(root)
            before = inventory(root)
            if system == "Linux" and any(row["uid"] == target_uid for row in before.values()):
                raise Unsupported("nobody owns a protected fixture entry and could repair its permissions")
            (evidence / (label + "-tree-before.json")).write_text(json.dumps(before, indent=2) + "\n")
            protected_before = {name: inventory(area / name) for name in ("home", "config")}
            def denied(command):
                return enforcement_command(system, command, env, sandbox=sandbox, profile=profile,
                                           sudo=sudo, timeout=timeout)
            result = {"layout": label, "cache_deleted": True}
            try:
                control = runner.run(label + "-denial-control", denied([
                    sys.executable, "-B", "-c", CONTROL_PROGRAM, root]))
                result["positive_control"] = validate_control(control, target_uid, before[CONTROL]["sha256"])
                answers = []
                for kind, arguments in (
                        ("search", ["search", QUERY, "--mode", "lexical"]),
                        ("context", ["context", QUERY, "--mode", "lexical"]),
                        ("read", ["read", "--path", CONTENT, "--max-bytes", "12000"])):
                    row = runner.run(label + "-" + kind, denied(cli_command(executable, root, arguments)), cli=True)
                    answers.append(json_result(row, normalized))
                result["content_checks"] = validate_content(*answers, (root / CONTENT).read_bytes())
                human = runner.run(label + "-human-read", denied(cli_command(
                    executable, root, ["read", "--path", CONTENT, "--max-bytes", "12000"], machine=False)), cli=True)
                if (human["exit"] != 0 or human["stdout"] != (root / CONTENT).read_text()
                        or "Provider network: not used" not in human["stderr"]):
                    raise RuntimeError("human read did not keep exact text on stdout and network status on stderr")
            finally:
                after = inventory(root)
                (evidence / (label + "-tree-after.json")).write_text(json.dumps(after, indent=2) + "\n")
                result["tree_unchanged"] = before == after
                result["private_configuration_unchanged"] = all(
                    inventory(area / name) == previous for name, previous in protected_before.items())
                report["fixtures"].append(result)
                report["cli_calls"] = runner.cli_calls
                (evidence / "receipt.json").write_text(json.dumps(report, indent=2) + "\n")
            if not result["tree_unchanged"] or not result["private_configuration_unchanged"]:
                raise RuntimeError("denied lookup changed wiki bytes, entries, metadata or isolated configuration")
        if digest(executable) != source_pin or digest(binary) != source_pin:
            raise RuntimeError("candidate executable changed during qualification")
        report.update(status="passed", qualification_passed=True,
                      commands=len(runner.records), cli_calls=runner.cli_calls)
        return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="new evidence directory")
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    status = {"status": "failed"}
    exit_code = 1
    try:
        status = qualify(args.binary.resolve(strict=True), args.repo_root.resolve(strict=True), args.output)
        exit_code = 0
    except Unsupported as error:
        status = {"status": "unsupported", "reason": str(error), "qualification_passed": False}
        exit_code = 2
    except (OSError, ValueError, KeyError, TypeError, RuntimeError) as error:
        status = {"status": "failed", "reason": str(error), "qualification_passed": False}
    (args.output / "result.json").write_text(json.dumps(status, indent=2) + "\n")
    print(json.dumps(status), flush=True)
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
