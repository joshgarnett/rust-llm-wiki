#!/usr/bin/env python3
"""Bounded source-only preparation. Never executes lwiki or acquires a corpus."""
import argparse
import codecs
from collections import Counter
from functools import lru_cache
import hashlib
import json
import os
from pathlib import Path
import platform
import resource
import shutil
import stat
import sys
import time

PIN = "398229c96b305cc166f84cd80fb379719fbfbbfd1036bb4acafddfb114404679"
COMMIT = "66221abdeca2002d318fde6efff516aab091df0e"
SEED = "lwiki-25k-functional-v1"
COUNTS = {"smoke": 10, "1000": 1000, "10000": 10000, "25000": 25000}
GIB = 1024 ** 3
INPUT_CAP = 256 * 1024 ** 2
FREE_FLOOR = 32 * GIB
UNAVAILABLE = "UNAVAILABLE"


def require(condition, message):
    if not condition:
        raise ValueError(message)


class PreparationGuard:
    """One cooperative interval for this childless input-preparation process."""
    def __init__(self):
        self.started = time.monotonic()
        self.output = self.account = None
        self.last_sample = self.started
        self.observed = {"whole_elapsed_seconds": 0, "self_native_peak_rss_bytes": 0,
                         "account_peak_allocated_bytes_observed": 0, "input_peak_allocated_bytes_observed": 0,
                         "minimum_host_free_bytes_observed": None, "allocation_sample_count": 0,
                         "allocation_max_sample_gap_seconds": 0, "scope": "single process; no children",
                         "limits": {"seconds": 1800, "self_rss_bytes": GIB, "input_allocated_bytes": INPUT_CAP,
                                    "account_allocated_bytes": 100 * GIB, "host_free_floor_bytes": FREE_FLOOR},
                         "enforcement": "cooperative per chunk/file; filesystem and RSS sampling can overshoot"}
    def quick(self):
        self.observed["whole_elapsed_seconds"] = time.monotonic() - self.started
        raw = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        unit = "bytes" if platform.system() == "Darwin" else "KiB"
        self.observed["self_native_peak_rss_raw"] = raw
        self.observed["self_native_peak_rss_raw_unit"] = unit
        peak = raw if unit == "bytes" else raw * 1024
        self.observed["self_native_peak_rss_bytes"] = peak
        require(peak <= GIB, "finite preparation self RSS limit exceeded")
        require(self.observed["whole_elapsed_seconds"] <= 1800, "finite preparation deadline exceeded")

    def check(self, force=False):
        self.quick()
        if self.account is None or (not force and time.monotonic() - self.last_sample < 1):
            return
        free = shutil.disk_usage(self.account).free
        owned, inputs = allocation(self.account, self.quick), allocation(self.output, self.quick)
        now = time.monotonic()
        self.observed["allocation_max_sample_gap_seconds"] = max(self.observed["allocation_max_sample_gap_seconds"], now - self.last_sample)
        self.last_sample = now
        self.observed["allocation_sample_count"] += 1
        old_free = self.observed["minimum_host_free_bytes_observed"]
        self.observed["minimum_host_free_bytes_observed"] = free if old_free is None else min(old_free, free)
        self.observed["account_peak_allocated_bytes_observed"] = max(owned, self.observed["account_peak_allocated_bytes_observed"])
        self.observed["input_peak_allocated_bytes_observed"] = max(inputs, self.observed["input_peak_allocated_bytes_observed"])
        require(free >= FREE_FLOOR, "host free floor exceeded")
        require(inputs <= INPUT_CAP, "finite input allocated-byte limit exceeded")
        require(owned <= 100 * GIB, "100 GiB physical account limit exceeded")
        self.quick()


def digest(path, guard=None):
    h = hashlib.sha256()
    with open(path, "rb") as stream:
        for data in iter(lambda: stream.read(65536), b""):
            if guard:
                guard.check()
            h.update(data)
    if guard:
        guard.check()
    return h.hexdigest()


def regular(path):
    path = Path(path)
    require(path.is_absolute(), "input paths must be absolute")
    require(not path.is_symlink() and stat.S_ISREG(path.stat().st_mode),
            "input must be a regular non-symlink file")
    return path


