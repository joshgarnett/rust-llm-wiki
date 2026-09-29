#!/usr/bin/env python3
"""Synthetic archive checks; no foreign executable is extracted or run."""

import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import tarfile
import tempfile
import unittest


SPEC = importlib.util.spec_from_file_location("verify_release", Path(__file__).with_name("verify_release.py"))
verifier = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verifier)


class VerifyReleaseTests(unittest.TestCase):
    COMMIT = "a" * 40
    VERSION = "0.1.0"

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "downloaded artifacts café"
        self.root.mkdir()
        for target in verifier.TARGETS:
            self.write_artifact(target)

    def directory(self, target):
        return self.root / ("lwiki-" + target + "-" + self.COMMIT)

    def archive(self, target):
        return self.directory(target) / ("lwiki-" + self.VERSION + "-" + target + ".tar.gz")

    def checksum(self, target):
        return self.directory(target) / (self.archive(target).name + ".sha256")

    def write_artifact(self, target, info_updates=None, source_updates=None,
                       capabilities=None, version_text=None, extra_member=None, binary_type=None,
                       binary_mode=None):
        directory = self.directory(target)
        directory.mkdir(exist_ok=True)
        binary = "lwiki.exe" if "windows" in target else "lwiki"
        executable = ("synthetic executable for " + target).encode("utf-8")
        info = {
            "schema_version": 1, "source": {"commit": self.COMMIT, "dirty": False},
            "host": target, "target": target, "package_version": self.VERSION,
            "binary": binary, "binary_sha256": hashlib.sha256(executable).hexdigest(),
        }
        info.update(info_updates or {})
        info["source"].update(source_updates or {})
        contents = {
            binary: executable,
            "version.txt": (version_text or "lwiki " + self.VERSION + "\n").encode("utf-8"),
            "capabilities.json": json.dumps(capabilities if capabilities is not None else {
                "ok": True, "command": "capabilities", "schema_version": "1",
            }).encode("utf-8"),
            "build-info.json": json.dumps(info).encode("utf-8"),
        }
        with tarfile.open(self.archive(target), "w:gz") as bundle:
            for name, data in contents.items():
                member = tarfile.TarInfo(name)
                member.size = len(data)
                if name == binary:
                    member.mode = binary_mode if binary_mode is not None else (0o644 if "windows" in target else 0o755)
                if name == binary and binary_type is not None:
                    member.type = binary_type
                    member.size = 0
                    member.linkname = "outside"
                    bundle.addfile(member)
                else:
                    bundle.addfile(member, io.BytesIO(data))
            if extra_member is not None:
                member = tarfile.TarInfo(extra_member)
                member.size = 1
                bundle.addfile(member, io.BytesIO(b"x"))
        self.write_checksum(target)
        (directory / "bazel-build.log").write_text("synthetic diagnostic\n", encoding="utf-8")

    def write_checksum(self, target):
        archive = self.archive(target)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.checksum(target).write_text(digest + "  " + archive.name + "\n", encoding="utf-8")

    def verify(self):
        return verifier.verify_release(self.root, self.COMMIT, self.VERSION)

    def test_all_six_targets_return_only_archive_and_checksum_assets(self):
        assets = self.verify()
        expected = [str(path.resolve()) for target in verifier.TARGETS
                    for path in (self.archive(target), self.checksum(target))]
        self.assertEqual(assets, expected)
        self.assertEqual(len(assets), 12)
        self.assertFalse(any(path.name in {"lwiki", "lwiki.exe"} for path in self.root.rglob("*")))

    def test_cli_emits_json_only_after_all_targets_pass(self):
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            result = verifier.main([str(self.root), "--commit", self.COMMIT, "--version", self.VERSION])
        self.assertEqual(result, 0)
        self.assertEqual(json.loads(stdout.getvalue()), self.verify())
        self.assertEqual(stderr.getvalue(), "")

    def test_missing_platform_rejected(self):
        shutil.rmtree(self.directory(verifier.TARGETS[-1]))
        with self.assertRaisesRegex(verifier.VerificationError, "six artifact directories"):
            self.verify()

    def test_duplicate_platform_rejected(self):
        target, replaced = verifier.TARGETS[:2]
        shutil.rmtree(self.directory(replaced))
        shutil.copytree(self.directory(target), self.root / "duplicate artifact")
        with self.assertRaisesRegex(verifier.VerificationError, "duplicate native target"):
            self.verify()

    def test_extra_archive_rejected(self):
        target = verifier.TARGETS[0]
        shutil.copy2(self.archive(target), self.directory(target) / "extra.tar.gz")
        with self.assertRaisesRegex(verifier.VerificationError, "one expected native archive"):
            self.verify()

    def test_extra_checksum_rejected(self):
        target = verifier.TARGETS[0]
        (self.directory(target) / "extra.sha256").write_text("extra\n", encoding="utf-8")
        with self.assertRaisesRegex(verifier.VerificationError, "exactly its archive checksum"):
            self.verify()

    def test_checksum_filename_and_digest_must_match(self):
        target = verifier.TARGETS[0]
        original = self.checksum(target).read_text(encoding="utf-8")
        for value, error in ((original.replace(self.archive(target).name, "other.tar.gz"), "name exactly"),
                             ("0" * 64 + "  " + self.archive(target).name + "\n", "checksum mismatch")):
            with self.subTest(value=value):
                self.checksum(target).write_text(value, encoding="utf-8")
                with self.assertRaisesRegex(verifier.VerificationError, error):
                    self.verify()

    def test_dirty_wrong_commit_target_version_and_binary_hash_rejected(self):
        target = verifier.TARGETS[0]
        cases = (
            ({}, {"dirty": True}, "clean expected commit"),
            ({}, {"dirty": 0}, "clean expected commit"),
            ({}, {"commit": "b" * 40}, "clean expected commit"),
            ({"target": verifier.TARGETS[1]}, {}, "target or binary mismatch"),
            ({"host": verifier.TARGETS[1]}, {}, "target or binary mismatch"),
            ({"package_version": "0.0.0"}, {}, "version, target"),
            ({"binary_sha256": "0" * 64}, {}, "binary checksum mismatch"),
        )
        for info, source, error in cases:
            with self.subTest(info=info, source=source):
                self.write_artifact(target, info_updates=info, source_updates=source)
                with self.assertRaisesRegex(verifier.VerificationError, error):
                    self.verify()

    def test_malicious_extra_duplicate_and_traversal_members_rejected(self):
        target = verifier.TARGETS[0]
        for name in ("lwiki", "../outside", "/absolute", "nested/file"):
            with self.subTest(name=name):
                self.write_artifact(target, extra_member=name)
                with self.assertRaisesRegex(verifier.VerificationError, "four unique regular files"):
                    self.verify()

    def test_nonregular_binary_entries_rejected(self):
        target = verifier.TARGETS[0]
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.DIRTYPE, tarfile.FIFOTYPE):
            with self.subTest(kind=kind):
                self.write_artifact(target, binary_type=kind)
                with self.assertRaisesRegex(verifier.VerificationError, "four unique regular files"):
                    self.verify()

    def test_unix_binary_without_execute_bit_rejected(self):
        target = verifier.TARGETS[0]
        self.write_artifact(target, binary_mode=0o644)
        with self.assertRaisesRegex(verifier.VerificationError, "lacks executable permissions"):
            self.verify()

    def test_wrong_version_and_unsuccessful_capabilities_rejected(self):
        target = verifier.TARGETS[0]
        for kwargs, error in (({"version_text": "lwiki 0.0.0\n"}, "version output mismatch"),
                              ({"capabilities": {"ok": False, "command": "capabilities"}}, "unsuccessful capabilities"),
                              ({"capabilities": {"ok": True, "command": "other"}}, "unsuccessful capabilities")):
            with self.subTest(kwargs=kwargs):
                self.write_artifact(target, **kwargs)
                with self.assertRaisesRegex(verifier.VerificationError, error):
                    self.verify()

    def test_failure_cli_emits_no_asset_list(self):
        target = verifier.TARGETS[0]
        self.write_artifact(target, source_updates={"dirty": True})
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            result = verifier.main([str(self.root), "--commit", self.COMMIT, "--version", self.VERSION])
        self.assertEqual(result, 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("release verification failed", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
