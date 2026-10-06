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
from pathlib import Path, PurePosixPath
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
    root_info = root.lstat()
    if not stat.S_ISDIR(root_info.st_mode):
        raise ValueError("account root must be a regular directory")
    physical = root_info.st_blocks * 512
    logical = files = vanished = 0
    def unreadable(error):
        if not isinstance(error, FileNotFoundError):
            raise error
    for base, dirs, names in os.walk(root, followlinks=False, onerror=unreadable):
        for name in dirs + names:
            entry = Path(base) / name
            try:
                info = entry.lstat()
            except FileNotFoundError:
                vanished += 1  # Atomic private-stage renames during a timed sample.
                continue
            if not (stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode)):
                raise ValueError(f"unsafe fixture entry: {entry}")
            if info.st_nlink != 1 and stat.S_ISREG(info.st_mode):
                raise ValueError(f"hardlinked fixture entry: {entry}")
            physical += info.st_blocks * 512
            if stat.S_ISREG(info.st_mode):
                logical += info.st_size
                files += 1
    return dict(allocated_bytes=physical, logical_bytes=logical, files=files,
                vanished_during_sample=vanished)


def strict_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate manifest key: {key}")
        result[key] = value
    return result


def blake_digest(path):
    if blake3 is None:
        raise ValueError("normalized preseed requires the blake3 Python package for exact verification")
    value = blake3.blake3()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return "blake3:" + value.hexdigest()


