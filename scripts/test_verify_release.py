#!/usr/bin/env python3
"""Synthetic archive checks; no foreign executable is extracted or run."""

import contextlib
import hashlib
import gzip
import importlib.util
import io
import json
from pathlib import Path
import shutil
import tarfile
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("verify_release", Path(__file__).with_name("verify_release.py"))
verifier = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verifier)


class VerifyReleaseTests(unittest.TestCase):
    COMMIT = "a" * 40
    VERSION = "0.1.0"

    def setUp(self):
        self.inspection = patch.object(verifier.platforms, "inspect_bytes", side_effect=lambda data, target: {"kind":"macos","minimum_version":"15.0.0"} if "apple" in target else {"kind":"glibc","minimum_version":"2.17"})
        self.inspection.start()
        self.addCleanup(self.inspection.stop)
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
            "schema_version": 2, "platform_requirements": {"kind":"macos","minimum_version":"15.0.0"} if "apple" in target else {"kind":"glibc","minimum_version":"2.17"}, "build_command": ["bazel", "build", "--config=release"], "source": {"commit": self.COMMIT, "dirty": False},
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
        contents.update({"LICENSE": b"MIT fixture license\n", "THIRD-PARTY-NOTICES": b"fixture dependency notices\n", "INSTALL.txt": b"fixture installation instructions\n"})
        contents["artifact-info.txt"] = verifier.artifact_text(contents, self.COMMIT, self.VERSION, target).encode("ascii")
        with tarfile.open(self.archive(target), "w:gz", format=tarfile.USTAR_FORMAT) as bundle:
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

    def test_both_supported_targets_return_only_archive_and_checksum_assets(self):
        assets = self.verify()
        expected = [str(path.resolve()) for target in verifier.TARGETS
                    for path in (self.archive(target), self.checksum(target))]
        self.assertEqual(assets, expected)
        self.assertEqual(len(assets), 4)
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
        with self.assertRaisesRegex(verifier.VerificationError, "two artifact directories"):
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
                with self.assertRaisesRegex(verifier.VerificationError, "eight unique regular root files"):
                    self.verify()

    def test_nonregular_binary_entries_rejected(self):
        target = verifier.TARGETS[0]
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.DIRTYPE, tarfile.FIFOTYPE):
            with self.subTest(kind=kind):
                self.write_artifact(target, binary_type=kind)
                with self.assertRaisesRegex(verifier.VerificationError, "unsupported tar type"):
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

    def test_manifests_are_generated_from_one_verified_object(self):
        destination = Path(self.temp.name) / "manifest output"
        assets = verifier.verify_release(self.root, self.COMMIT, self.VERSION, destination)
        self.assertEqual(len(assets), 4)
        manifest = json.loads((destination / "release-manifest.json").read_text())
        self.assertEqual((destination / "release-manifest.txt").read_text(), verifier.manifest_text(manifest))
        self.assertEqual([record["target"] for record in manifest["targets"]], list(verifier.TARGETS))
        for record in manifest["targets"]:
            archive = self.archive(record["target"])
            self.assertEqual(record["archive_sha256"], verifier.digest(archive.read_bytes()))
            self.assertEqual(record["archive_bytes"], archive.stat().st_size)
            self.assertEqual(record["platform_requirements"], {"kind":"macos","minimum_version":"15.0.0"} if "apple" in record["target"] else {"kind":"glibc","minimum_version":"2.17"})
            self.assertEqual(set(record["files"]), set(verifier.MEMBERS))
        with self.assertRaisesRegex(verifier.VerificationError, "fresh directory"):
            verifier.verify_release(self.root, self.COMMIT, self.VERSION, destination)

    def test_packaged_floor_cannot_disagree_with_consistent_rewritten_metadata(self):
        target = verifier.TARGETS[0]
        self.write_artifact(target, info_updates={"platform_requirements": {"kind":"macos","minimum_version":"26.5.0"}})
        with self.assertRaisesRegex(verifier.VerificationError, "platform requirements disagree"):
            self.verify()

    def test_requirements_missing_invalid_or_for_other_target_are_refused(self):
        target = verifier.TARGETS[0]
        for requirements in (None, {"kind":"glibc","minimum_version":"2.39"},
                             {"kind":"macos","minimum_version":"26.x.0"},
                             {"kind":"macos","minimum_version":"15.0.0","extra":True}):
            with self.subTest(requirements=requirements), self.assertRaises(verifier.platforms.PlatformError):
                self.write_artifact(target, info_updates={"platform_requirements":requirements})

    def test_pax_physical_header_trailing_archive_and_bad_padding_refused(self):
        target = verifier.TARGETS[0]
        archive = self.archive(target)
        original = archive.read_bytes()
        raw = gzip.decompress(original)
        variants = [raw + raw]
        header = bytearray(raw)
        header[156] = ord("x")
        header[148:156] = b"        "
        header[148:156] = ("{:06o}\0 ".format(sum(header[:512]))).encode()
        variants.append(header)
        header = bytearray(raw)
        size = int(raw[124:136].rstrip(b"\x00 "), 8)
        header[512 + size] = 1
        variants.append(header)
        for variant in variants:
            archive.write_bytes(gzip.compress(variant)); self.write_checksum(target)
            with self.assertRaises(verifier.VerificationError): self.verify()

    def test_duplicate_json_keys_are_not_silently_adopted(self):
        with self.assertRaisesRegex(verifier.VerificationError, "duplicate JSON"):
            verifier.strict_json(b'{"ok":false,"ok":true}')


if __name__ == "__main__":
    unittest.main()
