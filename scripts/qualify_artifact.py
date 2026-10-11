#!/usr/bin/env python3
"""Run native correctness and installed-note checks against one archived executable."""

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time

import artifact_platform as platforms
import verify_release as verifier


class QualificationError(ValueError):
    pass


PROFILE_FILE = "native-rust-actions.json"
PROFILE_QUERY = 'mnemonic("Rustc", filter("^//:", deps(//:test_build)))'
PROFILE_LIMIT = 32 * 1024 * 1024
PROFILE_FLAGS = ["aquery", "--nofetch", "--config=release", "--output=jsonproto",
                 "--include_commandline", "--include_param_files", "--noinclude_artifacts", PROFILE_QUERY]


def digest(path):
    with Path(path).open("rb") as source:
        return verifier.sha256_stream(source)


def observe():
    return datetime.now(timezone.utc).isoformat()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def command_text(command, cwd=None):
    return subprocess.check_output(command, cwd=cwd, text=True, timeout=30).strip()


def source_identity(repo):
    commit = command_text(["git", "rev-parse", "HEAD"], repo)
    if command_text(["git", "status", "--porcelain"], repo):
        raise QualificationError("qualification requires a clean source commit")
    return commit


def package_version(repo):
    # Cargo's package version is a literal in this repository. Keep the native
    # system Python sufficient on older runners; do not require a TOML package.
    sections = re.findall(r"(?ms)^\[package\]\s*\n(.*?)(?=^\[|\Z)", (repo / "Cargo.toml").read_text(encoding="utf-8"))
    versions = re.findall(r'^version\s*=\s*"([^"\n]+)"\s*$', sections[0], re.MULTILINE) if len(sections) == 1 else []
    if len(versions) != 1:
        raise QualificationError("Cargo.toml must name one literal package version")
    verifier.validate_identity("0" * 40, versions[0])
    return versions[0]


def host_information(target, runner_label, expected_macos_major=None):
    host = {"runner_label": runner_label, "system": platform.system(),
            "machine": platform.machine(), "kernel_release": platform.release(),
            "runner_image_os": os.environ.get("ImageOS"),
            "runner_image_version": os.environ.get("ImageVersion")}
    if target == "aarch64-apple-darwin":
        if host["system"] != "Darwin" or host["machine"] != "arm64":
            raise QualificationError("the Mac archive requires a native Apple Silicon runner")
        host["macos_version"] = platforms.version(command_text(["sw_vers", "-productVersion"]), "macos")
        host["macos_build"] = command_text(["sw_vers", "-buildVersion"])
        if (expected_macos_major is not None
                and int(host["macos_version"].split(".")[0]) != expected_macos_major):
            raise QualificationError("the runner is not running the required macOS version")
    elif target == "x86_64-unknown-linux-gnu":
        if host["system"] != "Linux" or host["machine"] != "x86_64" or expected_macos_major is not None:
            raise QualificationError("the Linux archive requires a native Linux x86_64 runner")
        libc = command_text(["getconf", "GNU_LIBC_VERSION"]).split()
        if len(libc) != 2 or libc[0] != "glibc":
            raise QualificationError("the Linux runner must use glibc")
        host["glibc_version"] = platforms.version(libc[1], "glibc")
        host["os_release"] = Path("/etc/os-release").read_text(encoding="utf-8")
    else:
        raise QualificationError("unsupported native qualification target")
    return host


def version_tuple(value):
    parts = tuple(map(int, value.split(".")))
    return parts + (0,) * (3 - len(parts))


def read_manifest(release_directory, commit, version):
    manifest = verifier.strict_json((release_directory / "release-manifest.json").read_bytes())
    if (not isinstance(manifest, dict)
            or manifest.get("schema_version") != "lwiki.release-manifest.v2"
            or manifest.get("source_commit") != commit or manifest.get("package_version") != version):
        raise QualificationError("installation requires the verified manifest for this source and version")
    rows = manifest.get("targets")
    if (not isinstance(rows, list) or len(rows) != len(verifier.TARGETS)
            or any(not isinstance(row, dict) for row in rows)
            or [row.get("target") for row in rows] != list(verifier.TARGETS)):
        raise QualificationError("installation requires both real release targets in manifest order")
    if (release_directory / "release-manifest.txt").read_text(encoding="ascii") != verifier.manifest_text(manifest):
        raise QualificationError("JSON and installer manifests disagree")
    return manifest


