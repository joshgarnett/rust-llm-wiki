#!/usr/bin/env python3
"""Require four successful native runs of the exact verified release executable bytes."""

import argparse
import json
from pathlib import Path
import sys

import artifact_platform as platforms
import qualify_artifact as qualification
import verify_release as verifier


RUNNERS = {
    "ubuntu-24.04": ("x86_64-unknown-linux-gnu", "Linux", "x86_64", None),
    "macos-15": ("aarch64-apple-darwin", "Darwin", "arm64", 15),
    "macos-26": ("aarch64-apple-darwin", "Darwin", "arm64", 26),
    "xcode-27": ("aarch64-apple-darwin", "Darwin", "arm64", 27),
}
REQUIRED_STAGES = {
    "fmt", "clippy", "test-build", "tests", "release-skill", "metadata", "artifact-tests",
    "install-preview", "install", "version", "capabilities", "setup", "capture", "search",
    "context", "recover", "rebuild", "check", "config-show", "skill-export", "linkage",
    "agent-preview", "agent-install", "agent-status", "agent-install-repeat", "agent-remove", "agent-status-removed",
}
BAZEL_TARGETS = {"fmt": "//:format", "clippy": "//:clippy", "test-build": "//:test_build",
                 "tests": "//:test", "release-skill": "//:skill_export_test", "artifact-tests": "//:artifact_tests"}
CLI_COMMANDS = {"version": ["--version"], "capabilities": ["capabilities"], "setup": ["setup"],
                "capture": ["source", "add"], "search": ["search"], "context": ["context"],
                "recover": ["recover"], "rebuild": ["index", "rebuild"], "check": ["check"],
                "config-show": ["config", "show"], "skill-export": ["skill", "export"]}
CLI_COMMANDS.update({"agent-preview": ["--dry-run", "skill", "install"], "agent-install": ["skill", "install"],
                     "agent-status": ["skill", "status"], "agent-install-repeat": ["skill", "install"],
                     "agent-remove": ["skill", "remove"], "agent-status-removed": ["skill", "status"]})


def verify(root, release_directory, commit, version):
    manifest = qualification.read_manifest(release_directory, commit, version)
    records = {record["target"]: record for record in manifest["targets"]}
    reports = sorted(root.glob("*/qualification.json"))
    if len(reports) != len(RUNNERS):
        raise qualification.QualificationError("exactly four native qualification reports are required")
    seen, results = set(), []
    for path in reports:
        report = verifier.strict_json(path.read_bytes())
        if not isinstance(report, dict):
            raise qualification.QualificationError("invalid native report")
        host = report.get("host")
        label = host.get("runner_label") if isinstance(host, dict) else None
        if label not in RUNNERS or label in seen:
            raise qualification.QualificationError("missing, duplicate or unexpected native runner")
        seen.add(label)
        target, system, machine, major = RUNNERS[label]
        record = records[target]
        if (report.get("schema_version") != "lwiki.native-qualification.v1"
                or report.get("status") != "passed"
                or report.get("source_commit") != commit or report.get("package_version") != version
                or report.get("source_clean_before") is not True or report.get("source_clean_after") is not True
                or report.get("target") != target or report.get("artifact") != record
                or report.get("existing_archive") is not True or report.get("installed_workflow") is not True
                or report.get("agent_lifecycle") is not True
                or report.get("binary_sha256_after") != record["binary_sha256"]
                or report.get("installed_binary_sha256") != record["binary_sha256"]
                or report.get("archive_sha256_after") != record["archive_sha256"]
                or host.get("system") != system or host.get("machine") != machine):
            raise qualification.QualificationError("native qualification identity or result mismatch: " + str(label))
        host_version = host.get("macos_version") if major else host.get("glibc_version")
        kind = "macos" if major else "glibc"
        canonical = platforms.version(host_version, kind)
        if (canonical != host_version or (major and int(canonical.split(".")[0]) != major)
                or qualification.version_tuple(canonical) < qualification.version_tuple(record["platform_requirements"]["minimum_version"])):
            raise qualification.QualificationError("native runner version does not meet its declared row: " + label)
        if major is None and 'VERSION_ID="24.04"' not in host.get("os_release", ""):
            raise qualification.QualificationError("Linux qualification must run on Ubuntu 24.04")
        stages = [verifier.strict_json(line) for line in (path.parent / "stages.jsonl").read_bytes().splitlines()]
        if (any(not isinstance(stage, dict) for stage in stages)
                or {stage.get("stage") for stage in stages} != REQUIRED_STAGES
                or len(stages) != len(REQUIRED_STAGES)
                or any(type(stage.get("exit_code")) is not int or stage["exit_code"] != 0 for stage in stages)):
            raise qualification.QualificationError("native qualification stages are incomplete: " + label)
        for stage in stages:
            arguments = stage.get("argv")
            if not isinstance(arguments, list) or not all(isinstance(value, str) for value in arguments):
                raise qualification.QualificationError("invalid native stage command: " + label)
            if stage["stage"] in BAZEL_TARGETS and BAZEL_TARGETS[stage["stage"]] not in arguments:
                raise qualification.QualificationError("native suite target is missing: " + label)
            if stage["stage"] in ("tests", "release-skill", "artifact-tests"):
                if ("--test_env=LWIKI_QUALIFY_BINARY=" + report["binary_path"] not in arguments
                        or "--test_strategy=standalone" not in arguments or "--nocache_test_results" not in arguments):
                    raise qualification.QualificationError("native CLI tests did not pin the archived executable: " + label)
            if stage["stage"] in CLI_COMMANDS:
                expected = ([report.get("installed_binary_path")] + (["--json", "--offline"]
                            if stage["stage"] != "version" else []) + CLI_COMMANDS[stage["stage"]])
                if arguments[:len(expected)] != expected or not report.get("installed_binary_path"):
                    raise qualification.QualificationError("own-note checks did not run the installed executable: " + label)
        results.append({"runner_label": label, "host": host, "target": target,
                        "archive_sha256": record["archive_sha256"], "binary_sha256": record["binary_sha256"],
                        "platform_requirements": record["platform_requirements"],
                        "qualification_report_sha256": qualification.digest(path),
                        "stage_report_sha256": qualification.digest(path.parent / "stages.jsonl")})
    if seen != set(RUNNERS):
        raise qualification.QualificationError("missing required native runner")
    return {"schema_version": "lwiki.release-native-qualification.v1", "source_commit": commit,
            "package_version": version, "status": "passed", "runners": results}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="downloaded native evidence directories")
    parser.add_argument("--release-dir", required=True, type=Path)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args(argv)
    try:
        result = verify(args.root, args.release_dir, args.commit, args.version)
    except (qualification.QualificationError, verifier.VerificationError, platforms.PlatformError,
            OSError, ValueError, KeyError, TypeError, UnicodeError) as error:
        print("native qualification verification failed: " + str(error), file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
