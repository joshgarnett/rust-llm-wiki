#!/usr/bin/env python3
"""Supervise one offline normalized-fixture export; preserve all failure evidence.

The unit-test executable and its expected SHA-256 must be supplied explicitly.
The new workdir receives fixture/, stdout.log, stderr.log and report.json.
This script does not build binaries or invoke providers.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import threading
import time

GIB = 1024 ** 3
OUTPUT_LIMIT = 4 * 1024 * 1024
TEST = "app::refresh_fixture_export::export_normalized_refresh_fixture"
CLEAN_ENV = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8"}


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def inventory(root):
    """Count physical allocation, including directories; never follow links."""
    result = {"allocated_bytes": root.lstat().st_blocks * 512, "logical_bytes": 0, "files": 0,
              "vanished_during_sample": 0}
    def unreadable(error):
        raise error

    for base, dirs, files in os.walk(root, followlinks=False, onerror=unreadable):
        for name in dirs + files:
            path = Path(base) / name
            try:
                info = path.lstat()
            except FileNotFoundError:
                # The exporter atomically renames temporary files while running.
                result["vanished_during_sample"] += 1
                continue
            regular = stat.S_ISREG(info.st_mode)
            if not (regular or stat.S_ISDIR(info.st_mode)):
                raise ValueError(f"unsafe entry in disposable workdir: {path}")
            if regular and info.st_nlink != 1:
                raise ValueError(f"hardlinked fixture entry: {path}")
            result["allocated_bytes"] += info.st_blocks * 512
            if regular:
                result["logical_bytes"] += info.st_size
                result["files"] += 1
    result["free_bytes"] = shutil.disk_usage(root).free
    return result


def process_tree_rss(pid):
    sample = subprocess.run(["/bin/ps", "-axo", "pid=,ppid=,rss="],
                            capture_output=True, text=True, check=True,
                            timeout=3, env=CLEAN_ENV)
    rows = [tuple(map(int, line.split())) for line in sample.stdout.splitlines()
            if line.strip()]
    family = {pid}
    while True:
        expanded = family | {child for child, parent, _ in rows if parent in family}
        if expanded == family:
            return sum(rss * 1024 for child, _, rss in rows if child in family)
        family = expanded


def stop(proc):
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def absolute_plain(path, *, existing):
    if not path.is_absolute():
        raise ValueError(f"absolute path required: {path}")
    # Reject symlink components, including an existing final output directory.
    at = Path(path.anchor)
    for part in path.parts[1:]:
        if part in (".", ".."):
            raise ValueError("dot path components are not allowed")
        at = at / part
        try:
            if stat.S_ISLNK(at.lstat().st_mode):
                raise ValueError(f"symlink path component: {at}")
        except FileNotFoundError:
            if existing or at != path:
                raise ValueError(f"missing path component: {at}")
    return path


def write_report(path, report):
    temporary = path.with_suffix(".json.tmp")
    with temporary.open("w", encoding="utf-8") as stream:
        json.dump(report, stream, indent=2, sort_keys=True)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)


def resource_failure(resources, disk_limit):
    if resources["allocated_bytes"] > disk_limit:
        return "physical workdir allocation exceeded ceiling"
    if resources["free_bytes"] < 32 * GIB:
        return "free space fell below 32 GiB"
    return None


def supervise(args, report):
    workdir = args.workdir
    report_path = workdir / "report.json"
    proc = None
    done = threading.Event()
    failure = []
    failure_lock = threading.Lock()
    monitor = {"sampled_tree_peak_rss_bytes": 0, "rss_samples": 0,
               "peak_sampled_allocated_bytes": 0, "minimum_sampled_free_bytes": None}
    started = time.monotonic()

    def fail(message):
        with failure_lock:
            if not failure:
                failure.append(message)
        if proc is not None:
            stop(proc)

    def sample():
        next_disk = 0.0
        while not done.is_set():
            try:
                rss = process_tree_rss(proc.pid)
                monitor["rss_samples"] += 1
                monitor["sampled_tree_peak_rss_bytes"] = max(
                    monitor["sampled_tree_peak_rss_bytes"], rss)
                if rss > 8 * GIB:
                    fail("sampled process-tree RSS exceeded 8 GiB")
                free = shutil.disk_usage(workdir).free
                previous = monitor["minimum_sampled_free_bytes"]
                monitor["minimum_sampled_free_bytes"] = min(previous, free) if previous is not None else free
                if free < 32 * GIB:
                    fail("free space fell below 32 GiB")
                if time.monotonic() >= next_disk:
                    resources = inventory(workdir)
                    monitor["last_resource_sample"] = resources
                    monitor["peak_sampled_allocated_bytes"] = max(
                        monitor["peak_sampled_allocated_bytes"], resources["allocated_bytes"])
                    problem = resource_failure(resources, args.max_disk_gib * GIB)
                    if problem:
                        fail(problem)
                    next_disk = time.monotonic() + 2
            except Exception as error:
                fail(f"resource monitor failed: {error}")
            if failure:
                return
            done.wait(.2)

    watcher = None
    deadline = None
    try:
        binary = absolute_plain(args.unit_test_binary, existing=True)
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise ValueError("unit-test binary must be an executable regular file")
        actual = digest(binary)
        report["binary_sha256_observed_before"] = actual
        if actual != args.binary_sha256:
            raise ValueError("unit-test binary SHA-256 differs from supplied pin")
        if sys.platform not in ("darwin", "linux"):
            raise ValueError("native time/RSS units supported only on macOS and Linux")
        before = inventory(workdir)
        report["resource_before"] = before
        problem = resource_failure(before, args.max_disk_gib * GIB)
        if problem:
            raise ValueError(problem)
        env = dict(CLEAN_ENV, LWIKI_FIXTURE_EXPORT=str(workdir / "fixture"),
                   LWIKI_FIXTURE_SOURCE_COUNT=str(args.source_count),
                   LWIKI_FIXTURE_SEED=str(args.seed),
                   LWIKI_FIXTURE_BYTES_PER_SOURCE=str(args.bytes_per_source))
        argv = [str(binary), TEST, "--ignored", "--exact", "--nocapture"]
        timed = ["/usr/bin/time", "-l" if sys.platform == "darwin" else "-v", *argv]
        report.update(argv=argv, supervisor_argv=timed, child_environment=env,
                      cwd=str(workdir), status="running")
        write_report(report_path, report)
        with (workdir / "stdout.log").open("xb") as out, (workdir / "stderr.log").open("xb") as err:
            proc = subprocess.Popen(timed, cwd=workdir, env=env, stdin=subprocess.DEVNULL,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    start_new_session=True)
            deadline = threading.Timer(args.export_seconds, lambda: fail("export elapsed ceiling"))
            deadline.daemon = True
            deadline.start()
            watcher = threading.Thread(target=sample, daemon=True)
            watcher.start()
            written = 0
            failure_started = None
            with selectors.DefaultSelector() as selector:
                for pipe, output in ((proc.stdout, out), (proc.stderr, err)):
                    os.set_blocking(pipe.fileno(), False)
                    selector.register(pipe, selectors.EVENT_READ, output)
                while selector.get_map() or proc.poll() is None:
                    if time.monotonic() - started > args.export_seconds:
                        fail("export elapsed ceiling")
                    for key, _ in selector.select(.1):
                        block = os.read(key.fileobj.fileno(), 65536)
                        if not block:
                            selector.unregister(key.fileobj)
                            key.fileobj.close()
                            continue
                        available = OUTPUT_LIMIT - written
                        kept = block[:available]
                        key.data.write(kept)
                        written += len(kept)
                        if len(kept) != len(block):
                            fail("combined child output exceeded 4 MiB")
                    if failure and proc.poll() is not None:
                        # SIGKILL also closes descendant writers; bounded select
                        # drains their remaining buffered output on the next loop.
                        stop(proc)
                    if failure:
                        failure_started = failure_started or time.monotonic()
                        if time.monotonic() - failure_started > 3:
                            # Do not hang on an inherited pipe from a detached
                            # child after the entire admitted group was killed.
                            for key in list(selector.get_map().values()):
                                selector.unregister(key.fileobj)
                                key.fileobj.close()
                            break
            report["returncode"] = proc.wait(timeout=5)
            report["retained_child_output_bytes"] = written
        done.set()
        watcher.join(timeout=5)
        if watcher.is_alive():
            fail("resource monitor did not finish within 5 seconds")
        report.update(monitor)
        stderr = (workdir / "stderr.log").read_text(errors="replace")
        pattern = (r"(\d+)\s+maximum resident set size" if sys.platform == "darwin"
                   else r"Maximum resident set size \(kbytes\):\s*(\d+)")
        native = re.search(pattern, stderr)
        peak = int(native[1]) * (1 if sys.platform == "darwin" else 1024) if native else None
        report["native_peak_rss_bytes"] = peak
        after = inventory(workdir)
        report["resource_after"] = after
        report["binary_sha256_observed_after"] = digest(binary)
        report["script_sha256_after"] = digest(Path(__file__).resolve())
        for problem in [resource_failure(after, args.max_disk_gib * GIB),
                        "export process failed" if report["returncode"] else None,
                        "native peak RSS unavailable" if peak is None else None,
                        "native peak RSS exceeded 8 GiB" if peak is not None and peak > 8 * GIB else None,
                        "pinned binary changed during export" if report["binary_sha256_observed_after"] != args.binary_sha256 else None,
                        "supervisor script changed during export" if report["script_sha256_after"] != report["script_sha256"] else None,
                        "fixture directory missing" if not (workdir / "fixture").is_dir() else None]:
            if problem:
                fail(problem)
        # The exact ignored test must actually execute, not silently match zero tests.
        stdout = (workdir / "stdout.log").read_text(errors="replace")
        if not re.search(r"test result: ok\. 1 passed; 0 failed; 0 ignored;", stdout):
            fail("unit-test harness did not report exactly one passing exporter test")
        for name in ("corpus.json", "seed-report.json"):
            artifact = workdir / "fixture" / name
            if not artifact.is_file():
                fail(f"required exporter artifact missing: {name}")
            else:
                report.setdefault("fixture_artifacts", {})[name] = {
                    "path": str(artifact), "sha256": digest(artifact)}
        if failure:
            raise ValueError(failure[0])
        report.update(status="passed", passed=True)
    except BaseException as error:
        if proc is not None:
            stop(proc)
            try:
                report["returncode"] = proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                report["returncode"] = None
        report.update(status="failed", passed=False, failure=str(error) or type(error).__name__)
    finally:
        if deadline is not None:
            deadline.cancel()
        done.set()
        if watcher is not None:
            watcher.join(timeout=5)
        report.update(monitor)
        report["elapsed_seconds"] = time.monotonic() - started
        try:
            report["resource_final"] = inventory(workdir)
            problem = resource_failure(report["resource_final"], args.max_disk_gib * GIB)
            if problem or report["elapsed_seconds"] > args.export_seconds:
                report.update(status="failed", passed=False,
                              failure=report.get("failure") or problem or "export elapsed ceiling")
        except Exception as error:
            report["final_inventory_error"] = str(error)
            report.update(status="failed", passed=False)
        write_report(report_path, report)
    return 0 if report["passed"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--unit-test-binary", type=Path, required=True)
    parser.add_argument("--binary-sha256", required=True)
    parser.add_argument("--workdir", type=Path, required=True)
    parser.add_argument("--source-count", type=int, required=True)
    parser.add_argument("--bytes-per-source", type=int, default=100000)
    parser.add_argument("--seed", type=int, default=731)
    parser.add_argument("--export-seconds", type=int, default=1800)
    parser.add_argument("--max-disk-gib", type=int, default=8)
    args = parser.parse_args()
    for valid, message in [
        (1 <= args.source_count <= 10000, "source-count must be 1..10000"),
        (1024 <= args.bytes_per_source <= 1048576, "bytes-per-source must be 1024..1048576"),
        (0 <= args.seed <= 2 ** 64 - 1, "seed must fit u64"),
        (1 <= args.export_seconds <= 14400, "export-seconds must be 1..14400"),
        (1 <= args.max_disk_gib <= 100, "max-disk-gib must be 1..100"),
        (re.fullmatch(r"[0-9a-f]{64}", args.binary_sha256) is not None,
         "binary-sha256 must be 64 lowercase hex characters")]:
        if not valid:
            parser.error(message)
    try:
        absolute_plain(args.workdir, existing=False)
        args.workdir.mkdir(mode=0o700)  # Existing paths are never reused or removed.
    except Exception as error:
        parser.error(str(error))
    report = dict(version=1, status="initializing", passed=False, returncode=None,
                  native_peak_rss_bytes=None,
                  script_sha256=digest(Path(__file__).resolve()),
                  binary=str(args.unit_test_binary), binary_sha256_expected=args.binary_sha256,
                  test=TEST, fixture=str(args.workdir / "fixture"),
                  source_count=args.source_count, seed=args.seed,
                  bytes_per_source=args.bytes_per_source,
                  limits=dict(export_seconds=args.export_seconds, rss_bytes=8 * GIB,
                              free_floor_bytes=32 * GIB, allocated_bytes=args.max_disk_gib * GIB,
                              child_output_bytes=OUTPUT_LIMIT),
                  accounting="monotonic supervisor elapsed; native single-process maximum plus sampled process-tree RSS; sampled physical allocation; no live-provider qualification")
    write_report(args.workdir / "report.json", report)
    def interrupted(signum, _frame):
        raise InterruptedError(f"supervisor interrupted by signal {signum}")

    previous = signal.signal(signal.SIGTERM, interrupted)
    try:
        code = supervise(args, report)
    finally:
        signal.signal(signal.SIGTERM, previous)
    print(json.dumps({"passed": report["passed"], "report": str(args.workdir / "report.json")}))
    return code


if __name__ == "__main__":
    sys.exit(main())