def write_json(path, value):
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2, sort_keys=True)
        stream.write("\n")


def load_json(path, guard=None):
    if guard:
        guard.check()
    require(Path(path).stat().st_size <= 1024 ** 2, "metadata exceeds 1 MiB")
    result = json.loads(Path(path).read_text(encoding="utf-8"))
    if guard:
        guard.check()
    return result


def overlay(path):
    if path is None:
        return UNAVAILABLE
    manifest = load_json(regular(path))
    require(set(manifest) == {"schema", "sources"} and manifest["schema"] == 1,
            "overlay schema requires only schema and source-only sources")
    require(len(manifest["sources"]) == 2, "exactly two public sources required")
    entries = []
    fields = {"path", "title", "sha256", "bytes", "origin", "commit", "license", "notices"}
    for item in manifest["sources"]:
        require(set(item) == fields, "unknown or missing overlay fields (no query/gold inputs)")
        source = regular(item["path"])
        require(item["commit"] == COMMIT and item["license"] == "MIT OR Apache-2.0",
                "overlay must use the frozen Cargo 0.85 commit/license declaration")
        expected = f"https://github.com/rust-lang/cargo/blob/{COMMIT}/src/doc/src/reference/{source.name}"
        raw = f"https://raw.githubusercontent.com/rust-lang/cargo/{COMMIT}/src/doc/src/reference/{source.name}"
        require(source.name in {"config.md", "features.md"} and item["origin"] in {expected, raw},
                "overlay origin/path is outside the frozen source-only Cargo selection")
        require(source.stat().st_size == item["bytes"] <= 1024 ** 2,
                "public source length mismatch or acquisition bound exceeded")
        require(digest(source) == item["sha256"], "public source SHA256 mismatch")
        source.read_bytes().decode("utf-8", errors="strict")
        require(item["notices"], "license/notice references required")
        notices = []
        for value in item["notices"]:
            notice = regular(value)
            require(notice.stat().st_size <= 1024 ** 2, "notice exceeds 1 MiB")
            notices.append({"path": str(notice), "bytes": notice.stat().st_size,
                            "sha256": digest(notice)})
        require({Path(n["path"]).name for n in notices} >= {"README.md", "LICENSE-MIT", "LICENSE-APACHE"},
                "retain Cargo README and both licenses")
        entries.append(dict(item, notice_inventory=notices))
    require({Path(e["path"]).name for e in entries} == {"config.md", "features.md"},
            "public sources must be distinct config/features")
    return sorted(entries, key=lambda e: Path(e["path"]).name)