def prepare_archive(archive, evidence, commit, version, target, host, release_directory=None):
    if archive.is_symlink():
        raise QualificationError("the archive must be a regular file, not a symlink")
    archive = archive.resolve(strict=True)
    record = verifier.verify_archive(archive, Path(str(archive) + ".sha256"), commit, version, target)
    required = record["platform_requirements"]
    host_version = host["macos_version"] if required["kind"] == "macos" else host["glibc_version"]
    if version_tuple(host_version) < version_tuple(required["minimum_version"]):
        raise QualificationError("this executable requires a newer operating system or glibc")
    if release_directory is not None:
        manifest = read_manifest(release_directory, commit, version)
        if next(row for row in manifest["targets"] if row["target"] == target) != record:
            raise QualificationError("the selected archive does not match the complete release manifest")
        asset = release_directory / ("lwiki-{}-{}.tar.gz".format(version, target))
        if asset.is_symlink() or digest(asset) != record["archive_sha256"]:
            raise QualificationError("the installer archive differs from the qualified archive")
    contents = verifier.read_ustar(archive)
    binary = evidence / "lwiki"
    with binary.open("xb") as destination:
        destination.write(contents["lwiki"])
    binary.chmod(0o755)
    if digest(binary) != record["binary_sha256"]:
        raise QualificationError("extracted executable differs from verified archive bytes")
    if platforms.inspect_file(binary, target, evidence) != required:
        raise QualificationError("extracted executable platform requirements changed")
    (evidence / "artifact-executable.sha256").write_text(record["binary_sha256"] + "\n")
    (evidence / "artifact-archive.sha256").write_text(record["archive_sha256"] + "\n")
    (evidence / "build-info.json").write_bytes(contents["build-info.json"])
    return archive, binary, record


def verify_unchanged(binary, archive, record, installed=None):
    if digest(binary) != record["binary_sha256"]:
        raise QualificationError("qualified executable changed during qualification")
    if digest(archive) != record["archive_sha256"]:
        raise QualificationError("qualified archive changed during qualification")
    if installed is not None and digest(installed) != record["binary_sha256"]:
        raise QualificationError("installed executable differs from the qualified archive")


def file_hashes(root):
    return {str(path.relative_to(root)): digest(path) for path in root.rglob("*") if path.is_file()}


def rust_build_profile(path):
    """Derive the observed compiler profile from bounded native aquery evidence."""
    if path.is_symlink() or path.stat().st_size > PROFILE_LIMIT:
        raise QualificationError("native compiler profile is not a bounded regular JSON file")
    graph = verifier.strict_json(path.read_bytes())
    if not isinstance(graph, dict):
        raise QualificationError("invalid native compiler action graph")
    targets = {row["id"]: row["label"] for row in graph["targets"]}
    configurations = {row["id"]: row for row in graph["configuration"]}
    observations = []
    for action in graph["actions"]:
        label = targets[action["targetId"]]
        config = configurations[action["configurationId"]]
        if (action.get("mnemonic") != "Rustc" or not label.startswith("//:")
                or not config.get("mnemonic", "").endswith("-opt")
                or not re.fullmatch("[0-9a-f]{64}", config.get("checksum", ""))
                or not re.fullmatch("[0-9a-f]{64}", action.get("actionKey", ""))):
            raise QualificationError("native compiler action is not a root release configuration")
        param_files = {}
        for row in action.get("paramFiles", []):
            name, arguments = row["execPath"], row["arguments"]
            if name in param_files or not isinstance(name, str) or not isinstance(arguments, list) or not all(isinstance(value, str) for value in arguments):
                raise QualificationError("invalid native compiler parameter file")
            param_files[name] = arguments

        def expand(arguments, stack=()):
            if not isinstance(arguments, list) or not all(isinstance(value, str) for value in arguments):
                raise QualificationError("invalid native compiler arguments")
            output = []
            for value in arguments:
                if value.startswith("@"):
                    name = value[1:]
                    if name not in param_files or name in stack or len(stack) >= 8:
                        raise QualificationError("missing or recursive native compiler parameter file")
                    output.extend(expand(param_files[name], stack + (name,)))
                else:
                    output.append(value)
                if len(output) > 100000:
                    raise QualificationError("native compiler arguments exceed observation limit")
            return output

        arguments = expand(action["arguments"])
        compiler = next((index for index, value in enumerate(arguments) if Path(value).name == "rustc"), None)
        if compiler is None:
            raise QualificationError("native Rustc action omitted its compiler command")
        arguments = arguments[compiler + 1:]
        levels = []
        for index, value in enumerate(arguments):
            option = arguments[index + 1] if value in ("-C", "--codegen") and index + 1 < len(arguments) else None
            if option is None:
                match = re.fullmatch(r"(?:-C=?|--codegen=)(opt-level=.*)", value)
                option = match[1] if match else None
            if option and option.startswith("opt-level="):
                levels.append(option.removeprefix("opt-level="))
        if not levels or any(value != "3" for value in levels):
            raise QualificationError("native Rustc action must explicitly use only opt-level 3")
        observations.append({"target": label, "action_key": action["actionKey"],
                             "configuration_mnemonic": config["mnemonic"], "configuration_checksum": config["checksum"],
                             "opt_level": "3", "test_harness": "--test" in arguments})
    labels = {row["target"] for row in observations}
    if not {"//:lwiki_lib", "//:unit_tests"} <= labels or not any(row["target"] == "//:unit_tests" and row["test_harness"] for row in observations):
        raise QualificationError("native compiler profile omitted the library or unit test harness")
    return {"schema_version": "lwiki.native-build-profile.v1", "configuration": "release", "rust_opt_level": "3",
            "aquery_file": path.name, "aquery_sha256": digest(path),
            "actions": sorted(observations, key=lambda row: (row["target"], row["action_key"]))}


