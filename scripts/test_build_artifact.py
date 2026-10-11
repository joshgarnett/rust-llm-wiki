#!/usr/bin/env python3
"""Mocked tool tests: these do not demonstrate real Bazel/provider qualification."""

import hashlib
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import shutil
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
        self.dirty = False

    def run(self, command, cwd, stdout, stderr, check):
        self.calls.append(command)
        assert cwd == self.repo and check is False
        code = 0
        if command[:2] == ["git", "rev-parse"]:
            content = "a" * 40 + "\n"
        elif command[:2] == ["git", "status"]:
            self.status_reads += 1
            content = " M src/main.rs\n" if self.dirty or self.change_source and self.status_reads == 2 else ""
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
        (self.repo / "Cargo.lock").write_bytes(b"mock lock\n")
        contract_contents = {
            "MODULE.bazel": 'module(name = "rust_llm_wiki", version = "0.1.0")\n',
            "MODULE.bazel.lock": '{}\n',
            "cargo-bazel-lock.json": '{}\n',
            ".bazelversion": '9.2.0\n',
            ".bazelrc": 'build:release --compilation_mode=opt\n',
            "rust-toolchain.toml": '[toolchain]\nchannel = "1.98.0"\n',
            "scripts/bazel.py": '# mock wrapper\n',
        }
        for name in helper.BUILD_CONTRACT_FILES:
            if name not in ("Cargo.toml", "Cargo.lock"):
                contract_contents.setdefault(name, "fixture contract\n")
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
        with patch.object(helper.subprocess, "run", self.tools.run), patch.object(helper.notices, "generate", return_value=(self.repo / "THIRD-PARTY-NOTICES").read_bytes()), patch.object(helper.verifier.platforms, "inspect_file", return_value={"kind":"macos","minimum_version":"15.0.0"}):
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
        self.assertEqual(info["platform_requirements"], {"kind":"macos","minimum_version":"15.0.0"})
        self.assertIn("Requires macOS 15.0.0 or newer.", (self.output / "INSTALL.txt").read_text())
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
        self.assertEqual(info["source"]["commit"], "a" * 40)
        self.assertFalse(info["source"]["dirty"])
        self.assertIn("not qualified provenance", info["source"]["note"])
        self.assertIn("not full qualification", info["scope"])
        self.assertIn(":(exclude,literal)evidence/artifact", self.tools.calls[1])
        archive = self.archive_path()
        self.assertEqual(archive.name, "lwiki-0.1.0-aarch64-apple-darwin.tar.gz")
        self.assertEqual((self.output / (archive.name + ".sha256")).read_text(encoding="utf-8"),
                         hashlib.sha256(archive.read_bytes()).hexdigest() + "  " + archive.name + "\n")
        with tarfile.open(archive) as bundle:
            self.assertEqual(set(bundle.getnames()), set(helper.verifier.MEMBERS))
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
        def fail_archive(path, mode, **kwargs):
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

    def test_unsupported_native_target_refused_before_build(self):
        self.tools.host = "x86_64-pc-windows-msvc"
        with self.assertRaisesRegex(helper.BuildError, "supported two-target"):
            self.build()
        self.assertFalse(any("build" in call for call in self.tools.calls))
        self.assert_failed_candidate()

    def test_candidate_archive_passes_release_member_verifier(self):
        self.build()
        contents = helper.verifier.read_ustar(self.archive_path())
        self.assertEqual(set(contents), set(helper.verifier.MEMBERS))
        checksum = self.output / (self.archive_path().name + ".sha256")
        with patch.object(helper.verifier.platforms, "inspect_bytes", return_value={"kind":"macos","minimum_version":"15.0.0"}):
            record = helper.verifier.verify_archive(self.archive_path(), checksum, "a" * 40,
                                                    self.tools.package_version, self.tools.host)
        self.assertEqual(record["binary_sha256"], helper.sha256(self.output / "lwiki"))

    def test_floor_inspection_failure_refuses_before_any_binary_smoke_execution(self):
        with patch.object(helper.subprocess, "run", self.tools.run), patch.object(helper.notices, "generate", return_value=(self.repo / "THIRD-PARTY-NOTICES").read_bytes()), patch.object(helper.verifier.platforms, "inspect_file", side_effect=helper.verifier.platforms.PlatformError("missing deployment version")):
            with self.assertRaisesRegex(helper.verifier.platforms.PlatformError, "missing deployment"):
                helper.build(self.output, self.repo)
        self.assertFalse(any(call[1:]==["--version"] for call in self.tools.calls))
        self.assert_failed_candidate()

    def test_stale_notices_refused_before_native_build(self):
        with patch.object(helper.subprocess, "run", self.tools.run), patch.object(helper.notices, "generate", return_value=b"stale"):
            with self.assertRaisesRegex(helper.BuildError, "NOTICES is stale"):
                helper.build(self.output, self.repo)
        self.assertFalse(any("build" in call for call in self.tools.calls))
        self.assert_failed_candidate()

    def test_informational_dirty_candidate_never_asserts_clean_source(self):
        self.tools.dirty = True
        self.build()
        info = json.loads((self.output / "build-info.json").read_text())
        self.assertTrue(info["source"]["dirty"])
        self.assertIn("source_clean false\n", (self.output / "artifact-info.txt").read_text())
        with self.assertRaises(helper.verifier.VerificationError):
            helper.verifier.verify_archive(self.archive_path(),
                self.output / (self.archive_path().name + ".sha256"), "a" * 40,
                self.tools.package_version, self.tools.host)


class NoticeGenerationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name) / "repo"; self.repo.mkdir()
        self.cache = Path(self.temp.name) / "cache"; (self.cache / "registry").mkdir(parents=True)
        (self.repo / "licenses").mkdir()
        (self.repo / "licenses/provenance.json").write_text('{"schema_version":1,"packages":[]}')
        self.archive = self.cache / "registry/example-1.0.0.crate"
        self.make_crate()

    def make_crate(self, license_text=b"Verbatim synthetic license text\n", vcs_commit=None):
        with tarfile.open(self.archive, "w:gz") as bundle:
            files = {"Cargo.toml": b'[package]\nname="example"\nversion="1.0.0"\nlicense = "MIT"\nrepository = "https://github.com/example/example"\n'}
            if license_text is not None: files["LICENSE"] = license_text
            if vcs_commit is not None:
                files[".cargo_vcs_info.json"] = json.dumps({"git": {"sha1": vcs_commit}}).encode()
            for name, data in files.items():
                member = tarfile.TarInfo("example-1.0.0/" + name); member.size = len(data)
                bundle.addfile(member, io.BytesIO(data))
        checksum = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        (self.repo / "Cargo.lock").write_text('version = 4\n[[package]]\nname = "example"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "' + checksum + '"\n')

    def test_exact_text_and_crate_provenance_retained(self):
        result = helper.notices.generate(self.repo, self.cache)
        self.assertIn(b"Verbatim synthetic license text\n", result)
        self.assertIn(hashlib.sha256(self.archive.read_bytes()).hexdigest().encode(), result)
        self.assertIn(b"conservative superset", result)

    def test_missing_license_and_bad_crate_checksum_block_generation(self):
        self.make_crate(None)
        with self.assertRaisesRegex(helper.notices.NoticeError, "missing upstream license text"):
            helper.notices.generate(self.repo, self.cache)
        self.make_crate()
        self.archive.write_bytes(self.archive.read_bytes() + b"changed")
        with self.assertRaisesRegex(helper.notices.NoticeError, "missing checksum-verified"):
            helper.notices.generate(self.repo, self.cache)

    def supplement(self, license_text=b"Verbatim supplemental license\n"):
        commit = "b" * 40
        self.make_crate(None, vcs_commit=commit)
        filename = "example-1.0.0-LICENSE"
        (self.repo / "licenses" / filename).write_bytes(license_text)
        item = {"package": "example", "version": "1.0.0",
                "crate_sha256": hashlib.sha256(self.archive.read_bytes()).hexdigest(),
                "upstream_commit": commit, "commit_evidence": "archive .cargo_vcs_info.json",
                "files": [{"file": filename, "upstream_path": "LICENSE",
                           "url": "https://raw.githubusercontent.com/example/example/" + commit + "/LICENSE",
                           "sha256": hashlib.sha256(license_text).hexdigest()}]}
        self.write_supplement(item)
        return item

    def write_supplement(self, item):
        (self.repo / "licenses/provenance.json").write_text(json.dumps({"schema_version": 1, "packages": [item]}))

    def test_empty_missing_malformed_and_duplicate_supplement_lists_refuse(self):
        original = self.supplement()
        self.assertIn(b"Verbatim supplemental license", helper.notices.generate(self.repo, self.cache))
        for files in ([], None, "LICENSE", [{}], [original["files"][0], original["files"][0]]):
            with self.subTest(files=files):
                item = dict(original); item["files"] = files; self.write_supplement(item)
                with self.assertRaises(helper.notices.NoticeError): helper.notices.generate(self.repo, self.cache)
        item = dict(original); del item["files"]; self.write_supplement(item)
        with self.assertRaises(helper.notices.NoticeError): helper.notices.generate(self.repo, self.cache)

    def test_empty_or_missing_actual_supplement_license_refuses(self):
        for material in (b"", b" \n\t", b"\x00"):
            with self.subTest(material=material):
                self.supplement(material)
                with self.assertRaises(helper.notices.NoticeError): helper.notices.generate(self.repo, self.cache)
        item = self.supplement(); (self.repo / "licenses" / item["files"][0]["file"]).unlink()
        with self.assertRaises(OSError): helper.notices.generate(self.repo, self.cache)

    def test_supplement_revision_is_bound_to_actual_pinned_crate_metadata(self):
        item = self.supplement()
        item["upstream_commit"] = "c" * 40
        item["files"][0]["url"] = item["files"][0]["url"].replace("b" * 40, "c" * 40)
        self.write_supplement(item)
        with self.assertRaisesRegex(helper.notices.NoticeError, "does not match the pinned crate"):
            helper.notices.generate(self.repo, self.cache)

    def test_cli_missing_license_failure_is_nonzero_without_replacing_notices(self):
        item = self.supplement(); item["files"] = []; self.write_supplement(item)
        notices = self.repo / "THIRD-PARTY-NOTICES"; notices.write_bytes(b"preserve existing notices")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            status = helper.notices.main(["--repo", str(self.repo), "--cache", str(self.cache)])
        self.assertEqual(status, 1)
        self.assertIn("nonempty validated file list", stderr.getvalue())
        self.assertEqual(notices.read_bytes(), b"preserve existing notices")

    def test_explicit_cargo_home_matches_registry_cache_location(self):
        cargo_home = self.repo / "cargo-home"
        shutil.copytree(self.cache, cargo_home / "registry/cache")
        with patch.dict(helper.notices.os.environ, {"CARGO_HOME": str(cargo_home)}):
            self.assertIn(b"Verbatim synthetic license", helper.notices.generate(self.repo))


if __name__ == "__main__":
    unittest.main()