def validate_preseed(args, runner):
    """Authenticate a closed exported fixture without opening its SQLite files."""
    runner.check_resources()
    manifest = args.preseed / "corpus.json"
    if manifest.is_symlink() or not manifest.is_file() or manifest.stat().st_size > 64 * 1024 * 1024:
        raise ValueError("missing/unsafe/oversized preseed manifest")
    data = json.loads(manifest.read_text(), object_pairs_hook=strict_object)
    required = {"version", "kind", "seed", "bytes_per_source", "source_count",
                "current_content_bytes", "target_prior_extra_revisions", "target_current_revision",
                "sources", "vault_id", "snapshot", "proof_layout_version", "revision_ownership_version",
                "files", "generator"}
    if set(data) != required or data["version"] != 1 or data["kind"] != "normalized-refresh-fixture":
        raise ValueError("unsupported preseed envelope")
    count = args.source_count if getattr(args, "scale_attribution", False) else TIERS[args.tier]
    expected = {"seed": args.seed, "bytes_per_source": args.bytes_per_source,
                "source_count": count, "current_content_bytes": count * args.bytes_per_source,
                "target_prior_extra_revisions": 0, "proof_layout_version": 2,
                "revision_ownership_version": 1}
    if args.history_revisions or any(type(data[key]) is not int or data[key] != value for key, value in expected.items()):
        raise ValueError("preseed dimensions/layout differ from declared diagnostic")
    snapshot = data["snapshot"]
    if snapshot.get("generation") != 1 or snapshot.get("publication", {}).get("version") != 1:
        raise ValueError("preseed must name its initial normalized publication")
    file_id = snapshot["publication"].get("file_id", "")
    if not re.fullmatch(r"[0-9a-f]{32}", file_id):
        raise ValueError("invalid preseed selected file identity")
    vault = args.preseed / "vault"
    if vault.is_symlink() or not vault.is_dir():
        raise ValueError("preseed vault must be an owned regular directory")
    files = data["files"]
    if not isinstance(files, list) or not 1 <= len(files) <= 8 * count + 128:
        raise ValueError("preseed file count exceeds fixture bound")
    pinned = {}
    for entry in files:
        if set(entry) != {"path", "bytes", "blake3"}:
            raise ValueError("invalid preseed file entry")
        relative = entry["path"]
        path = PurePosixPath(relative)
        if (not relative or path.is_absolute() or str(path) != relative or ".." in path.parts
                or "\\" in relative or relative in pinned):
            raise ValueError("unsafe/duplicate preseed file path")
        physical = vault.joinpath(*path.parts)
        if type(entry["bytes"]) is not int or entry["bytes"] < 0 or not re.fullmatch(r"blake3:[0-9a-f]{64}", entry["blake3"]):
            raise ValueError("invalid preseed byte/hash binding")
        if physical.stat().st_size != entry["bytes"] or blake_digest(physical) != entry["blake3"]:
            raise ValueError(f"preseed file differs from its pin: {relative}")
        pinned[relative] = entry
    # inventory rejects links and nonregular files before any fixture is copied.
    actual = {str(path.relative_to(vault)) for path in vault.rglob("*") if path.is_file()}
    if actual != set(pinned):
        raise ValueError("preseed has missing or unlisted files")
    for suffix in (".sqlite", ".sqlite-wal", ".sqlite-shm"):
        if f".wiki/cache/catalogs/{file_id}{suffix}" not in pinned:
            raise ValueError("preseed is missing its selected database/sidecar pair")
    sources = data["sources"]
    if len(sources) != count:
        raise ValueError("preseed source count differs")
    ids = set()
    for index, source in enumerate(sources):
        if set(source) != {"index", "source_id", "revision_id", "title", "bytes", "blake3", "marker"}:
            raise ValueError("invalid preseed source envelope")
        if (source["index"] != index or source["bytes"] != args.bytes_per_source
                or source["marker"] != f"refreshprobe{index:06d}v000000"
                or source["title"] != f"Synthetic capture {index:06d}"
                or not re.fullmatch(r"(?:source_[A-Za-z0-9_-]+|[0-9]{39})", source["source_id"])
                or not re.fullmatch(r"revision_[A-Za-z0-9_-]+", source["revision_id"])
                or source["source_id"] in ids):
            raise ValueError("invalid preseed source identity/sequence")
        ids.add(source["source_id"])
        prefix = f"sources/{source['source_id']}/revisions/{source['revision_id']}/"
        for leaf in ("original.bin", "content.md"):
            entry = pinned.get(prefix + leaf, {})
            if entry.get("bytes") != source["bytes"] or entry.get("blake3") != source["blake3"]:
                raise ValueError("source bytes disagree with pinned current revision")
        body = (vault / (prefix + "content.md")).read_bytes()
        if source["marker"] not in body.decode("utf-8"):
            raise ValueError("source marker absent from exact UTF-8 content")
    if data["target_current_revision"] != sources[0]["revision_id"]:
        raise ValueError("target revision differs from source manifest")
    runner.check_resources()
    return data, digest(manifest)


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
        self.env["XDG_CONFIG_HOME"] = str(args.workdir / "config")

    def check_resources(self):
        remaining = self.args.run_seconds - (time.monotonic() - self.started)
        if remaining <= 0:
            raise ValueError("whole-run elapsed ceiling")
        resources = inventory(self.args.account_root)
        resources["free_bytes"] = shutil.disk_usage(self.args.workdir).free
        ceiling = getattr(self.args, "attribution_allocation_limit", self.args.max_disk_gib * GIB)
        if resources["allocated_bytes"] > ceiling:
            raise ValueError("disposable allocated-disk ceiling")
        floor = (40 if getattr(self.args, "scale_attribution", False) else 32) * GIB
        if resources["free_bytes"] < floor:
            raise ValueError("free-space floor including declared reserve" if getattr(self.args, "scale_attribution", False) else "32 GiB free-space floor")
        return resources, remaining

    def run(self, binary, vault, words, phase, label, case=None, *, driver_task=None,
            expected_error=None, export_environment=None):
        before, remaining = self.check_resources()
        if digest(binary) != self.pins[str(binary)]:
            raise ValueError("pinned binary changed")
        number = len(self.commands)
        stem = self.args.workdir / "commands" / f"{number:05d}-{label}"
        stdout, stderr = Path(str(stem) + ".stdout.json"), Path(str(stem) + ".stderr.txt")
        argv = ([str(binary), *map(str, words)] if driver_task or export_environment else
                [str(binary), "--json", "--offline", "--wiki", str(vault), *map(str, words)])
        child_env = dict(self.env)
        if driver_task:
            child_env["LWIKI_SCALE_ATTRIBUTION_TASK"] = str(driver_task)
        if export_environment:
            child_env.update(export_environment)
        timed = ["/usr/bin/time", "-l" if sys.platform == "darwin" else "-v", *argv]
        record = dict(label=label, phase=phase, case=case, argv=argv, supervisor_argv=timed,
                      cwd=str(self.args.workdir), binary_sha256=self.pins[str(binary)],
                      stdout=str(stdout), stderr=str(stderr), resource_before=before,
                      evidence_path=str(stem) + ".command.json")
        if driver_task:
            record["driver_task"] = str(driver_task)
        if export_environment:
            record["export_environment"] = export_environment
        if expected_error:
            record["expected_error"] = expected_error
        self.commands.append(record)
        write_json(Path(str(stem) + ".command.json"), record)
        done = threading.Event()
        monitor = {"rss_peak_bytes": 0, "violation": None,
                   "ps_polls": 0, "ps_seconds": 0.0,
                   "timed_inventory_sweeps": 0, "timed_inventory_seconds": 0.0}
        rss_limit = (1 if phase == "context" else 8) * GIB

        def supervise(proc):
            next_disk = time.monotonic() + 5
            while not done.is_set():
                try:
                    poll_started = time.monotonic()
                    monitor["rss_peak_bytes"] = max(monitor["rss_peak_bytes"], process_tree_rss(proc.pid))
                    monitor["ps_polls"] += 1
                    monitor["ps_seconds"] += time.monotonic() - poll_started
                    if monitor["rss_peak_bytes"] > rss_limit:
                        monitor["violation"] = "process-tree RSS ceiling"
                    if shutil.disk_usage(self.args.workdir).free < (40 if getattr(self.args, "scale_attribution", False) else 32) * GIB:
                        monitor["violation"] = "free-space floor during command"
                    if stdout.stat().st_size + stderr.stat().st_size > 4 * 1024 * 1024:
                        monitor["violation"] = "command output ceiling"
                    if time.monotonic() >= next_disk:
                        sweep_started = time.monotonic()
                        resources = inventory(self.args.account_root)
                        monitor["timed_inventory_sweeps"] += 1
                        monitor["timed_inventory_seconds"] += time.monotonic() - sweep_started
                        if resources["allocated_bytes"] > getattr(self.args, "attribution_allocation_limit", self.args.max_disk_gib * GIB):
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
                                    env=child_env, start_new_session=True)
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
            except BaseException as error:
                stop(proc)
                proc.wait()
                record.update(passed=False, failure=str(error))
                raise
            finally:
                record["elapsed_seconds"] = time.monotonic() - start
                deadline.cancel()
                done.set()
                watcher.join()
                record.update(**monitor)
                write_json(Path(str(stem) + ".command.json"), record)
        record.update(returncode=code, **monitor)
        # Native maximum supplements sampling for short-lived process peaks.
        native = re.search(r"(\d+)\s+maximum resident set size", stderr.read_text())
        if sys.platform != "darwin":
            native = re.search(r"Maximum resident set size \(kbytes\):\s*(\d+)", stderr.read_text())
        record["native_peak_rss_bytes"] = int(native[1]) * (1 if sys.platform == "darwin" else 1024) if native else None
        native_text = stderr.read_text()
        for field, pattern in {
            "block_inputs": r"(\d+)\s+block input operations" if sys.platform == "darwin" else r"File system inputs:\s*(\d+)",
            "block_outputs": r"(\d+)\s+block output operations" if sys.platform == "darwin" else r"File system outputs:\s*(\d+)",
        }.items():
            match = re.search(pattern, native_text)
            record[field] = int(match[1]) if match else None
        try:
            record["resource_after"], _ = self.check_resources()
            if digest(binary) != self.pins[str(binary)]:
                raise ValueError("pinned binary changed during command")
            if record["native_peak_rss_bytes"] is None:
                raise ValueError("native peak RSS unavailable")
            if record["native_peak_rss_bytes"] > rss_limit:
                raise ValueError("native peak RSS ceiling")
            if export_environment:
                exported = Path(export_environment["LWIKI_FIXTURE_EXPORT"])
                if code or monitor["violation"] or not (exported / "corpus.json").is_file():
                    raise ValueError("fixture exporter failed or omitted manifest")
                record["passed"] = True
                return {}, record
            if driver_task:
                driver_report = Path(json.loads(driver_task.read_text())["report"])
                record["driver_report"] = str(driver_report)
                if not driver_report.is_file() or driver_report.stat().st_size > 65536:
                    raise ValueError("missing/oversized scale driver report")
                envelope = json.loads(driver_report.read_text(), object_pairs_hook=strict_object)
                record["driver_data"] = envelope.get("data")
            else:
                envelope = json.loads(stdout.read_text())
            if (expected_error and not monitor["violation"] and envelope.get("ok") is False
                    and envelope.get("error", {}).get("code") == expected_error):
                record.update(passed=False, expected_refusal=True, returncode=code,
                              meta=envelope.get("meta"))
                return envelope.get("error"), record
            if code or monitor["violation"] or envelope.get("ok") is not True:
                failure = (monitor["violation"] or envelope.get("error") or
                           (envelope.get("data") or {}).get("sync_error") or code)
                raise ValueError(f"CLI failed: {failure}")
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
    if ((vault / current_path).read_bytes() != expected
            or (vault / current_path).with_name("original.bin").read_bytes() != expected):
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
    parser.add_argument("--preseed", type=Path, help="Closed normalized fixture export (requires Python blake3)")
    parser.add_argument("--account-root", type=Path, help="Disposable directory covering seed, exports, clones and logs; required with --preseed")
    parser.add_argument("--tier", choices=TIERS, default="small", help="small=100, tiny=2; controls=1000/10000")
    parser.add_argument("--bytes-per-source", type=int, default=100 * 1024)
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--history-revisions", type=int, default=0, help="Fixed extra target revisions, via actual CLI refresh")
    parser.add_argument("--seed", type=int, default=731)
    parser.add_argument("--command-seconds", type=int, default=900)
    parser.add_argument("--run-seconds", type=int, default=3600)
    parser.add_argument("--max-disk-gib", type=int, default=8, help="Disposable allocation ceiling, maximum 100 GiB")
    parser.add_argument("--scale-attribution", action="store_true", help="Fixed A/B/C/D cost control; no quality qualification")
    parser.add_argument("--unit-test-binary", type=Path, help="Matching pinned native unit_tests for fixed attribution mode")
    parser.add_argument("--shared-reserved-bytes", type=int, help="Attribution mode: root-owned executable/build reservation deducted from the same 2 GiB new-space ceiling")
    args = parser.parse_args()
    if not 1024 <= args.bytes_per_source <= 1024 * 1024 or not 1 <= args.trials <= 20:
        parser.error("source bytes must be 1 KiB..1 MiB; trials 1..20")
    if not 0 <= args.history_revisions <= 32 or not 1 <= args.max_disk_gib <= 100:
        parser.error("history revisions must be 0..32; disk ceiling 1..100 GiB")
    if not 1 <= args.command_seconds <= 4 * 3600 or not args.command_seconds <= args.run_seconds <= 24 * 3600:
        parser.error("command/run time exceeds large-vault envelope or is inconsistent")
    if sys.platform not in ("darwin", "linux"):
        parser.error("native RSS monitoring is available only on macOS/Linux")
    if args.preseed and (not args.account_root or blake3 is None):
        parser.error("--preseed requires --account-root and the blake3 Python package")
    if args.scale_attribution:
        if not args.unit_test_binary or blake3 is None or args.preseed or args.baseline_binary:
            parser.error("attribution requires matching unit_tests/Python blake3 and fresh four-cell exports")
        if not args.account_root or args.shared_reserved_bytes != GIB or args.max_disk_gib < 2:
            parser.error("attribution requires --account-root and exactly 1 GiB compiler-growth reservation within the 2 GiB ceiling")
        args.command_seconds = min(args.command_seconds, 120)
        args.run_seconds = min(args.run_seconds, 1200)
        args.max_disk_gib = min(args.max_disk_gib, 2)
        args.attribution_allocation_limit = args.max_disk_gib*GIB-args.shared_reserved_bytes
        if args.attribution_allocation_limit <= 0:
            parser.error("shared reservation leaves no fixture allocation allowance")
    return args