class Runner:
    def __init__(self, evidence, repo):
        self.evidence = evidence
        self.repo = repo
        self.stage = "toolchain"

    def run(self, stage, command, env=None, cwd=None, timeout=5400, stdout_path=None, output_limit=None):
        self.stage = stage
        started = time.monotonic()
        limit_output = None
        if output_limit is not None:
            # Qualification runs only on native macOS/Linux. Import locally so
            # read-only tool tests remain importable on compile-only Windows.
            import resource

            def limit_output():
                resource.setrlimit(resource.RLIMIT_FSIZE, (output_limit, output_limit))
        with (self.evidence / (stage + ".log")).open("wb") as output:
            if stdout_path is None:
                result = subprocess.run(command, cwd=cwd or self.repo, env=env,
                                        stdout=output, stderr=subprocess.STDOUT, timeout=timeout, check=False)
            else:
                with stdout_path.open("xb") as stdout:
                    result = subprocess.run(command, cwd=cwd or self.repo, env=env,
                                            stdout=stdout, stderr=output, timeout=timeout, check=False, preexec_fn=limit_output)
        with (self.evidence / "stages.jsonl").open("a", encoding="utf-8") as output:
            output.write(json.dumps({"stage": stage, "argv": list(map(str, command)),
                                     "exit_code": result.returncode,
                                     "elapsed_seconds": time.monotonic() - started}) + "\n")
        if result.returncode:
            raise subprocess.CalledProcessError(result.returncode, command)
        return self.evidence / (stage + ".log")


