#!/usr/bin/env python3
"""Mocked tool tests: these do not demonstrate real Bazel/provider qualification."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("build_artifact", Path(__file__).with_name("build_artifact.py"))
helper = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(helper)


class MockTools:
    def __init__(self, repo, executable):
        self.repo, self.executable = repo, executable
        self.calls = []
        self.host = "aarch64-apple-darwin"
        self.package_version = "0.1.0"
        self.fail_build = False
        self.fail_smoke = False
        self.omit_artifact = False
        self.extra_artifact = False
        self.absolute_artifact = False
        self.change_source = False
        self.change_lock = False
        self.change_module_lock = False
        self.execution_root = executable.parents[3]
        self.version = "lwiki 0.1.0\n"
        self.capabilities = {"ok": True, "command": "capabilities", "schema_version": "1", "data": {"mock": True}}
        self.status_reads = 0

    def run(self, command, cwd, stdout, stderr, check):
        self.calls.append(command)
        assert cwd == self.repo and check is False
        code = 0
        if command[:2] == ["git", "rev-parse"]:
            content = "abc123\n"
        elif command[:2] == ["git", "status"]:
            self.status_reads += 1
            content = " M src/main.rs\n" if self.change_source and self.status_reads == 2 else ""
        elif command[:2] == ["cargo", "metadata"]:
            content = json.dumps({"packages": [{"name": "rust-llm-wiki", "id": "correct-package", "version": self.package_version,
                                   "manifest_path": str(self.repo / "Cargo.toml")}]})
        elif command[:2] == [sys.executable, str(self.repo / "scripts/bazel.py")]:
            args = command[2:]
            if args == ["--print-target"]:
                content = self.host + "\n"
            elif args == ["--", "version", "--gnu_format"]:
                content = "bazel 9.2.0\n"
            elif args == ["--", "build", "--config=release", "--nofetch", "//:lwiki"]:
                if self.change_lock:
                    (self.repo / "Cargo.lock").write_text("changed lock\n", encoding="utf-8")
                if self.change_module_lock:
                    (self.repo / "MODULE.bazel.lock").write_text("changed module lock\n", encoding="utf-8")
                content = "mock Bazel build output\n"
                stderr.write(b"mock build diagnostic\n")
                code = 101 if self.fail_build else 0
            elif args == ["--", "cquery", "--config=release", "--nofetch", "--output=files", "//:lwiki"]:
                content = "bazel-out/native-opt/bin/liblwiki.rlib\n"
                if not self.omit_artifact:
                    path = self.executable if self.absolute_artifact else self.executable.relative_to(self.execution_root)
                    content += str(path) + "\n"
                if self.extra_artifact:
                    content += "bazel-out/other-opt/bin/lwiki\n"
            elif args == ["--", "info", "--config=release", "--nofetch", "execution_root"]:
                content = str(self.execution_root) + "\n"
            else:
                raise AssertionError("unexpected Bazel wrapper command: " + repr(command))
        elif command[1:] == ["--version"]:
            assert Path(command[0]).read_bytes() == self.executable.read_bytes()
            content = self.version
            code = 1 if self.fail_smoke else 0
        elif command[1:] == ["--json", "capabilities"]:
            content = json.dumps(self.capabilities) + "\n"
        else:
            raise AssertionError("unexpected command: " + repr(command))
        stdout.write(content.encode())
        return subprocess.CompletedProcess(command, code)


class BuildArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name).resolve() / "repo"
        self.repo.mkdir()
        (self.repo / "Cargo.toml").write_text('[package]\nname = "rust-llm-wiki"\n')
        (self.repo / "Cargo.lock").write_text("mock lock\n")
        contract_contents = {
            "MODULE.bazel": 'module(name = "rust_llm_wiki", version = "0.1.0")\n',
            "MODULE.bazel.lock": '{}\n',
            "cargo-bazel-lock.json": '{}\n',
            ".bazelversion": '9.2.0\n',
            ".bazelrc": 'build:release --compilation_mode=opt\n',
            "rust-toolchain.toml": '[toolchain]\nchannel = "1.98.0"\n',
            "scripts/bazel.py": '# mock wrapper\n',
        }
        for name, contents in contract_contents.items():
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(contents, encoding="utf-8")
        stale = self.repo / "target/release/lwiki"
        stale.parent.mkdir(parents=True)
        stale.write_bytes(b"stale binary must never be selected")
        stale_bazel = self.repo / "bazel-bin/lwiki"
        stale_bazel.parent.mkdir(parents=True)
        stale_bazel.write_bytes(b"stale Bazel binary must never be selected")
        self.executable = Path(self.temp.name).resolve() / "execution root with spaces café/bazel-out/native-opt/bin/lwiki"
        self.executable.parent.mkdir(parents=True)
        self.executable.write_bytes(b"mock fresh native executable\n")
        self.executable.chmod(0o755)
        self.output = self.repo / "evidence/artifact"
        self.tools = MockTools(self.repo, self.executable)

    def build(self):
        with patch.object(helper.subprocess, "run", self.tools.run):
            return helper.build(self.output, self.repo)

    def archive_path(self):
        return self.output / ("lwiki-" + self.tools.package_version + "-" + self.tools.host + ".tar.gz")

    def test_bazel_artifact_path_native_wrapper_and_archive(self):
        with patch.dict(os.environ, {"CARGO_TARGET_DIR": str(self.executable.parent.parent.parent),
                                     "CARGO_BUILD_TARGET": "wasm32-unknown-unknown"}):
            self.build()
        command = next(call for call in self.tools.calls if "build" in call)
        self.assertEqual(command, [sys.executable, str(self.repo / "scripts/bazel.py"), "--", "build",
                                   "--config=release", "--nofetch", "//:lwiki"])
        self.assertFalse(any(call[:2] == ["cargo", "build"] for call in self.tools.calls))
        self.assertFalse(any(call[0] == "rustc" for call in self.tools.calls))
        copied = self.output / "lwiki"
        self.assertEqual(copied.read_bytes(), self.executable.read_bytes())
        observed_mode = self.executable.stat().st_mode & 0o777
        self.assertEqual(copied.stat().st_mode & 0o777, observed_mode)
        if os.name != "nt":
            self.assertEqual(observed_mode, 0o755)
        info = json.loads((self.output / "build-info.json").read_text())
        self.assertEqual(info["binary_sha256"], hashlib.sha256(copied.read_bytes()).hexdigest())
        self.assertEqual(info["cargo_lock_sha256"], hashlib.sha256(b"mock lock\n").hexdigest())
        self.assertEqual(info["host"], self.tools.host)
        self.assertEqual(info["target"], self.tools.host)
        self.assertEqual(info["package_version"], "0.1.0")
        self.assertEqual(info["bazel"], "bazel 9.2.0")
        self.assertEqual(info["build_command"], command)
        self.assertEqual(info["bazel_version_pin"], "9.2.0")
        self.assertIn('channel = "1.98.0"', info["rust_toolchain"])
        self.assertIn('module(name = "rust_llm_wiki"', info["bazel_module"])
        self.assertEqual(info["build_contract_sha256"], {
            name: hashlib.sha256((self.repo / name).read_bytes()).hexdigest()
            for name in helper.BUILD_CONTRACT_FILES
        })
        self.assertEqual(info["source"]["commit"], "abc123")
        self.assertFalse(info["source"]["dirty"])
        self.assertIn("not qualified provenance", info["source"]["note"])
        self.assertIn("not full qualification", info["scope"])
        self.assertIn(":(exclude,literal)evidence/artifact", self.tools.calls[1])
        archive = self.archive_path()
        self.assertEqual(archive.name, "lwiki-0.1.0-aarch64-apple-darwin.tar.gz")
        self.assertEqual((self.output / (archive.name + ".sha256")).read_text(encoding="utf-8"),
                         hashlib.sha256(archive.read_bytes()).hexdigest() + "  " + archive.name + "\n")
        with tarfile.open(archive) as bundle:
            self.assertEqual(set(bundle.getnames()), {"lwiki", "version.txt", "capabilities.json", "build-info.json"})
            self.assertEqual(bundle.getmember("lwiki").mode & 0o777, observed_mode)
            self.assertEqual(hashlib.sha256(bundle.extractfile("lwiki").read()).hexdigest(), info["binary_sha256"])
            self.assertEqual(json.load(bundle.extractfile("build-info.json")), info)

    def test_existing_empty_or_nonempty_output_refused_before_tools(self):
        self.output.mkdir(parents=True)
        with self.assertRaisesRegex(helper.BuildError, "already exists"):
            self.build()
        sentinel = self.output / "keep.txt"
        sentinel.write_text("preserve me")
        with self.assertRaisesRegex(helper.BuildError, "already exists"):
            self.build()
        self.assertEqual(self.tools.calls, [])
        self.assertEqual(sentinel.read_text(), "preserve me")

    def test_repeated_success_refused_and_preserved(self):
        self.build()
        before = (self.output / "build-info.json").read_bytes()
        count = len(self.tools.calls)
        with self.assertRaisesRegex(helper.BuildError, "already exists"):
            self.build()
        self.assertEqual(len(self.tools.calls), count)
        self.assertEqual((self.output / "build-info.json").read_bytes(), before)

    def test_broken_symlink_output_refused(self):
        self.output.parent.mkdir(parents=True)
        try:
            self.output.symlink_to(self.output.parent / "missing")
        except OSError as error:
            if os.name == "nt" and getattr(error, "winerror", None) == 1314:
                self.skipTest("Windows symlink privilege unavailable")
            raise
        with self.assertRaisesRegex(helper.BuildError, "already exists"):
            self.build()
        self.assertEqual(self.tools.calls, [])
        self.assertTrue(self.output.is_symlink())

    def assert_failed_candidate(self):
        self.assertFalse((self.output / "build-info.json").exists())
        self.assertFalse(self.archive_path().exists())
        self.assertFalse((self.output / (self.archive_path().name + ".sha256")).exists())
        self.assertTrue((self.output / "failure.txt").exists())

    def test_build_failure_preserves_logs_and_cannot_be_reused(self):
        self.tools.fail_build = True
        with self.assertRaisesRegex(helper.BuildError, "exit 101"):
            self.build()
        self.assert_failed_candidate()
        self.assertIn("Bazel build", (self.output / "bazel-build.log").read_text())
        self.assertIn("diagnostic", (self.output / "bazel-build.log.stderr.log").read_text())
        self.assertFalse((self.output / "lwiki").exists())
        count = len(self.tools.calls)
        with self.assertRaisesRegex(helper.BuildError, "already exists"):
            self.build()
        self.assertEqual(len(self.tools.calls), count)

    def test_missing_selected_artifact_never_falls_back_to_stale_binary(self):
        self.tools.omit_artifact = True
        with self.assertRaisesRegex(helper.BuildError, "exactly one executable"):
            self.build()
        self.assert_failed_candidate()
        self.assertFalse((self.output / "lwiki").exists())

    def test_smoke_failure_has_no_success_info(self):
        self.tools.fail_smoke = True
        with self.assertRaisesRegex(helper.BuildError, "exit 1"):
            self.build()
        self.assert_failed_candidate()
        self.assertTrue((self.output / "version.txt").exists())

    def test_valid_json_error_envelope_is_not_success(self):
        self.tools.capabilities = {"ok": False, "command": "capabilities", "error": "mock error"}
        with self.assertRaisesRegex(helper.BuildError, "successful capabilities envelope"):
            self.build()
        self.assert_failed_candidate()

    def test_wrong_capabilities_command_is_not_success(self):
        self.tools.capabilities["command"] = "other-command"
        with self.assertRaisesRegex(helper.BuildError, "successful capabilities envelope"):
            self.build()
        self.assert_failed_candidate()

    def test_version_must_match_selected_package(self):
        self.tools.version = "lwiki 0.0.0\n"
        with self.assertRaisesRegex(helper.BuildError, "version does not match"):
            self.build()
        self.assert_failed_candidate()

    def test_source_state_change_refused(self):
        self.tools.change_source = True
        with self.assertRaisesRegex(helper.BuildError, "changed during the build"):
            self.build()
        self.assert_failed_candidate()

    def test_lock_change_refused_even_with_unchanged_git_status(self):
        self.tools.change_lock = True
        with self.assertRaisesRegex(helper.BuildError, "changed during the build"):
            self.build()
        self.assert_failed_candidate()

    def test_bazel_module_lock_change_refused_even_with_unchanged_git_status(self):
        self.tools.change_module_lock = True
        with self.assertRaisesRegex(helper.BuildError, "changed during the build"):
            self.build()
        self.assert_failed_candidate()

    def test_absolute_cquery_artifact_supported(self):
        self.tools.absolute_artifact = True
        self.build()
        self.assertEqual((self.output / "lwiki").read_bytes(), self.executable.read_bytes())

    def test_ambiguous_cquery_artifacts_rejected(self):
        self.tools.extra_artifact = True
        with self.assertRaisesRegex(helper.BuildError, "exactly one executable"):
            self.build()
        self.assert_failed_candidate()

    def test_empty_native_target_rejected_before_build(self):
        self.tools.host = ""
        with self.assertRaisesRegex(helper.BuildError, "one native target"):
            self.build()
        self.assertFalse(any("build" in call for call in self.tools.calls))
        self.assert_failed_candidate()

    def test_packaging_failure_removes_success_info(self):
        def fail_archive(path, mode):
            Path(path).write_bytes(b"partial archive")
            (self.output / "unrelated.tar.gz").write_bytes(b"preserve unrelated output")
            raise OSError("mock packaging failure")

        with patch.object(helper.tarfile, "open", side_effect=fail_archive):
            with self.assertRaisesRegex(OSError, "packaging failure"):
                self.build()
        self.assert_failed_candidate()
        self.assertTrue((self.output / "capabilities.json").exists())
        self.assertEqual((self.output / "unrelated.tar.gz").read_bytes(), b"preserve unrelated output")

    def test_checksum_failure_removes_archive_and_partial_checksum(self):
        write_text = Path.write_text

        def fail_checksum(path, contents, **kwargs):
            if path.name.endswith(".sha256"):
                path.write_bytes(b"partial checksum")
                raise OSError("mock checksum failure")
            return write_text(path, contents, **kwargs)

        with patch.object(Path, "write_text", fail_checksum):
            with self.assertRaisesRegex(OSError, "checksum failure"):
                self.build()
        self.assert_failed_candidate()
        self.assertTrue((self.output / "capabilities.json").exists())

    def test_archive_name_and_info_use_the_selected_package_version(self):
        self.tools.package_version = "0.2.3"
        self.tools.version = "lwiki 0.2.3\n"
        self.build()
        self.assertEqual(self.archive_path().name, "lwiki-0.2.3-aarch64-apple-darwin.tar.gz")
        self.assertTrue(self.archive_path().exists())
        self.assertEqual(json.loads((self.output / "build-info.json").read_text())["package_version"], "0.2.3")

    def test_windows_host_names_binary_with_exe(self):
        self.tools.host = "x86_64-pc-windows-msvc"
        renamed = self.executable.with_suffix(".exe")
        self.executable.rename(renamed)
        self.executable = renamed
        self.tools.executable = renamed
        self.build()
        self.assertTrue((self.output / "lwiki.exe").exists())
        info = json.loads((self.output / "build-info.json").read_text())
        self.assertEqual(info["binary"], "lwiki.exe")
        self.assertEqual(info["target"], "x86_64-pc-windows-msvc")
        archive = self.archive_path()
        self.assertEqual(archive.name, "lwiki-0.1.0-x86_64-pc-windows-msvc.tar.gz")
        checksum = self.output / (archive.name + ".sha256")
        self.assertEqual(checksum.read_text(encoding="utf-8"),
                         hashlib.sha256(archive.read_bytes()).hexdigest() + "  " + archive.name + "\n")
        with tarfile.open(archive) as bundle:
            self.assertEqual(set(bundle.getnames()), {"lwiki.exe", "version.txt", "capabilities.json", "build-info.json"})


if __name__ == "__main__":
    unittest.main()