def sizes(count, public_bytes):
    bases = [80000] * (count // 5 - 1) + [100000] * (count * 3 // 5)
    bases += [120000] * (count - 2 - len(bases))
    q, r = divmod(count * 100000 - public_bytes - sum(bases), count - 2)
    targets = [base + q + (i < r) for i, base in enumerate(bases)]
    require(min(targets) >= 2000, "overlay too large for honest size policy")
    return bases, targets, q, r


def allocation(root, check=None):
    """Charge each inode once; block totals are observations, not CoW forecasts."""
    seen, total = set(), 0
    for parent, dirs, files in os.walk(root, followlinks=False):
        for path in [Path(parent)] + [Path(parent) / name for name in dirs + files]:
            if check:
                check()
            st = path.lstat()
            require(not stat.S_ISLNK(st.st_mode), "account root contains unaccountable symlink")
            require(stat.S_ISDIR(st.st_mode) or stat.S_ISREG(st.st_mode), "account contains special file")
            key = (st.st_dev, st.st_ino)
            if key not in seen:
                total += st.st_blocks * 512
                seen.add(key)
    return total


def binary_pin(binary, sha, guard=None):
    require(sha == PIN, "binary SHA256 is not the accepted native pin")
    require(digest(regular(binary), guard) == sha, "binary bytes differ from accepted pin")


def commands(binary, output, tier):
    vault = str(output / "vault with spaces")
    prefix = [binary, "--wiki", vault, "--offline", "--json"]
    search_limits = ["--mode", "lexical", "--candidates", "80", "--excerpt-bytes", "1024", "--limit", "5", "--verify-selected", "--no-sync"]
    key = f"{SEED}-{tier}"
    tasks = {
        "init": [binary, "--offline", "--json", "init", vault],
        "normalized_rebuild": prefix + ["index", "rebuild", "--normalized"],
        "prepare": [binary, "--offline", "--json", "source", "import", "prepare", "--input-list", str(output / "inputs.jsonl"), "--output", str(output / "import.jsonl")],
        "run": prefix + ["source", "import", "run", "--manifest", str(output / "import.jsonl"), "--key", key, "--group-size", "4", "--max-groups", "64"],
        "status": prefix + ["source", "import", "status", "--key", key],
        "resume": prefix + ["source", "import", "resume", "--key", key, "--max-groups", "64"],
        "search": prefix + ["search", "${QUERY}"] + search_limits,
        "context": prefix + ["context", "${QUERY}", "--scope", "indexed-documents", "--target", "documents", "--mode", "lexical", "--candidates", "80", "--excerpt-bytes", "1024", "--limit", "10", "--no-sync", "--max-bytes", "6000", "--max-tokens", "1500", "--verification-max-bytes", "67108864", "--verification-max-files", "4096", "--verification-max-entries", "16384", "--verification-max-elapsed-ms", "2000", "--instruction-bytes", "0", "--instruction-tokens", "0", "--output-bytes", "0", "--output-tokens", "0"],
        "read": prefix + ["read", "--path", "${RETURNED_PAYLOAD_PATH}", "--start", "${START}", "--end", "${END}", "--max-bytes", "4096"],
        "refresh": prefix + ["--stage", "source", "refresh", "${SOURCE_ID}", "--file", "${NOVEL_100K_UTF8_FILE}"],
        "refresh_show": prefix + ["changes", "show", "${REFRESH_CHANGE_ID}"],
        "refresh_apply": prefix + ["changes", "apply", "${REFRESH_CHANGE_ID}"],
        "fresh_add": prefix + ["source", "add", "${FRESH_100K_UTF8_FILE}", "--title", "Fresh functional record"],
        "noop": prefix + ["source", "refresh", "${SOURCE_ID}", "--file", "${NOVEL_100K_UTF8_FILE}"],
        "reactivate_history": prefix + ["source", "refresh", "${SOURCE_ID}", "--file", "${RETAINED_OLD_UTF8_FILE}"],
        "history": prefix + ["search", "${QUERY}", "--source-id", "${SOURCE_ID}", "--include-historical"] + search_limits,
        "historical_read": prefix + ["read", "--path", "${HISTORICAL_PAYLOAD_PATH}", "--max-bytes", "8192"],
        "withdraw": prefix + ["source", "withdraw", "${SOURCE_ID}", "--reason", "Frozen functional withdrawal"],
        "page_put": prefix + ["page", "put", "--file", "${REVIEWED_CITED_PAGE}", "--if-match", "${AUTHOR_HASH}"],
        "page_create": prefix + ["page", "put", "--file", "${REVIEWED_CITED_PAGE}"],
        "page_read": prefix + ["read", "--id", "${PAGE_ID}"],
        "page_search": prefix + ["search", "${PAGE_QUERY}"] + search_limits,
        "change_show": prefix + ["changes", "show", "${PENDING_CHANGE}"],
        "recover": prefix + ["recover"],
        "check": prefix + ["check"],
    }
    return {"schema": 1, "launch_admitted": False, "cwd": "external working directory outside vault",
            "argv": tasks, "bindings": {
                "source_ids": "Join inputs ordinals to returned results_path JSONL group_committed event.result.items Source/Revision mappings; assert no missing/duplicate ordinal before completion.",
                "read": "Bind path, UTF-8 start/end from actual verified search; retain exact read citation and current/historical eligibility.",
                "refresh": "Separately create/hash a genuine approximately 100KiB source that differs from every retained revision. Stage refresh, bind returned Change ID for show, independently compare staged bytes, then apply the reviewed Change. Repeat same bytes for separate no-op; history reactivation uses actual retained bytes; history read binds old returned payload path.",
                "proof": "Search selected proof is fixed by native CLI at 67108864 bytes/4096 files/16384 entries/2000ms; exact read retains native selected-proof limits. Context explicitly freezes supported proof and lexical candidate limits. These are ceilings, not performance acceptance claims.",
                "page": "Create reviewed canonical Page envelope citing both public Sources using actual citations; first creation omits if-match, replacement uses actual current author hash; preserve notes and test stale-hash refusal.",
                "recovery": "Only an independently frozen interrupted disposable attempt permits recover; inspect pending Change first, then same-key resume. Preserve authority/history and compare counts/identities.",
                "symbols": "${...} are runtime bindings, not existing IDs or shell expansion instructions. Bind each argv element without a shell; this file cannot launch commands."}}


def plan(args):
    binary_pin(args.binary, args.binary_sha256)
    account = Path(args.account_root).resolve(strict=True)
    require(account.is_dir(), "account root must exist")
    output = Path(args.output).absolute()
    require(output.parent.resolve() == output.parent and output.is_relative_to(account) and output != account,
            "output must be a new owned subdirectory within account root, with no alias")
    require(output.parent.is_dir() and not output.exists() and not output.is_symlink(), "output must be absent with existing parent")
    public = overlay(args.overlay_manifest)
    count = COUNTS[args.tier]
    layout = UNAVAILABLE if public == UNAVAILABLE else sizes(count, sum(e["bytes"] for e in public))
    existing = allocation(account)
    free = shutil.disk_usage(account).free
    missing = ["genuine_varied_1K_100MB_control", "genuine_varied_10K_1GB_control",
               "measured_physical_resource_projection_including_CoW_and_recovery_reserve",
               "measured_RSS_and_whole_command_time_projection", "frozen_independent_acceptance_protocol",
               "host_power_background_and_compiler_action_attestation"]
    if public == UNAVAILABLE:
        missing.append("verified_Cargo_source_only_overlay_and_notice_inventory")
    ledger = {"existing_account_allocated_bytes_observed": existing,
              "host_free_bytes_observed": free, "input_logical_bytes": count * 100000,
              "canonical_originals_logical_bytes": count * 100000,
              "canonical_content_logical_bytes": count * 100000}
    for name in ["archives_notices_physical", "headers_physical", "history_physical", "Change_retention_physical", "journals_manifests_physical", "Pages_graph_physical", "indexes_WAL_SHM_physical", "generation_overlap_physical", "scratch_physical", "backups_copied_seeds_physical", "reports_physical", "projected_peak_physical"]:
        ledger[name] = UNAVAILABLE
    packet = {"schema": 1, "generator_sha256": digest(Path(__file__).resolve()), "seed": SEED,
              "binary": {"path": args.binary, "sha256": PIN, "source": "a06", "opt_level": 3, "debug": 0,
                         "settings_evidence": "root-supplied accepted native binary record; not inferred from filename"},
              "account_root": str(account), "output": str(output), "tier": args.tier,
              "count": count, "content_bytes": count * 100000, "overlay": public,
              "size_policy": {"bases": [80000, 100000, 120000], "base_counts": [count // 5 - 1, count * 3 // 5, count // 5 - 1],
                              "adjustment_q": UNAVAILABLE if layout == UNAVAILABLE else layout[2],
                              "adjustment_r": UNAVAILABLE if layout == UNAVAILABLE else layout[3]},
              "ledger": ledger, "cli_admission": {"admitted": False, "missing": missing},
              "input_generation": {"requires_explicit_admission_flag": True, "maximum_count": 1000,
                                   "allocated_byte_limit": INPUT_CAP, "free_floor_bytes": FREE_FLOOR,
                                   "rss_limit_bytes": GIB, "deadline_seconds": 1800,
                                   "large_tier_missing": missing},
              "envelope": {"physical_account_bytes": 100 * GIB, "free_floor_bytes": FREE_FLOOR,
                           "whole_seconds": 86400, "command_seconds": 14400, "bulk_tree_rss_bytes": 8 * GIB, "query_tree_rss_bytes": GIB},
              "acceptance": {"independent": "PENDING", "capacity": "OPEN", "functional_25K": "SEPARATE_OPEN",
                             "bulk_1000_genuine_changes_600s": "MANDATORY_FAILED_GATE_UNRESOLVED", "quality_semantic_release": "OPEN"}}
    output.mkdir()
    write_json(output / "packet.json", packet)
    write_json(output / "command-plan.json", commands(args.binary, output, args.tier))
    return {"planned": True, "cli_launch_admitted": False, "missing": missing}


@lru_cache(maxsize=1)
def closing_choices():
    clauses = ["before departure", "before the departure", "before the next departure",
               "before the planned departure", "before the planned morning departure",
               "before the scheduled morning departure"]
    options = [f"{verb} {clause}" for verb in ["reviewed", "inspected"] for clause in clauses]
    deltas = [len(option) - len(options[0]) for option in options]
    paths = {0: ()}
    for _ in range(20):
        paths = {total + delta: choices + (index,) for total, choices in paths.items()
                 for index, delta in enumerate(deltas)}
    return options, paths


def closing_record(ordinal, record, h, extra=0):
    options, paths = closing_choices()
    require(extra in paths, "exact closing prose length is not representable")
    lead = f"Distant dispatch: station {ordinal + 17} confirmed route {h[20:28]} after review {record}.\n"
    lines = [lead]
    for index, choice in enumerate(paths[extra]):
        code = hashlib.sha256(f"{h}:closing:{index}".encode()).hexdigest()[:16]
        lines.append(f"Closing review {index:02d}: route {code} was {options[choice]}.\n")
    return "".join(lines).encode()


def closing_min_bytes(ordinal, record, h):
    lead = f"Distant dispatch: station {ordinal + 17} confirmed route {h[20:28]} after review {record}.\n"
    return len(lead.encode()) + 20 * len("Closing review 00: route 0000000000000000 was reviewed before departure.\n")


def payload_chunks(ordinal, target):
    """Complete unique records; natural closing clause variants fix byte length."""
    h = hashlib.sha256(f"{SEED}:{ordinal}".encode()).hexdigest()
    topics = ["harbour", "orchard", "library", "transit", "weather", "workshop", "archive"]
    topic = topics[ordinal % len(topics)]
    header = f"# {topic.title()} activity {ordinal:06d}\n\nSession {h[:12]} records café observations, naïve assumptions and 東京 notes.\nEarly dispatch: station {ordinal + 17} uses route {h[12:20]}.\n\n".encode()
    require(target >= len(header) + 1024, "payload target too small")
    yield header
    used, record = len(header), 0
    while True:
        code = hashlib.sha256(f"{h}:{record}".encode()).hexdigest()[:16]
        variants = [
            f"## Review {record}\nThe {topic} team inspected compartment {code} on day {record + 1}. Temperature {11 + (record * 7 + ordinal) % 29} degrees prompted a separate inventory review. The similarly named {topics[(ordinal + record + 1) % 7]} team kept its own schedule; its observations do not identify this station. München staff recorded an independent handover.\n\n",
            f"| activity | location | observation |\n|---|---|---|\n| {ordinal}-{record} | bay {code} | valves remained sealed during inspection |\n\nThe ledger distinguishes the completed inspection from a proposed inspection at another site. A résumé from Québec records the distinction for later review.\n\n",
            f"```text\nactivity={ordinal}-{record}\nroute={code}\nstatus=reviewed\n```\nThe local shift reviewed the record and compared {record + 3} measurements. Workers scheduled equipment checks before the next departure; an older archive entry describes a different date and should remain a distinct event.\n\n",
        ]
        data = variants[(ordinal + record) % 3].encode()
        if target - used - len(data) < closing_min_bytes(ordinal, record + 1, h):
            break
        yield data
        used += len(data)
        record += 1
    remaining = target - used - closing_min_bytes(ordinal, record, h)
    yield closing_record(ordinal, record, h, remaining)


def packet_at(path, guard=None):
    if guard:
        guard.check()
    path = Path(path).resolve(strict=True)
    packet = load_json(regular(path / "packet.json"), guard)
    require(packet["schema"] == 1 and packet["output"] == str(path), "packet ownership/path mismatch")
    if guard:
        guard.output, guard.account = path, Path(packet["account_root"]).resolve(strict=True)
        require(path.is_relative_to(guard.account) and path != guard.account, "input packet outside owned account")
        guard.check(force=True)
    require(packet["generator_sha256"] == digest(Path(__file__).resolve(), guard), "generator pin mismatch")
    require(packet["count"] == COUNTS[packet["tier"]] and packet["content_bytes"] == packet["count"] * 100000,
            "packet count/tier/content mismatch")
    binary_pin(packet["binary"]["path"], packet["binary"]["sha256"], guard)
    require(load_json(regular(path / "command-plan.json"), guard) == commands(packet["binary"]["path"], path, packet["tier"]), "command plan mismatch")
    require(packet["overlay"] != UNAVAILABLE, "verified public overlay UNAVAILABLE")
    for item in packet["overlay"]:
        require(digest(regular(item["path"]), guard) == item["sha256"] and Path(item["path"]).stat().st_size == item["bytes"], "overlay drift")
        for notice in item["notice_inventory"]:
            require(digest(regular(notice["path"]), guard) == notice["sha256"], "notice drift")
    return path, packet


def generation_admission(packet, explicit, free, existing):
    require(explicit, "generation requires --admit-input-generation; CLI import remains refused")
    require(packet["count"] <= 1000, "generation refused: " + ", ".join(packet["input_generation"]["large_tier_missing"]))
    projection = packet["content_bytes"] + packet["count"] * 16384 + 8 * 1024 ** 2
    require(projection <= INPUT_CAP, "finite input allocated-byte bound exceeded")
    require(free - projection >= FREE_FLOOR, "32 GiB free floor including preparation reserve unavailable")
    require(existing + projection <= 100 * GIB, "100 GiB physical account admission exceeded")
    return projection


def generate(args):
    guard = args._preparation_guard = PreparationGuard()
    output, packet = packet_at(args.packet, guard)
    account = Path(packet["account_root"])
    require(set(p.name for p in output.iterdir()) == {"packet.json", "command-plan.json"}, "generation never overwrites or resumes partial input files")
    generation_admission(packet, args.admit_input_generation, shutil.disk_usage(account).free, allocation(account, guard.quick))
    guard.check(force=True)
    sources = output / "sources"
    sources.mkdir()
    public = packet["overlay"]
    bases, targets, q, r = sizes(packet["count"], sum(e["bytes"] for e in public))
    require((q, r) == (packet["size_policy"]["adjustment_q"], packet["size_policy"]["adjustment_r"]), "size policy drift")
    with (output / "inputs.jsonl").open("x", encoding="utf-8") as inputs, (output / "inventory.jsonl").open("x", encoding="utf-8") as inventory:
        for ordinal in range(packet["count"]):
            guard.check()
            relative = f"sources/source-{ordinal:06d}.md"
            path = output / relative
            public_item = public[ordinal] if ordinal < 2 else None
            with path.open("xb") as stream:
                if public_item:
                    with open(public_item["path"], "rb") as original:
                        for chunk in iter(lambda: original.read(65536), b""):
                            guard.check()
                            stream.write(chunk)
                else:
                    for chunk in payload_chunks(ordinal - 2, targets[ordinal - 2]):
                        stream.write(chunk)
                        guard.check()
            title = public_item["title"] if public_item else f"Functional activity {ordinal - 2:06d}"
            inputs.write(json.dumps({"path": relative, "title": title}, ensure_ascii=False) + "\n")
            inventory.write(json.dumps({"ordinal": ordinal, "path": relative, "title": title, "bytes": path.stat().st_size,
                                        "sha256": digest(path, guard), "kind": "public" if public_item else "synthetic",
                                        "base_bytes": None if public_item else bases[ordinal - 2]}, sort_keys=True) + "\n")
    guard.check(force=True)
    return verify(args, guard)


def verify(args, guard=None):
    if guard is None:
        guard = args._preparation_guard = PreparationGuard()
    output, packet = packet_at(args.packet, guard)
    require(set(p.name for p in output.iterdir()) == {"packet.json", "command-plan.json", "sources", "inputs.jsonl", "inventory.jsonl"}, "unexpected packet members (labels/answers forbidden)")
    bases, targets, q, r = sizes(packet["count"], sum(e["bytes"] for e in packet["overlay"]))
    require((q, r) == (packet["size_policy"]["adjustment_q"], packet["size_policy"]["adjustment_r"]), "size policy mismatch")
    expected = {f"source-{i:06d}.md" for i in range(packet["count"])}
    require({p.name for p in (output / "sources").iterdir()} == expected, "source membership mismatch")
    total, histogram = 0, Counter()
    with regular(output / "inputs.jsonl").open(encoding="utf-8") as inputs, regular(output / "inventory.jsonl").open(encoding="utf-8") as inventory:
        for ordinal in range(packet["count"]):
            guard.check()
            inp, inv = json.loads(inputs.readline()), json.loads(inventory.readline())
            relative = f"sources/source-{ordinal:06d}.md"
            path = regular(output / relative)
            title = packet["overlay"][ordinal]["title"] if ordinal < 2 else f"Functional activity {ordinal - 2:06d}"
            require(inp == {"path": relative, "title": title}, "input list mismatch")
            if ordinal < 2:
                sha, length = packet["overlay"][ordinal]["sha256"], packet["overlay"][ordinal]["bytes"]
            else:
                sha_state = hashlib.sha256()
                for chunk in payload_chunks(ordinal - 2, targets[ordinal - 2]):
                    guard.check()
                    sha_state.update(chunk)
                sha, length = sha_state.hexdigest(), targets[ordinal - 2]
                histogram[str(bases[ordinal - 2])] += 1
            require(path.stat().st_size == length and digest(path, guard) == sha, "deterministic bytes/hash mismatch")
            decoder = codecs.getincrementaldecoder("utf-8")(errors="strict")
            with path.open("rb") as stream:
                for chunk in iter(lambda: stream.read(65536), b""):
                    guard.check()
                    decoder.decode(chunk)
                decoder.decode(b"", final=True)
            guard.check()
            require(inv == {"ordinal": ordinal, "path": relative, "title": title, "bytes": length, "sha256": sha,
                            "kind": "public" if ordinal < 2 else "synthetic", "base_bytes": None if ordinal < 2 else bases[ordinal - 2]}, "inventory mismatch")
            total += length
        require(not inputs.read(1) and not inventory.read(1), "input/inventory count mismatch")
    require(total == packet["content_bytes"] and list(histogram.values()) == packet["size_policy"]["base_counts"], "total/histogram mismatch")
    guard.check(force=True)
    return {"verified": True, "count": packet["count"], "content_bytes": total, "base_histogram": dict(histogram), "cli_launch_admitted": False, "observations": guard.observed}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("plan")
    for name in ["binary", "binary-sha256", "account-root", "output"]:
        p.add_argument("--" + name, required=True)
    p.add_argument("--tier", choices=COUNTS, required=True)
    p.add_argument("--overlay-manifest")
    for name in ["generate", "verify"]:
        p = sub.add_parser(name)
        p.add_argument("--packet", required=True)
        if name == "generate":
            p.add_argument("--admit-input-generation", action="store_true")
    args = parser.parse_args()
    try:
        result = globals()[args.command](args)
        rendered = json.dumps(result, sort_keys=True)
        if hasattr(args, "_preparation_guard"):
            args._preparation_guard.quick()
            rendered = json.dumps(result, sort_keys=True)
        print(rendered)
    except (ValueError, OSError, KeyError, TypeError, json.JSONDecodeError) as error:
        failure = {"refused": True, "error": str(error)}
        if hasattr(args, "_preparation_guard"):
            failure["observations"] = args._preparation_guard.observed
        print(json.dumps(failure), file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