def smoke(runner, binary, version, release_directory, record):
    home = runner.evidence / "home"
    home.mkdir(mode=0o700)
    env = {"PATH": "/usr/bin:/bin", "HOME": str(home),
           "XDG_CONFIG_HOME": str(home / ".config"), "LC_ALL": "C", "TZ": "UTC"}
    installed = None
    if release_directory is not None:
        prefix = home / ".local"
        install = ["/bin/sh", str(release_directory / "install.sh"), "--version", version,
                   "--prefix", str(prefix), "--from-dir", str(release_directory)]
        if (release_directory / "install.sh").read_bytes() != (runner.repo / "scripts/install.sh").read_bytes():
            raise QualificationError("the release installer differs from the qualified source commit")
        runner.run("install-preview", install + ["--dry-run"], env, home, 180)
        if prefix.exists():
            raise QualificationError("installer preview wrote to the installation prefix")
        runner.run("install", install, env, home, 180)
        installed = prefix / "bin/lwiki"
        if digest(installed) != record["binary_sha256"]:
            raise QualificationError("installer activated different executable bytes")
        binary = installed
    output = runner.run("version", [str(binary), "--version"], env, home, 30)
    if output.read_text(encoding="ascii") != "lwiki " + version + "\n":
        raise QualificationError("installed executable reports a different version")
    wiki = home / "My wiki"
    note = home / "My meeting notes.md"
    body = "# My meeting notes\n\nThe meeting is in the cedar room. Exact identifier QUAL-731.\n"
    note.write_text(body, encoding="utf-8")
    discovery = home / ".agents/skills/llm-wiki"
    unrelated = discovery.parent / "unrelated.txt"
    unrelated.parent.mkdir(parents=True, mode=0o700)
    unrelated.write_text("Keep my other agent files.\n", encoding="utf-8")
    agent_options = ["--target", "codex", "--scope", "user", "--directory", str(home)]
    commands = [
        ("capabilities", ["capabilities"]),
        ("setup", ["setup", str(wiki), "--title", "My wiki", "--non-interactive"]),
        ("capture", ["source", "add", str(note), "--title", "My meeting notes"]),
        ("search", ["search", "QUAL-731", "--mode", "literal"]),
        ("context", ["context", "QUAL-731", "--max-bytes", "12000", "--max-tokens", "3000"]),
        ("recover", ["recover"]),
        ("rebuild", ["index", "rebuild"]),
        ("check", ["check"]),
        ("config-show", ["config", "show"]),
        ("skill-export", ["skill", "export", "--target", "codex", "--output", str(home / "host-fixture")]),
        ("agent-preview", ["--dry-run", "skill", "install"] + agent_options),
        ("agent-install", ["skill", "install"] + agent_options),
        ("agent-status", ["skill", "status"] + agent_options),
        ("agent-install-repeat", ["skill", "install"] + agent_options),
        ("agent-remove", ["skill", "remove"] + agent_options),
        ("agent-status-removed", ["skill", "status"] + agent_options),
    ]
    results = {}
    payload = None
    payload_hashes = None
    preserved = None
    for name, arguments in commands:
        if name == "agent-preview":
            if not wiki.is_dir():
                raise QualificationError("setup did not create the user's wiki")
            preserved = {str(path): file_hashes(path) for path in (wiki, home / ".config")}
        output = runner.run(name, [str(binary), "--json", "--offline"] + arguments, env, home, 180)
        result = verifier.strict_json(output.read_bytes())
        if not isinstance(result, dict) or result.get("ok") is not True or result.get("meta", {}).get("network_used") is not False:
            raise QualificationError("failed offline own-note check: " + name)
        write_json(runner.evidence / (name + ".json"), result)
        results[name] = result["data"]
        if name.startswith("agent-"):
            data = result["data"]
            if (data.get("target") != "codex" or data.get("scope") != "user"
                    or data.get("destination") != str(discovery) or data.get("failure") is not None):
                raise QualificationError("agent installation did not report the selected fixture home")
            if name == "agent-preview":
                if (data.get("dry_run") is not True or data.get("applied") is not False
                        or discovery.exists() or discovery.is_symlink()
                        or (home / ".local/share/lwiki/skills").exists()):
                    raise QualificationError("agent preview changed the fixture installation")
            elif name in ("agent-install", "agent-status", "agent-install-repeat"):
                if (data.get("installed") is not True or data.get("durable") is not True
                        or data.get("cleanup_pending", False) is not False or not discovery.is_symlink()):
                    raise QualificationError("agent skill installation was not saved and discoverable")
                resolved = discovery.resolve(strict=True)
                if not resolved.is_relative_to(home):
                    raise QualificationError("agent discovery escaped the fixture home")
                hashes = file_hashes(resolved)
                manifest = verifier.strict_json((resolved / "manifest.json").read_bytes())
                if (not isinstance(manifest, dict) or not hashes.get("SKILL.md") or manifest.get("schema") != "lwiki.skill-manifest.v1"
                        or manifest.get("version") != version or manifest.get("target") != "shared"):
                    raise QualificationError("installed agent skill is not the packaged version")
                if name == "agent-install":
                    payload, payload_hashes = resolved, hashes
                elif resolved != payload or hashes != payload_hashes:
                    raise QualificationError("status or repeated install changed the installed agent payload")
                if name == "agent-install-repeat" and (data.get("changed") is not False or data.get("created") != []):
                    raise QualificationError("repeated agent install was not idempotent")
            else:
                if (data.get("installed") is not False or discovery.exists() or discovery.is_symlink()
                        or (name == "agent-remove" and data.get("durable") is not True)):
                    raise QualificationError("agent removal did not remove its owned discovery link")
            if unrelated.read_text(encoding="utf-8") != "Keep my other agent files.\n" or note.read_text(encoding="utf-8") != body:
                raise QualificationError("agent lifecycle changed the user's unrelated files or note")
            if not wiki.is_dir() or any(file_hashes(Path(path)) != hashes for path, hashes in preserved.items()):
                raise QualificationError("agent lifecycle changed the user's wiki or saved settings")
    if not any("QUAL-731" in hit["excerpt"]["text"] for hit in results["search"]["hits"]):
        raise QualificationError("literal search did not find the user's captured note")
    context = results["context"]
    if "QUAL-731" not in context["text"] or not context["passages"]:
        raise QualificationError("context omitted the user's captured note")
    ids = results["capture"]["allocated_ids"]
    valid = False
    for passage in context["passages"]:
        if "QUAL-731" not in passage["text"]:
            continue
        span = passage["span"]
        if body.encode("utf-8")[span["start"]:span["end"]].decode("utf-8") != passage["text"]:
            raise QualificationError("context passage does not match the cited note bytes")
        valid |= any(citation.get("kind") == "source"
                     and citation.get("reference", {}).get("source_id") == ids["source"]
                     and citation["reference"].get("source_revision") == ids["revision"]
                     and citation["reference"].get("span") == span for citation in passage["citations"])
    if not valid:
        raise QualificationError("context did not cite the captured note revision and passage")
    if results["check"]["error_count"] != 0:
        raise QualificationError("wiki check reported errors")
    return installed