def scale_attribution(args):
    """Fixed four-cell work control; the ordinary benchmark above is unchanged."""
    cells = (("A", 64, 16384), ("B", 256, 16384), ("C", 64, 1024), ("D", 64, 102400))
    driver_filter = "app::indexed_embedding_workflow_tests::scale_attribution::fixed_scale_attribution"
    exporter_filter = "app::refresh_fixture_export::export_normalized_refresh_fixture"
    args.binary = args.binary.expanduser().resolve(strict=True)
    args.unit_test_binary = args.unit_test_binary.expanduser().resolve(strict=True)
    args.workdir = args.workdir.expanduser().absolute()
    if args.workdir.exists() or args.workdir.is_symlink():
        raise ValueError("fixed attribution workdir must be new")
    args.account_root = args.account_root.expanduser().resolve(strict=True)
    if args.workdir == args.account_root or not args.workdir.is_relative_to(args.account_root):
        raise ValueError("fixed attribution workdir must be new beneath the owned account root")
    args.seed, args.history_revisions = 731, 0
    for binary in (args.binary, args.unit_test_binary):
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise ValueError("attribution executables must be explicit regular files")
        if not binary.is_relative_to(args.account_root):
            raise ValueError("attribution executables must be within the accounted owned tree")
    args.workdir.mkdir(mode=0o700)
    (args.workdir / "commands").mkdir()
    (args.workdir / "config").mkdir()
    pins = {str(binary): digest(binary) for binary in (args.binary, args.unit_test_binary)}
    runner = Runner(args, pins)
    report = dict(version=1, status="incomplete", scope="four-cell baseline cost attribution; no quality/import/25K qualification",
                  cells=[], commands=runner.commands, binary_pins=pins,
                  harness_sha256=digest(Path(__file__)), settings=dict(max_input_bytes=12000, quality_target_bytes=None),
                  dimensions=1536, limits=dict(children=50, mock_requests=160, initial_unique_inputs=2048,
                  changed_unique_inputs=64, compact_units=4096, execution_seconds=args.run_seconds,
                  per_cell_initial_admission={"A":{"inputs":256,"requests":16},"B":{"inputs":800,"requests":50},
                      "C":{"inputs":128,"requests":8},"D":{"inputs":704,"requests":44}},
                  per_cell_changed_admission=dict(inputs=16,requests=1),mock_items_per_request=16,
                  per_child_seconds=args.command_seconds, combined_new_allocated_bytes=args.max_disk_gib*GIB,
                  shared_executable_build_reserved_bytes=args.shared_reserved_bytes,
                  owned_tree_allocated_bytes=args.attribution_allocation_limit,
                  free_floor_and_reserve_bytes=40*GIB, logical_work_bytes=32*GIB),
                  counts=dict(mock_requests=0, initial_unique_inputs=0, changed_unique_inputs=0,
                              application_invocations=0, explicit_query_cache_writes=0, immutable_mock_task_read_bytes=0),
                  reserved_admission=dict(mock_requests=0,initial_unique_inputs=0,changed_unique_inputs=0),
                  logical_work=dict(measured_bytes=0, reserved_unmeasured_bytes=0),
                  predicted_unrun=[dict(owners=128, bytes_per_source=102400, minimum_proof_bytes=75*1024*1024),
                                   dict(owners=256, bytes_per_source=102400, minimum_proof_bytes=150*1024*1024)],
                  unavailable=["nested SQL publication time/rows", "physical I/O byte peaks",
                               "partial lower-layer proof work on driver outer failure"],
                  interpretation="Default segmentation distinct from 1000-byte quality fixture. Test-only mock/stream counters do not exist in shipping CLI. Diagnostic cached sync repeats separately once per cell. Filesystem hash/copy work warms pages; all errors retained. Entire owned account root includes executables/receipts/workdirs, plus fixed unmeasured 1 GiB compiler-growth reserve within the same 2 GiB ceiling.")
    write_json(args.workdir / "protocol.json", report)
    def reserve(size):
        total = report["logical_work"]["measured_bytes"] + report["logical_work"]["reserved_unmeasured_bytes"]
        if total + size > 32*GIB:
            raise ValueError("logical work reservation ceiling")
        report["logical_work"]["reserved_unmeasured_bytes"] += size
    def measured(size):
        report["logical_work"]["measured_bytes"] += size
        if sum(report["logical_work"].values()) > 32*GIB:
            raise ValueError("logical work ceiling")
    def call(binary, vault, words, phase, label, **kwargs):
        if len(runner.commands) >= 50:
            raise ValueError("fixed child ceiling")
        reserve((512 if kwargs.get("driver_task") else 256 if kwargs.get("export_environment") else 64)*1024*1024)
        measured(2*binary.stat().st_size)  # Runner hashes pre/post command, outside interval.
        try:
            return runner.run(binary, vault, words, phase, label, **kwargs)
        finally:
            write_json(args.workdir / "summary.json", report)
    def storage(root):
        buckets = {}
        for path in root.rglob("*"):
            info = path.lstat()
            if not (stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode)):
                raise ValueError("unsafe attribution storage entry")
            if not stat.S_ISREG(info.st_mode):
                continue
            if info.st_nlink != 1:
                raise ValueError("hardlinked attribution storage entry")
            relative = str(path.relative_to(root))
            bucket = ("complete_backup" if relative.startswith("backup/") else
                      "cache_loss_sibling" if relative.startswith("lost-cache/") else
                      "captured_original" if path.name == "original.bin" else
                      "captured_content" if path.name == "content.md" else
                      "vectors" if "embeddings.sqlite3" in path.name else
                      "catalog" if "/cache/catalogs/" in relative else
                      "retained_operations" if "/state/" in relative or "/retained/" in relative else
                      "canonical_markdown" if path.suffix == ".md" else "other")
            row = buckets.setdefault(bucket, dict(logical_bytes=0, allocated_bytes=0, files=0))
            row["logical_bytes"] += info.st_size
            row["allocated_bytes"] += info.st_blocks*512
            row["files"] += 1
        return dict(buckets=buckets, total=inventory(root))
    def hashes(root):
        result = {}
        for path in root.rglob("*"):
            if path.is_file():
                measured(path.stat().st_size)
                result[str(path.relative_to(root))] = blake_digest(path)
        return result
    def verify(data, vault, source, revision, expected, marker, size):
        if data.get("verification", {}).get("mode") != "indexed_evidence" or not data.get("passages"):
            raise ValueError("missing indexed evidence verification/passages")
        found = False
        for passage in data["passages"]:
            relative = passage["locator"]["path"]
            match = re.fullmatch(r"sources/([^/]+)/revisions/([^/]+)/content\.md", relative)
            if not match or passage.get("eligibility") != "current" or passage.get("label") != "captured_source":
                raise ValueError("noncurrent/noncaptured attribution passage")
            path = vault / relative
            if path.is_symlink() or not path.is_file() or path.stat().st_size != size:
                raise ValueError("unsafe attribution citation payload")
            body = path.read_bytes(); measured(len(body))
            start, end = passage["span"]["start"], passage["span"]["end"]
            if not 0 <= start < end <= len(body) or body[start:end].decode() != passage["text"]:
                raise ValueError("citation span/text mismatch")
            citations = passage.get("citations", [])
            if len(citations) != 1 or citations[0].get("kind") != "source":
                raise ValueError("missing exact source citation")
            ref = citations[0]["reference"]
            if (ref.get("source_id"), ref.get("source_revision"), ref.get("span"), ref.get("quote_hash")) != (
                    match[1], match[2], passage["span"], "blake3:"+blake3.blake3(body[start:end]).hexdigest()):
                raise ValueError("source citation binding/hash mismatch")
            if match[1] == source and marker in passage["text"]:
                if match[2] != revision or body != expected:
                    raise ValueError("target source revision/fact mismatch")
                found = True
        if not found:
            raise ValueError("cited target old/new fact absent")
    def query(vault, cell, mode, suffix, source, revision, expected, marker, size, *, missing=False):
        words = ["context", "refreshprobe000000", "--scope", "indexed-documents", "--mode", mode,
                 "--limit", "5", "--candidates", "80", "--max-bytes", "6000", "--max-tokens", "1500"]
        data, record = call(args.binary, vault, words, "context", f"{cell}-{suffix}-{mode}",
                            expected_error="OFFLINE_UNAVAILABLE" if missing else None)
        if missing:
            if not record.get("expected_refusal"):
                raise ValueError("cache-loss semantic query did not refuse")
        else:
            verify(data, vault, source, revision, expected, marker, size)
        return record
    def driver(root, vault, cell, owners, size, mode, config):
        ceilings = {"A":(256,16),"B":(800,50),"C":(128,8),"D":(704,44)}
        inputs, requests = (ceilings[cell] if mode=="initial" else (16,1) if mode=="changed" else (0,0))
        reserved = report["reserved_admission"]
        reserved["mock_requests"] += requests
        if mode=="initial": reserved["initial_unique_inputs"] += inputs
        if mode=="changed": reserved["changed_unique_inputs"] += inputs
        if reserved["mock_requests"]>160 or reserved["initial_unique_inputs"]>2048 or reserved["changed_unique_inputs"]>64:
            raise ValueError("prospective aggregate driver admission ceiling")
        task = root / f"{mode}-task.json"
        write_json(task, dict(version=1, cell=cell, mode=mode, owners=owners, bytes_per_source=size,
                             vault=str(vault), config=str(config), report=str(root/f"{mode}-driver.json"),
                             max_input_bytes=12000, quality_target_bytes=None))
        command_count = len(runner.commands)
        try:
            return call(args.unit_test_binary, vault, [driver_filter,"--ignored","--exact","--nocapture"],
                        "driver", f"{cell}-{mode}", driver_task=task)[0]
        finally:
            data = (runner.commands[-1].get("driver_data") or {}) if len(runner.commands)>command_count else {}
            if not data:
                report.setdefault("unavailable_driver_counts",[]).append(dict(cell=cell,mode=mode,
                    reserved_requests=requests,reserved_inputs=inputs,reason="outer error/panic/missing report; reservations retained"))
            counts = report["counts"]
            for field in ("mock_requests","application_invocations","explicit_query_cache_writes","immutable_mock_task_read_bytes"):
                counts[field] += data.get(field,0)
            if mode == "initial": counts["initial_unique_inputs"] += data.get("unique_inputs",0)
            if mode == "changed": counts["changed_unique_inputs"] += data.get("missing_inputs_before_sync",0)
            measured(data.get("immutable_mock_task_read_bytes",0))
            if counts["mock_requests"] > 160 or counts["initial_unique_inputs"] > 2048 or counts["changed_unique_inputs"] > 64:
                raise ValueError("aggregate attribution input/request ceiling")
    def expired(*_):
        raise TimeoutError("fixed whole-run deadline")
    signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL,args.run_seconds)
    try:
        runner.check_resources()
        for cell, owners, size in cells:
            row = dict(cell=cell,owners=owners,bytes_per_source=size,status="incomplete")
            report["cells"].append(row)
            root = args.workdir/cell; root.mkdir()
            args.source_count, args.bytes_per_source = owners, size
            args.preseed = root/"export"
            vault = root/"vault"
            try:
                call(args.unit_test_binary, vault, [exporter_filter,"--ignored","--exact","--nocapture"],
                     "setup", f"{cell}-export", export_environment={"LWIKI_FIXTURE_EXPORT":str(args.preseed),
                     "LWIKI_FIXTURE_SOURCE_COUNT":str(owners),"LWIKI_FIXTURE_BYTES_PER_SOURCE":str(size),"LWIKI_FIXTURE_SEED":"731"})
                reserve(256*1024*1024)  # Export admission/inventory payload reads.
                corpus, corpus_hash = validate_preseed(args,runner)
                row["corpus_sha256"] = corpus_hash
                row["generator"] = corpus["generator"]
                seed_allocated = inventory(args.preseed/"vault")["allocated_bytes"]
                resources, _ = runner.check_resources()
                forecast = seed_allocated*(3 if cell=="A" else 1)+64*1024*1024
                if resources["allocated_bytes"]+forecast > args.attribution_allocation_limit or resources["free_bytes"]-forecast < 40*GIB:
                    raise ValueError("closed seed/working/backup forecast admission")
                reserve(inventory(args.preseed/"vault")["logical_bytes"])
                shutil.copytree(args.preseed/"vault",vault)
                for entry in corpus["files"]:
                    measured(entry["bytes"])
                    if blake_digest(vault/entry["path"]) != entry["blake3"]:
                        raise ValueError("working clone differs from closed export")
                config = root/"mock.toml"
                config.write_text("version=1\n[profiles.primary]\nembedding='embed'\n[services.embed]\nadapter='embeddings-v1'\nurl='https://mock.invalid/v1/embeddings'\nmodel='scale-attribution-mock'\nrevision='fixed-v1'\ndimensions=1536\nmax_batch_items=16\nmax_batch_bytes=262144\n[services.embed.auth]\nkind='static'\nkey='synthetic-no-secret'\n[vault_bindings.main]\nroot="+json.dumps(str(vault))+"\nwiki_id="+json.dumps(corpus["vault_id"])+"\nallowed_profiles=['primary']\n")
                config.chmod(0o600)
                target = corpus["sources"][0]
                source, revision, marker = target["source_id"], target["revision_id"], target["marker"]
                current_path = vault/f"sources/{source}/revisions/{revision}/content.md"
                current = current_path.read_bytes(); measured(len(current))
                row["before_prepare"] = storage(root)
                row["initial_driver"] = driver(root,vault,cell,owners,size,"initial",config)
                call(args.binary,vault,["embeddings","sync"],"prepare",f"{cell}-noop")
                query(vault,cell,"semantic","initial",source,revision,current,marker,size)
                if cell=="A": query(vault,cell,"hybrid","initial",source,revision,current,marker,size)
                old_pins = {leaf:blake_digest(current_path.with_name(leaf)) for leaf in ("revision.md","original.bin","content.md")}
                reserve(2*size+65536)
                changed_marker = marker.replace("v000000","v000001")
                changed = current.replace(marker.encode(),changed_marker.encode(),1)
                if len(changed)!=len(current) or changed==current: raise ValueError("non-factual/sized refresh fixture")
                update = root/"changed.md"; update.write_bytes(changed)
                data, refresh = call(args.binary,vault,["source","refresh",source,"--file",update],"refresh",f"{cell}-refresh")
                new_revision = data["allocated_ids"]["revision"]
                if new_revision==revision or data.get("reused") is not False or data.get("status")!="committed":
                    raise ValueError("factual refresh did not commit immutable revision")
                if any(blake_digest(current_path.with_name(leaf))!=pin for leaf,pin in old_pins.items()):
                    raise ValueError("immutable old revision changed")
                reserve(2*size+65536)
                checked, _ = call(args.binary,vault,["embeddings","check"],"prepare",f"{cell}-missing-check")
                if checked["coverage"]["missing_units"] < 1: raise ValueError("changed input did not invalidate coverage")
                row["after_refresh"] = storage(root)
                row["changed_driver"] = driver(root,vault,cell,owners,size,"changed",config)
                query(vault,cell,"semantic","changed",source,new_revision,changed,changed_marker,size)
                if cell=="A": query(vault,cell,"hybrid","changed",source,new_revision,changed,changed_marker,size)
                row["exact_driver"] = driver(root,vault,cell,owners,size,"exact",config)
                row["prepared_storage"] = storage(root)
                if cell=="A":
                    backup = root/"backup"
                    before = hashes(vault)
                    reserve(inventory(vault)["logical_bytes"])
                    backup_start = time.monotonic(); shutil.copytree(vault,backup)
                    if hashes(backup)!=before: raise ValueError("complete backup differs")
                    row["backup_seconds"] = time.monotonic()-backup_start
                    row["backup_storage"] = storage(root)
                    (vault/".wiki/cache").rename(root/"lost-cache")
                call(args.binary,vault,["index","rebuild","--normalized"],"rebuild",f"{cell}-rebuild")
                row["rebuild_storage"] = storage(root)
                query(vault,cell,"semantic","rebuild",source,new_revision,changed,changed_marker,size,missing=cell=="A")
                if cell=="A":
                    query(vault,cell,"lexical","reconstruction",source,new_revision,changed,changed_marker,size)
                    checked, _ = call(args.binary,vault,["check"],"check",f"{cell}-full-check")
                    if checked.get("error_count") != 0 or checked.get("cache_matches_canonical") is not True:
                        raise ValueError("reconstructed full check mismatch")
                    for name in ("embeddings.sqlite3","embeddings.sqlite3-wal","embeddings.sqlite3-shm"):
                        original = backup/".wiki/cache"/name
                        if original.exists():
                            measured(original.stat().st_size)
                            shutil.copyfile(original,vault/".wiki/cache"/name)
                            if blake_digest(original)!=blake_digest(vault/".wiki/cache"/name):
                                raise ValueError("restored compatible vector bytes differ")
                            measured(2*original.stat().st_size)
                    call(args.binary,vault,["embeddings","sync"],"prepare",f"{cell}-restore-sync")
                    query(vault,cell,"semantic","restored",source,new_revision,changed,changed_marker,size)
                reserve(256*1024*1024)
                if validate_preseed(args,runner)[1] != corpus_hash:
                    raise ValueError("closed seed payload/manifest changed")
                row["final_storage"] = storage(root)
                row["status"] = "completed-baseline-control"
            except Exception as error:
                row.update(status="failed-or-refused",failure=str(error),dependent_phases="unrun")
                if cell=="A" and "initial_driver" not in row:
                    report["unrun_cells"] = ["B","C","D"]
                    break
            finally:
                write_json(args.workdir/"summary.json",report)
        report["status"] = ("completed-baseline-attribution" if len(report["cells"])==4 and
                            all(row["status"]=="completed-baseline-control" for row in report["cells"]) else "incomplete-with-retained-failures")
    except Exception as error:
        report.update(status="failed-or-refused",failure=str(error))
    finally:
        signal.setitimer(signal.ITIMER_REAL,0)
        report["total_seconds"] = time.monotonic()-runner.started
        write_json(args.workdir/"summary.json",report)
    print(json.dumps(dict(status=report["status"],summary=str(args.workdir/"summary.json"))))
    return 0 if report["status"]=="completed-baseline-attribution" else 1


