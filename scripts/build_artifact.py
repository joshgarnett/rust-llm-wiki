#!/usr/bin/env python3
"""Build and smoke-test a fresh native candidate; this is not qualification."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile


class BuildError(RuntimeError):
    pass


BUILD_CONTRACT_FILES = (
    "Cargo.toml", "Cargo.lock", "MODULE.bazel", "MODULE.bazel.lock",
    "cargo-bazel-lock.json", ".bazelversion", ".bazelrc", "rust-toolchain.toml",
    "scripts/bazel.py",
)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def run(command, repo, output, name):
    """Keep both streams, including when a command fails."""
    stdout = output / name
    with stdout.open("wb") as out, (output / (name + ".stderr.log")).open("wb") as err:
        result = subprocess.run(command, cwd=repo, stdout=out, stderr=err, check=False)
    if result.returncode:
        raise BuildError("{} failed (exit {}); see {}".format(
            subprocess.list2cmdline(command), result.returncode, stdout))
    return stdout


def source_state(repo, output, phase):
    commit = run(["git", "rev-parse", "HEAD"], repo, output, "git-" + phase + "-head.txt")
    command = ["git", "status", "--porcelain=v1", "--untracked-files=all", "--", "."]
    try:
        relative = output.relative_to(repo).as_posix()
    except ValueError:
        pass
    else:
        # Evidence created by this helper is not a source change.
        command.extend([":(exclude,literal)" + relative])
    status = run(command, repo, output, "git-" + phase + "-status.txt")
    return commit.read_text(encoding="utf-8").strip(), status.read_bytes()


def build(output, repo=None):
    repo = Path(repo or Path(__file__).resolve().parent.parent).resolve()
    output = Path(output)
    if output.exists() or output.is_symlink():
        raise BuildError("output already exists; choose a fresh directory: " + str(output.absolute()))
    output = output.resolve()
    try:
        output.mkdir(parents=True, exist_ok=False)
    except FileExistsError as error:
        raise BuildError("output already exists; choose a fresh directory: " + str(output)) from error
    success_paths = [output / "build-info.json"]
    try:
        before = source_state(repo, output, "before")
        contract_hashes = {name: sha256(repo / name) for name in BUILD_CONTRACT_FILES}
        wrapper = [sys.executable, str(repo / "scripts/bazel.py")]
        target_path = run(wrapper + ["--print-target"], repo, output, "native-target.txt")
        targets = target_path.read_text(encoding="utf-8").splitlines()
        if len(targets) != 1 or not targets[0].strip():
            raise BuildError("Bazel wrapper did not report one native target")
        host = targets[0].strip()
        bazel = run(wrapper + ["--", "version", "--gnu_format"], repo, output,
                    "bazel-version.txt").read_text(encoding="utf-8").strip()
        metadata_path = run(
            ["cargo", "metadata", "--locked", "--offline", "--no-deps", "--format-version", "1"],
            repo, output, "cargo-metadata.json",
        )
        packages = [p for p in json.loads(metadata_path.read_text(encoding="utf-8"))["packages"]
                    if p["name"] == "rust-llm-wiki"
                    and Path(p["manifest_path"]).resolve() == repo / "Cargo.toml"]
        if len(packages) != 1:
            raise BuildError("metadata did not identify the repository's rust-llm-wiki package")
        package_version = packages[0]["version"]
        command = wrapper + ["--", "build", "--config=release", "--nofetch", "//:lwiki"]
        run(command, repo, output, "bazel-build.log")
        artifacts = run(
            wrapper + ["--", "cquery", "--config=release", "--nofetch", "--output=files", "//:lwiki"],
            repo, output, "bazel-artifacts.txt",
        )
        execution_root_path = run(
            wrapper + ["--", "info", "--config=release", "--nofetch", "execution_root"],
            repo, output, "bazel-execution-root.txt",
        )
        execution_roots = execution_root_path.read_text(encoding="utf-8").splitlines()
        if len(execution_roots) != 1 or not execution_roots[0].strip():
            raise BuildError("Bazel did not report one execution root")
        execution_root = Path(execution_roots[0])
        if not execution_root.is_absolute():
            raise BuildError("Bazel execution root is not an absolute path")
        executables = set()
        for line in artifacts.read_text(encoding="utf-8").splitlines():
            executable = Path(line)
            if executable.name in {"lwiki", "lwiki.exe"}:
                executables.add(executable if executable.is_absolute() else execution_root / executable)
        if len(executables) != 1:
            raise BuildError("Bazel cquery did not report exactly one executable for //:lwiki")
        binary = output / ("lwiki.exe" if "windows" in host else "lwiki")
        shutil.copy2(executables.pop(), binary)
        version = run([str(binary), "--version"], repo, output, "version.txt")
        if version.read_text(encoding="utf-8").strip() != "lwiki " + package_version:
            raise BuildError("copied binary version does not match Cargo package metadata")
        capabilities = run([str(binary), "--json", "capabilities"], repo, output, "capabilities.json")
        envelope = json.loads(capabilities.read_text(encoding="utf-8"))
        if (not isinstance(envelope, dict) or envelope.get("ok") is not True
                or envelope.get("command") != "capabilities"):
            raise BuildError("copied binary did not return a successful capabilities envelope")
        after = source_state(repo, output, "after")
        if before != after or contract_hashes != {name: sha256(repo / name) for name in BUILD_CONTRACT_FILES}:
            raise BuildError("source HEAD, status, or build contract files changed during the build")
        info = {
            "schema_version": 1,
            "scope": "native release build and copied-binary version/capabilities smoke only; not full qualification",
            "source": {
                "commit": before[0], "dirty": bool(before[1]),
                "note": "Informational, not qualified provenance. HEAD/status and build contract digests were compared before/after; unchanged status does not prove dirty file contents stayed unchanged.",
            },
            "bazel": bazel, "host": host, "target": host,
            "package_version": package_version, "build_command": command,
            "rust_toolchain": (repo / "rust-toolchain.toml").read_text(encoding="utf-8"),
            "bazel_module": (repo / "MODULE.bazel").read_text(encoding="utf-8"),
            "bazel_version_pin": (repo / ".bazelversion").read_text(encoding="utf-8").strip(),
            "build_contract_sha256": contract_hashes,
            "cargo_lock_sha256": contract_hashes["Cargo.lock"],
            "binary": binary.name, "binary_sha256": sha256(binary),
        }
        info_path = output / "build-info.json"
        info_path.write_text(json.dumps(info, indent=2) + "\n", encoding="utf-8")
        archive_path = output / "lwiki-{}-{}.tar.gz".format(package_version, host)
        checksum_path = output / (archive_path.name + ".sha256")
        success_paths.extend([archive_path, checksum_path])
        with tarfile.open(archive_path, "w:gz") as bundle:
            for path in [binary, version, capabilities, info_path]:
                bundle.add(path, arcname=path.name, recursive=False)
        checksum_path.write_text("{}  {}\n".format(sha256(archive_path), archive_path.name), encoding="utf-8")
        return output
    except Exception as error:
        # A failed attempt keeps its logs, but never advertises a successful candidate.
        for path in success_paths:
            path.unlink(missing_ok=True)
        (output / "failure.txt").write_text(str(error) + "\n", encoding="utf-8")
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", help="fresh output directory (must not exist)")
    args = parser.parse_args()
    try:
        output = build(args.output)
    except (BuildError, OSError, ValueError, KeyError) as error:
        print("build failed: {}\noutput: {}".format(error, Path(args.output).resolve()), file=sys.stderr)
        return 1
    print(output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
