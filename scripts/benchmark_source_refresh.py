#!/usr/bin/env python3
"""Offline, actual-CLI refresh benchmark; retains every disposable fixture and failure.

Example (explicit immutable binary paths, new output directory):
  python3 scripts/benchmark_source_refresh.py --binary /tmp/new/lwiki \
    --baseline-binary /tmp/old/lwiki --workdir /tmp/refresh-run --tier small

Seed via real init/source add/refresh commands, clone identical canonical state,
then run no-op/title-only/changed refresh followed immediately by indexed context.
This is a development diagnostic, not the full large-vault acceptance protocol.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import re
import shutil
import signal
import stat
import subprocess
import sys
import threading
import time

try:
    import blake3
except ImportError:
    blake3 = None  # Optional; never install dependencies or invoke a provider.

GIB = 1024 ** 3
TIERS = {"tiny": 2, "small": 100, "1000": 1000, "10000": 10000}
REPO = Path(__file__).resolve().parent.parent


def write_json(path, data):
    path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def inventory(root):
    """Physical/logical allocation, no links or special files; no payload hashing."""
    physical = logical = files = 0
    for base, dirs, names in os.walk(root, followlinks=False):
        for name in dirs + names:
            entry = Path(base) / name
            info = entry.lstat()
            if not (stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode)):
                raise ValueError(f"unsafe fixture entry: {entry}")
            if info.st_nlink != 1 and stat.S_ISREG(info.st_mode):
                raise ValueError(f"hardlinked fixture entry: {entry}")
            physical += info.st_blocks * 512
            if stat.S_ISREG(info.st_mode):
                logical += info.st_size
                files += 1
    return dict(allocated_bytes=physical, logical_bytes=logical, files=files)


def payload(seed, source, version, size):
    marker = f"refreshprobe{source:06d}v{version:06d}"
    text = f"# Synthetic capture\n\n{marker}: café 東京 field observation.\n".encode()
    rng = random.Random(f"{seed}:{source}:{version}")
    while len(text) < size:
        words = " ".join(f"observation{rng.getrandbits(32):08x}" for _ in range(8))
        text += (words + ".\n").encode()
    return text[:size], marker


def nearest_rank(samples):
    values = sorted(samples)
    if not values:
        return None
    return {"count": len(values), "p50_seconds": values[math.ceil(.5 * len(values)) - 1],
            "p95_seconds": values[math.ceil(.95 * len(values)) - 1],
            "max_seconds": values[-1]}


def process_tree_rss(pid):
    # ps reports KiB on the supported native macOS/Linux hosts.
    data = subprocess.run(["/bin/ps", "-axo", "pid=,ppid=,rss="],
                          capture_output=True, text=True, check=True, timeout=3)
    rows = [tuple(map(int, row.split())) for row in data.stdout.splitlines() if row.strip()]
    children = {pid}
    while True:
        expanded = children | {child for child, parent, _ in rows if parent in children}
        if expanded == children:
            return sum(rss * 1024 for child, _, rss in rows if child in children)
        children = expanded


def stop(proc):
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


class Runner:
    def __init__(self, args, pins):
        self.args, self.pins = args, pins
        self.commands = []
        self.started = time.monotonic()
        # No ambient provider variables or credentials are passed to children.
        self.env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8"}

    def check_resources(self):
        remaining = self.args.run_seconds - (time.monotonic() - self.started)
        if remaining <= 0:
            raise ValueError("whole-run elapsed ceiling")
        resources = inventory(self.args.workdir)
        resources["free_bytes"] = shutil.disk_usage(self.args.workdir).free
        if resources["allocated_bytes"] > self.args.max_disk_gib * GIB:
            raise ValueError("disposable allocated-disk ceiling")
        if resources["free_bytes"] < 32 * GIB:
            raise ValueError("32 GiB free-space floor")
        return resources, remaining

    def run(self, binary, vault, words, phase, label, case=None):
        before, remaining = self.check_resources()
        if digest(binary) != self.pins[str(binary)]:
            raise ValueError("pinned binary changed")
        number = len(self.commands)
        stem = self.args.workdir / "commands" / f"{number:05d}-{label}"
        stdout, stderr = Path(str(stem) + ".stdout.json"), Path(str(stem) + ".stderr.txt")
        argv = [str(binary), "--json", "--offline", "--wiki", str(vault), *map(str, words)]
        timed = ["/usr/bin/time", "-l" if sys.platform == "darwin" else "-v", *argv]
        record = dict(label=label, phase=phase, case=case, argv=argv, supervisor_argv=timed,
                      cwd=str(self.args.workdir), binary_sha256=self.pins[str(binary)],
                      stdout=str(stdout), stderr=str(stderr), resource_before=before,
                      evidence_path=str(stem) + ".command.json")
        self.commands.append(record)
        write_json(Path(str(stem) + ".command.json"), record)
        done = threading.Event()
        monitor = {"rss_peak_bytes": 0, "violation": None}
        rss_limit = (1 if phase == "context" else 8) * GIB

        def supervise(proc):
            next_disk = time.monotonic() + 5
            while not done.is_set():
                try:
                    monitor["rss_peak_bytes"] = max(monitor["rss_peak_bytes"], process_tree_rss(proc.pid))
                    if monitor["rss_peak_bytes"] > rss_limit:
                        monitor["violation"] = "process-tree RSS ceiling"
                    if shutil.disk_usage(self.args.workdir).free < 32 * GIB:
                        monitor["violation"] = "free-space floor during command"
                    if stdout.stat().st_size + stderr.stat().st_size > 4 * 1024 * 1024:
                        monitor["violation"] = "command output ceiling"
                    if time.monotonic() >= next_disk:
                        resources = inventory(self.args.workdir)
                        if resources["allocated_bytes"] > self.args.max_disk_gib * GIB:
                            monitor["violation"] = "allocated disk ceiling during command"
                        next_disk = time.monotonic() + 5
                except Exception as error:
                    monitor["violation"] = f"resource monitor failed: {error}"
                if monitor["violation"]:
                    stop(proc)
                    return
                done.wait(.1)

        with stdout.open("xb") as out, stderr.open("xb") as err:
            start = time.monotonic()
            proc = subprocess.Popen(timed, stdout=out, stderr=err, cwd=self.args.workdir,
                                    env=self.env, start_new_session=True)
            watcher = threading.Thread(target=supervise, args=(proc,), daemon=True)
            watcher.start()
            def expire():
                if proc.poll() is None:
                    monitor["violation"] = "command or whole-run elapsed ceiling"
                    stop(proc)

            deadline = threading.Timer(min(self.args.command_seconds, remaining), expire)
            deadline.daemon = True
            deadline.start()
            try:
                code = proc.wait()
            except BaseException:
                stop(proc)
                proc.wait()
                raise
            finally:
                record["elapsed_seconds"] = time.monotonic() - start
                deadline.cancel()
                done.set()
                watcher.join()
        record.update(returncode=code, **monitor)
        # Native maximum supplements sampling for short-lived process peaks.
        native = re.search(r"(\d+)\s+maximum resident set size", stderr.read_text())
        if sys.platform != "darwin":
            native = re.search(r"Maximum resident set size \(kbytes\):\s*(\d+)", stderr.read_text())
        record["native_peak_rss_bytes"] = int(native[1]) * (1 if sys.platform == "darwin" else 1024) if native else None
        try:
            record["resource_after"], _ = self.check_resources()
            if digest(binary) != self.pins[str(binary)]:
                raise ValueError("pinned binary changed during command")
            if record["native_peak_rss_bytes"] is None:
                raise ValueError("native peak RSS unavailable")
            if record["native_peak_rss_bytes"] > rss_limit:
                raise ValueError("native peak RSS ceiling")
            envelope = json.loads(stdout.read_text())
            if code or monitor["violation"] or envelope.get("ok") is not True:
                raise ValueError(f"CLI failed: {monitor['violation'] or envelope.get('error') or code}")
            if envelope.get("meta", {}).get("network_used") is not False:
                raise ValueError("offline network_used evidence missing/true")
            record["passed"] = True
            record["meta"] = envelope["meta"]
            return envelope["data"], record
        except Exception as error:
            record.update(passed=False, failure=str(error))
            raise
        finally:
            write_json(Path(str(stem) + ".command.json"), record)


def verify_context(data, vault, source, revision, expected, marker):
    verification = data.get("verification", {})
    if verification.get("mode") != "indexed_evidence" or verification.get("global_membership_verified") is not False:
        raise ValueError("missing explicit indexed-evidence scope")
    if marker not in data.get("text", "") or not data.get("passages"):
        raise ValueError("immediate indexed context omitted new/current marker")
    current_path = f"sources/{source}/revisions/{revision}/content.md"
    if (vault / current_path).read_bytes() != expected:
        raise ValueError("current immutable content differs from request bytes")
    found = False
    for passage in data["passages"]:
        if passage.get("eligibility") != "current" or passage.get("label") != "captured_source":
            raise ValueError("noncurrent or mislabeled returned evidence")
        if passage["locator"]["path"] != current_path:
            raise ValueError("wrong source/revision locator")
        span = passage["span"]
        start, end = span["start"], span["end"]
        if not isinstance(start, int) or not isinstance(end, int) or not 0 <= start < end <= len(expected):
            raise ValueError("invalid citation byte span")
        if expected[start:end].decode("utf-8") != passage["text"]:
            raise ValueError("citation passage differs from exact immutable bytes")
        citations = passage.get("citations", [])
        if len(citations) != 1 or citations[0].get("kind") != "source":
            raise ValueError("missing captured-source citation")
        ref = citations[0]["reference"]
        if ref.get("source_id") != source or ref.get("source_revision") != revision or ref.get("span") != span:
            raise ValueError("wrong source/revision/span in citation")
        if not re.fullmatch(r"blake3:[0-9a-f]{64}", ref.get("quote_hash", "")):
            raise ValueError("missing citation quote hash")
        if blake3 is not None and ref["quote_hash"] != "blake3:" + blake3.blake3(expected[start:end]).hexdigest():
            raise ValueError("citation quote hash differs from exact cited bytes")
        found |= marker in passage["text"]
    if not found:
        raise ValueError("marker absent from cited passages")


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="Explicit pinned candidate executable")
    parser.add_argument("--baseline-binary", type=Path, help="Optional old executable; also seeds the shared fixture")
    parser.add_argument("--workdir", type=Path, required=True, help="New nonexistent disposable directory; retained on failure")
    parser.add_argument("--tier", choices=TIERS, default="small", help="small=100, tiny=2; controls=1000/10000")
    parser.add_argument("--bytes-per-source", type=int, default=100 * 1024)
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--history-revisions", type=int, default=0, help="Fixed extra target revisions, via actual CLI refresh")
    parser.add_argument("--seed", type=int, default=731)
    parser.add_argument("--command-seconds", type=int, default=900)
    parser.add_argument("--run-seconds", type=int, default=3600)
    parser.add_argument("--max-disk-gib", type=int, default=8, help="Disposable allocation ceiling, maximum 100 GiB")
    args = parser.parse_args()
    if not 1024 <= args.bytes_per_source <= 1024 * 1024 or not 1 <= args.trials <= 20:
        parser.error("source bytes must be 1 KiB..1 MiB; trials 1..20")
    if not 0 <= args.history_revisions <= 32 or not 1 <= args.max_disk_gib <= 100:
        parser.error("history revisions must be 0..32; disk ceiling 1..100 GiB")
    if not 1 <= args.command_seconds <= 4 * 3600 or not args.command_seconds <= args.run_seconds <= 24 * 3600:
        parser.error("command/run time exceeds large-vault envelope or is inconsistent")
    if sys.platform not in ("darwin", "linux"):
        parser.error("native RSS monitoring is available only on macOS/Linux")
    return args


def main():
    args = arguments()
    args.binary = args.binary.expanduser().resolve(strict=True)
    if args.baseline_binary:
        args.baseline_binary = args.baseline_binary.expanduser().resolve(strict=True)
    binaries = [("baseline", args.baseline_binary)] if args.baseline_binary else []
    binaries.append(("candidate", args.binary))
    for _, binary in binaries:
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise ValueError(f"binary is not an executable regular file: {binary}")
    args.workdir = args.workdir.expanduser().absolute()
    if args.workdir.exists() or args.workdir.is_symlink():
        raise ValueError("workdir must be new; existing data will not be modified")
    args.workdir.mkdir(parents=True)
    (args.workdir / "commands").mkdir()
    pins = {str(binary): digest(binary) for _, binary in binaries}
    report = dict(protocol_version=1, status="incomplete", binary_pins=pins,
                  harness_sha256=digest(Path(__file__)), host=dict(platform=platform.platform(),
                  cpu_count=os.cpu_count()), arguments={key: str(value) if isinstance(value, Path) else value
                  for key, value in vars(args).items()}, commands=[], failures=[], results=[],
                  unavailable=["held reader: no public CLI command retains a SQL read transaction",
                  "logical filesystem/SQL counters and power/background conditions: unavailable"],
                  citation_hash_backend="python-blake3" if blake3 is not None else None,
                  interpretation="Whole CLI processes including /usr/bin/time launch, offline; setup/rebuild excluded from refresh/context timings. N sequential CLI adds can be quadratic: they seed authentic fixtures, not an import scaling qualification. Warm filesystem/index pages, not OS-cold. Supervisor samples ps/free space every ~100ms and inventory every 5s; pre/post untimed inventories warm metadata. Native RSS supplements sampling, which may overshoot. Statistics include only completed correct refresh/context pairs; all command durations/failures remain recorded and aborted tasks remain unrun. No full-scale, semantic, import-capacity, recovery, or normalized-activation qualification.")
    if blake3 is None:
        report["unavailable"].append("quote-hash recomputation: optional blake3 module unavailable; exact cited bytes/IDs/span checked, full citation correctness unqualified")
    write_json(args.workdir / "protocol.json", report)
    runner = Runner(args, pins)
    report["commands"] = runner.commands
    try:
        _, _ = runner.check_resources()
        count = TIERS[args.tier]
        # Conservative preallocation admission; actual physical usage is retained.
        forecast = args.bytes_per_source * (count + args.history_revisions + args.trials) * (3 + 2 * len(binaries)) + 512 * 1024 * 1024
        if forecast > args.max_disk_gib * GIB or shutil.disk_usage(args.workdir).free - forecast < 32 * GIB:
            raise ValueError("fixture/clones/history forecast exceeds disk admission")
        inputs = args.workdir / "inputs"
        inputs.mkdir()
        seed_vault = args.workdir / "seed vault"
        seed_binary = args.baseline_binary or args.binary
        runner.run(seed_binary, seed_vault, ["init", seed_vault], "setup", "init")
        sources = []
        for index in range(count):
            body, marker = payload(args.seed, index, 0, args.bytes_per_source)
            path = inputs / f"source-{index:06d}.md"
            path.write_bytes(body)
            data, _ = runner.run(seed_binary, seed_vault, ["source", "add", path, "--title", f"Synthetic capture {index:06d}"], "setup", f"add-{index:06d}")
            sources.append(dict(index=index, source_id=data["allocated_ids"]["source"],
                                revision_id=data["allocated_ids"]["revision"], bytes=len(body),
                                sha256=hashlib.sha256(body).hexdigest(), marker=marker))
            parent = seed_vault / "sources" / sources[-1]["source_id"] / "revisions" / sources[-1]["revision_id"]
            if (parent / "original.bin").read_bytes() != body or (parent / "content.md").read_bytes() != body:
                raise ValueError("CLI seed capture is not byte-exact")
        source, revision = sources[0]["source_id"], sources[0]["revision_id"]
        target = inputs / "target.md"
        for version in range(1, args.history_revisions + 1):
            body, _ = payload(args.seed, 0, version, args.bytes_per_source)
            target.write_bytes(body)
            data, _ = runner.run(seed_binary, seed_vault, ["source", "refresh", source, "--file", target], "setup", f"history-{version}")
            revision = data["allocated_ids"]["revision"]
        current, marker = payload(args.seed, 0, args.history_revisions, args.bytes_per_source)
        sources[0].update(revision_id=revision, sha256=hashlib.sha256(current).hexdigest(), marker=marker)
        write_json(args.workdir / "corpus.json", dict(seed=args.seed, sources=sources,
                   current_content_bytes=count * args.bytes_per_source, source_count=count,
                   target_prior_extra_revisions=args.history_revisions, target_current_revision=revision,
                   template="Actual CLI init/source add/refresh; no direct record or SQL generation"))
        report["corpus_manifest_sha256"] = digest(args.workdir / "corpus.json")
        report["corpus_current_bytes"] = count * args.bytes_per_source
        report["corpus_current_sources"] = count
        for name, binary in binaries:
            runner.check_resources()
            vault = args.workdir / f"{name} vault"
            shutil.copytree(seed_vault, vault)
            runner.run(binary, vault, ["index", "rebuild"], "setup", f"{name}-rebuild")
            version, current_revision = args.history_revisions, revision
            expected_title = "Synthetic capture 000000"
            for trial in range(args.trials):
                for case in ("noop", "title-only", "changed"):
                    if case == "changed":
                        version += 1
                    expected, marker = payload(args.seed, 0, version, args.bytes_per_source)
                    target.write_bytes(expected)
                    words = ["source", "refresh", source, "--file", target]
                    if case == "title-only":
                        expected_title = f"Synthetic display {trial}"
                        words += ["--title", expected_title]
                    prior_manifest = vault / "sources" / source / "revisions" / current_revision / "revision.md"
                    prior_manifest_hash = digest(prior_manifest)
                    data, refresh = runner.run(binary, vault, words, "refresh", f"{name}-{trial}-{case}", case)
                    observed_revision = data["allocated_ids"]["revision"]
                    try:
                        if (case == "changed") == (observed_revision == current_revision):
                            raise ValueError("refresh did not preserve/create revision as expected")
                        if digest(prior_manifest) != prior_manifest_hash:
                            raise ValueError("refresh rewrote a retained immutable revision manifest")
                        # Fixture titles use only these plain ASCII strings; no YAML parser is required.
                        source_note = (vault / "sources" / source / "source.md").read_text()
                        title = re.search(r"^title:[ \t]*(.+)$", source_note, re.MULTILINE)
                        if title is None or title[1].strip().strip("\"'") != expected_title:
                            raise ValueError("source display title was not preserved/explicitly updated")
                    except Exception as error:
                        refresh.update(passed=False, failure=str(error))
                        write_json(Path(refresh["evidence_path"]), refresh)
                        raise
                    current_revision = observed_revision
                    query = ["context", marker, "--scope", "indexed-evidence", "--no-sync", "--mode", "lexical",
                             "--source-id", source, "--limit", "3", "--candidates", "16",
                             "--max-bytes", "12000", "--max-tokens", "3000"]
                    context, observed = runner.run(binary, vault, query, "context", f"{name}-{trial}-{case}-context", case)
                    try:
                        verify_context(context, vault, source, current_revision, expected, marker)
                    except Exception as error:
                        observed.update(passed=False, failure=str(error))
                        write_json(Path(observed["evidence_path"]), observed)
                        raise
                    observed["verification"] = context["verification"]
                    write_json(Path(observed["evidence_path"]), observed)
                    report["results"].append(dict(binary=name, trial=trial, case=case,
                        refresh_seconds=refresh["elapsed_seconds"], context_seconds=observed["elapsed_seconds"],
                        source_id=source, revision_id=current_revision, input_sha256=hashlib.sha256(expected).hexdigest(),
                        marker=marker, passed=True))
                    write_json(args.workdir / "summary.json", report)
        report["status"] = "passed-development-diagnostic"
    except Exception as error:
        report["status"] = "failed-or-refused"
        report["failures"].append(str(error))
    finally:
        report["total_seconds"] = time.monotonic() - runner.started
        report["statistics"] = {name: {case: {phase: nearest_rank([
            row[phase + "_seconds"] for row in report["results"]
            if row["binary"] == name and row["case"] == case])
            for phase in ("refresh", "context")} for case in ("noop", "title-only", "changed")}
            for name, _ in binaries}
        report["disk_after"] = inventory(args.workdir)
        write_json(args.workdir / "summary.json", report)
    print(json.dumps(dict(status=report["status"], summary=str(args.workdir / "summary.json"))))
    return 0 if report["status"] == "passed-development-diagnostic" else 1


if __name__ == "__main__":
    sys.exit(main())