def qualify(args, repo=None):
    repo = Path(repo or Path(__file__).resolve().parent.parent)
    if args.evidence:
        evidence = Path(args.evidence).absolute()
        evidence.parent.mkdir(parents=True, exist_ok=True)
        evidence.mkdir(mode=0o700)  # Refuse existing paths, including empty directories and links.
    else:
        evidence = Path(tempfile.mkdtemp(prefix="lwiki-qualification-"))
    evidence = evidence.resolve()
    print("Qualification evidence: " + str(evidence), file=sys.stderr)
    started = time.monotonic()
    report = {"schema_version": "lwiki.native-qualification.v1", "status": "running",
              "observed_start_utc": observe(), "evidence_directory": str(evidence),
              "existing_archive": args.archive is not None, "installed_workflow": False,
              "test_scope": "Full native library, fault/recovery and CLI suites; public CLI subprocesses use the extracted archive executable. Library assertions run in newly compiled test harnesses."}
    runner = Runner(evidence, repo)
    archive = binary = installed = record = None
    status = 1
    try:
        target = command_text([sys.executable, "scripts/bazel.py", "--print-target"], repo)
        commit = source_identity(repo)
        version = package_version(repo)
        host = host_information(target, args.runner_label, args.expected_macos_major)
        report.update(source_commit=commit, source_clean_before=True, package_version=version, target=target, host=host)
        write_json(evidence / "host.json", host)
        bazel = [sys.executable, "scripts/bazel.py", "--"]
        runner.run("fmt", bazel + ["test", "--nofetch", "--nocache_test_results", "//:format"])
        runner.run("clippy", bazel + ["test", "--nofetch", "--nocache_test_results", "//:clippy"])
        archive = args.archive
        if archive is None:
            runner.run("release", [sys.executable, "scripts/build_artifact.py", str(evidence / "artifact")])
            archives = list((evidence / "artifact").glob("*.tar.gz"))
            if len(archives) != 1:
                raise QualificationError("expected one native release archive")
            archive = archives[0]
        runner.stage = "archive-verification"
        archive, binary, record = prepare_archive(archive, evidence, commit, version, target, host, args.release_dir)
        report.update(artifact=record, archive_path=str(archive), binary_path=str(binary))
        write_json(evidence / "qualification.json", report)
        override = "--test_env=LWIKI_QUALIFY_BINARY=" + str(binary)
        profile = evidence / PROFILE_FILE
        runner.run("compiler-profile", bazel + PROFILE_FLAGS, timeout=180, stdout_path=profile, output_limit=PROFILE_LIMIT)
        report["build_profile"] = rust_build_profile(profile)
        write_json(evidence / "qualification.json", report)
        runner.run("test-build", bazel + ["build", "--nofetch", "--config=release", "//:test_build"])
        runner.run("tests", bazel + ["test", "--nofetch", "--config=release", "--nocache_test_results", "--test_strategy=standalone",
                                     override, "--zip_undeclared_test_outputs", "//:test"])
        runner.stage = "retrieval-evidence"
        testlogs = command_text(bazel + ["info", "--nofetch", "--config=release", "bazel-testlogs"], repo)
        shutil.copy2(Path(testlogs) / "retrieval_baseline_test/test.outputs/outputs.zip", evidence / "retrieval-baseline.zip")
        runner.run("release-skill", bazel + ["test", "--nofetch", "--config=release", "--nocache_test_results",
                                             "--test_strategy=standalone", override, "//:skill_export_test", "--test_arg=--test-threads=1"])
        runner.run("metadata", ["cargo", "metadata", "--locked", "--offline", "--no-deps", "--format-version", "1"])
        shutil.copy2(evidence / "metadata.log", evidence / "package.json")
        runner.run("artifact-tests", bazel + ["test", "--nofetch", "--config=release", "--nocache_test_results",
                                              "--test_strategy=standalone", override, "//:artifact_tests"])
        runner.stage = "executable-verification"
        verify_unchanged(binary, archive, record)
        installed = smoke(runner, binary, version, args.release_dir, record)
        runner.stage = "final-identity"
        verify_unchanged(binary, archive, record, installed)
        if source_identity(repo) != commit:
            raise QualificationError("source commit changed during qualification")
        if target == "aarch64-apple-darwin":
            runner.run("linkage", ["otool", "-L", str(binary)], timeout=30)
        else:
            runner.run("linkage", ["ldd", str(binary)], timeout=30)
        report.update(status="passed", source_clean_after=True, installed_workflow=installed is not None,
                      agent_lifecycle=True,
                      installed_binary_path=str(installed) if installed is not None else None,
                      installed_binary_sha256=digest(installed) if installed is not None else None)
        (evidence / "qualification.txt").write_text("All qualification stages passed.\n", encoding="utf-8")
        print(evidence)
        status = 0
    except (QualificationError, verifier.VerificationError, platforms.PlatformError, OSError, ValueError,
            KeyError, TypeError, subprocess.SubprocessError, EOFError) as error:
        report.update(status="failed", failed_stage=runner.stage, error=str(error))
        if isinstance(error, subprocess.CalledProcessError) and 0 < error.returncode < 256:
            status = error.returncode
        print("Qualification failed during {}; evidence: {}\n{}".format(runner.stage, evidence, error), file=sys.stderr)
    finally:
        for name, path in (("binary_sha256_after", binary), ("archive_sha256_after", archive)):
            if path is not None:
                try:
                    report[name] = digest(path)
                except OSError:
                    report[name] = None
        report.update(elapsed_seconds=time.monotonic() - started, observed_end_utc=observe())
        write_json(evidence / "qualification.json", report)
    return status


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", nargs="?", help="new evidence directory; existing paths are refused")
    parser.add_argument("--archive", type=Path, help="existing final archive to qualify without building a replacement")
    parser.add_argument("--release-dir", type=Path, help="complete verified release assets for installed-note checks")
    parser.add_argument("--runner-label", default="local", help="label recorded with native host evidence")
    parser.add_argument("--expected-macos-major", type=int, help="refuse a Mac runner running a different OS major version")
    parser.add_argument("--print-version", action="store_true", help="print the source package version without building")
    args = parser.parse_args(argv)
    if args.print_version:
        print(package_version(Path(__file__).resolve().parent.parent))
        return 0
    if args.release_dir is not None and args.archive is None:
        parser.error("--release-dir requires --archive")
    if args.release_dir is not None:
        args.release_dir = args.release_dir.resolve(strict=True)
    try:
        return qualify(args)
    except OSError as error:
        print("cannot create qualification evidence: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