def main():
    args = arguments()
    if args.scale_attribution:
        return scale_attribution(args)
    args.binary = args.binary.expanduser().resolve(strict=True)
    if args.baseline_binary:
        args.baseline_binary = args.baseline_binary.expanduser().resolve(strict=True)
    binaries = [("baseline", args.baseline_binary)] if args.baseline_binary else []
    binaries.append(("candidate", args.binary))
    for _, binary in binaries:
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise ValueError(f"binary is not an executable regular file: {binary}")
    args.workdir = args.workdir.expanduser().resolve()
    args.account_root = args.account_root.expanduser().resolve(strict=True) if args.account_root else args.workdir
    if not args.workdir.is_relative_to(args.account_root):
        raise ValueError("workdir must be inside the disposable account root")
    if args.preseed:
        args.preseed = args.preseed.expanduser().resolve(strict=True)
        if (not args.preseed.is_relative_to(args.account_root)
                or args.preseed.is_relative_to(args.workdir)
                or args.workdir.is_relative_to(args.preseed)):
            raise ValueError("preseed must be separate from workdir and inside account root")
    if args.workdir.exists() or args.workdir.is_symlink():
        raise ValueError("workdir must be new; existing data will not be modified")
    args.workdir.mkdir(parents=True)
    (args.workdir / "commands").mkdir()
    (args.workdir / "config").mkdir()
    pins = {str(binary): digest(binary) for _, binary in binaries}
    report = dict(protocol_version=2, status="incomplete", binary_pins=pins,
                  harness_sha256=digest(Path(__file__)), host=dict(platform=platform.platform(),
                  cpu_count=os.cpu_count()), arguments={key: str(value) if isinstance(value, Path) else value
                  for key, value in vars(args).items()}, commands=[], failures=[], results=[],
                  unavailable=["held reader: no public CLI command retains a SQL read transaction",
                  "logical filesystem/SQL counters and power/background conditions: unavailable"],
                  citation_hash_backend="python-blake3" if blake3 is not None else None,
                  blake3_version=getattr(blake3, "__version__", None),
                  interpretation="Whole CLI processes including /usr/bin/time launch, offline; setup/rebuild excluded from refresh/context timings. N sequential CLI adds can be quadratic: they seed authentic fixtures, not an import scaling qualification. Warm filesystem/index pages, not OS-cold. Supervisor samples ps/free space every ~100ms and inventory every 5s; pre/post untimed inventories warm metadata. Native RSS supplements sampling, which may overshoot. Statistics include only completed correct refresh/context pairs; all command durations/failures remain recorded and aborted tasks remain unrun. No full-scale, semantic, import-capacity, recovery, or normalized-activation qualification.")
    if blake3 is None:
        report["unavailable"].append("quote-hash recomputation: optional blake3 module unavailable; exact cited bytes/IDs/span checked, full citation correctness unqualified")
    write_json(args.workdir / "protocol.json", report)
    runner = Runner(args, pins)
    report["commands"] = runner.commands
    def whole_run_expired(_signal, _frame):
        raise TimeoutError("whole-run deadline, including setup/copy/hash work")
    signal.signal(signal.SIGALRM, whole_run_expired)
    signal.setitimer(signal.ITIMER_REAL, args.run_seconds)
    try:
        _, _ = runner.check_resources()
        count = TIERS[args.tier]
        # Conservative preallocation admission; actual physical usage is retained.
        forecast = args.bytes_per_source * (count + args.history_revisions + args.trials) * (3 + 2 * len(binaries)) + 512 * 1024 * 1024
        if forecast > args.max_disk_gib * GIB or shutil.disk_usage(args.workdir).free - forecast < 32 * GIB:
            raise ValueError("fixture/clones/history forecast exceeds disk admission")
        inputs = args.workdir / "inputs"
        inputs.mkdir()
        target = inputs / "target.md"
        if args.preseed:
            corpus, seed_manifest_hash = validate_preseed(args, runner)
            seed_vault = args.preseed / "vault"
            sources = corpus["sources"]
            source, revision = sources[0]["source_id"], sources[0]["revision_id"]
            current = (seed_vault / f"sources/{source}/revisions/{revision}/content.md").read_bytes()
            marker = sources[0]["marker"]
            shutil.copyfile(args.preseed / "corpus.json", args.workdir / "corpus.json")
            report["preseed_manifest_sha256"] = seed_manifest_hash
            report["seed_report_sha256"] = digest(args.preseed / "seed-report.json")
            allocated = inventory(seed_vault)["allocated_bytes"]
            resources, _ = runner.check_resources()
            clone_forecast = allocated * len(binaries) + 512 * 1024 * 1024
            if (resources["allocated_bytes"] + clone_forecast > args.max_disk_gib * GIB
                    or shutil.disk_usage(args.workdir).free - clone_forecast < 32 * GIB):
                raise ValueError("measured preseed plus clone forecast exceeds account admission")
        else:
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
        initial_marker = marker
        for name, binary in binaries:
            runner.check_resources()
            vault = args.workdir / f"{name} vault"
            shutil.copytree(seed_vault, vault)
            if not args.preseed:
                runner.run(binary, vault, ["index", "rebuild"], "setup", f"{name}-rebuild")
            else:
                # Verify the closed database/sidecar copy before any reader can
                # update its SHM coordination. Never open the original seed.
                for entry in corpus["files"]:
                    if blake_digest(vault / entry["path"]) != entry["blake3"]:
                        raise ValueError("clone differs from closed preseed")
            version, current_revision = args.history_revisions, revision
            current_bytes, current_marker = current, initial_marker
            expected_title = "Synthetic capture 000000"
            current_epoch = corpus["snapshot"]["generation"] if args.preseed else None
            setup_query = ["context", current_marker, "--scope", "indexed-evidence", "--no-sync", "--mode", "lexical",
                           "--source-id", source, "--limit", "3", "--candidates", "16", "--max-bytes", "12000", "--max-tokens", "3000"]
            setup_context, setup_record = runner.run(binary, vault, setup_query, "context", f"{name}-setup-context")
            verify_context(setup_context, vault, source, current_revision, current_bytes, current_marker)
            if args.preseed and setup_record["meta"].get("index_generation") != current_epoch:
                raise ValueError("setup context differs from pinned preseed epoch")
            for trial in range(args.trials):
                for case in ("noop", "title-only", "changed"):
                    if case == "changed":
                        version += 1
                        expected, marker = payload(args.seed, 0, version, args.bytes_per_source)
                    else:
                        expected, marker = current_bytes, current_marker
                    target.write_bytes(expected)
                    words = ["source", "refresh", source, "--file", target]
                    if case == "title-only":
                        expected_title = f"Synthetic display {trial}"
                        words += ["--title", expected_title]
                    prior_manifest = vault / "sources" / source / "revisions" / current_revision / "revision.md"
                    prior_hashes = {leaf: digest(prior_manifest.with_name(leaf))
                                    for leaf in ("revision.md", "original.bin", "content.md")}
                    data, refresh = runner.run(binary, vault, words, "refresh", f"{name}-{trial}-{case}", case)
                    observed_revision = data["allocated_ids"]["revision"]
                    try:
                        if (case == "changed") == (observed_revision == current_revision):
                            raise ValueError("refresh did not preserve/create revision as expected")
                        if any(digest(prior_manifest.with_name(leaf)) != old_hash for leaf, old_hash in prior_hashes.items()):
                            raise ValueError("refresh rewrote retained immutable revision bytes")
                        if data.get("reused") is not (case != "changed"):
                            raise ValueError("refresh reuse flag differs from case")
                        if case == "noop":
                            if any(data.get(key) is not None for key in ("change", "status", "snapshot")):
                                raise ValueError("no-op created a change/publication")
                        elif data.get("status") != "committed" or not data.get("change"):
                            raise ValueError("changed source metadata/content was not committed")
                        if args.preseed and case != "noop":
                            resulting = data.get("snapshot", {})
                            if (resulting.get("generation") != current_epoch + 1
                                    or resulting.get("publication", {}).get("file_id") != corpus["snapshot"]["publication"]["file_id"]):
                                raise ValueError("refresh did not publish the exact next selected epoch")
                            current_epoch += 1
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
                    current_bytes, current_marker = expected, marker
                    query = ["context", marker, "--scope", "indexed-evidence", "--no-sync", "--mode", "lexical",
                             "--source-id", source, "--limit", "3", "--candidates", "16",
                             "--max-bytes", "12000", "--max-tokens", "3000"]
                    context, observed = runner.run(binary, vault, query, "context", f"{name}-{trial}-{case}-context", case)
                    try:
                        verify_context(context, vault, source, current_revision, expected, marker)
                        if args.preseed and (observed["meta"].get("index_generation") != current_epoch
                                or context["verification"].get("discovery_generation") != current_epoch):
                            raise ValueError("immediate context returned another epoch")
                    except Exception as error:
                        observed.update(passed=False, failure=str(error))
                        write_json(Path(observed["evidence_path"]), observed)
                        raise
                    observed["verification"] = context["verification"]
                    write_json(Path(observed["evidence_path"]), observed)
                    report["results"].append(dict(binary=name, trial=trial, case=case,
                        refresh_seconds=refresh["elapsed_seconds"], context_seconds=observed["elapsed_seconds"],
                        source_id=source, revision_id=current_revision, input_sha256=hashlib.sha256(expected).hexdigest(),
                        marker=marker, current_epoch=current_epoch,
                        retained_revision_count=version + 1,
                        timed_inventory_sweeps=refresh["timed_inventory_sweeps"] + observed["timed_inventory_sweeps"],
                        passed=True))
                    write_json(args.workdir / "summary.json", report)
        if args.preseed:
            _, final_manifest_hash = validate_preseed(args, runner)
            if final_manifest_hash != seed_manifest_hash:
                raise ValueError("original closed seed changed during experiment")
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
        report["disk_after"] = inventory(args.account_root)
        report["small_sample_note"] = "Nearest-rank p95 equals max for five trials; no tail-confidence claim."
        report["initial_history"] = args.history_revisions
        report["target_checks"] = {}
        for name, statistics in report["statistics"].items():
            def below(case, phase, metric, ceiling):
                value = statistics[case][phase]
                return value[metric] <= ceiling if value else None
            report["target_checks"][name] = {
                "prospective_noop_p95_250ms": below("noop", "refresh", "p95_seconds", .250),
                "prospective_title_p95_1s": below("title-only", "refresh", "p95_seconds", 1),
                "prospective_changed_p95_1s": below("changed", "refresh", "p95_seconds", 1),
                "frozen_noop_every_5s": below("noop", "refresh", "max_seconds", 5),
                "frozen_changed_every_60s": below("changed", "refresh", "max_seconds", 60),
                "prospective_refresh_every_5s": all(below(case, "refresh", "max_seconds", 5) is True for case in statistics),
                "frozen_context_p95_5s": all(below(case, "context", "p95_seconds", 5) is True for case in statistics),
            }
        signal.setitimer(signal.ITIMER_REAL, 0)
        write_json(args.workdir / "summary.json", report)
    print(json.dumps(dict(status=report["status"], summary=str(args.workdir / "summary.json"))))
    return 0 if report["status"] == "passed-development-diagnostic" else 1


if __name__ == "__main__":
    sys.exit(main())
