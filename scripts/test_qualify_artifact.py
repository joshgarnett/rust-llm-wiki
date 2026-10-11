"""Archive identity and native qualification mechanics; no Rust builds or real installs."""

import argparse
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import qualify_artifact as qualification
import verify_release as verifier


COMMIT = "a" * 40
VERSION = "0.3.0"
MAC = "aarch64-apple-darwin"
LINUX = "x86_64-unknown-linux-gnu"


def artifact_record(target, archive=b"fixture archive", binary=b"fixture executable"):
    return {"target": target, "archive_sha256": verifier.digest(archive), "archive_bytes": len(archive),
            "binary_sha256": verifier.digest(binary), "binary_bytes": len(binary), "files": {},
            "platform_requirements": {"kind": "macos" if target == MAC else "glibc",
                                      "minimum_version": "15.0.0" if target == MAC else "2.39"}}


def release_manifest(records):
    return {"schema_version": "lwiki.release-manifest.v2", "package_version": VERSION,
            "source_commit": COMMIT, "targets": records}


class ArchiveTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.evidence = self.root / "evidence"
        self.evidence.mkdir()
        self.assets = self.root / "assets"
        self.assets.mkdir()
        self.archive = self.assets / ("lwiki-{}-{}.tar.gz".format(VERSION, MAC))
        self.archive.write_bytes(b"fixture archive")
        self.record = artifact_record(MAC)
        self.manifest = release_manifest([self.record, artifact_record(LINUX)])
        self.save_manifest()
        self.host = {"macos_version": "26.0.0"}

    def save_manifest(self):
        qualification.write_json(self.assets / "release-manifest.json", self.manifest)
        (self.assets / "release-manifest.txt").write_text(verifier.manifest_text(self.manifest))

    def prepare(self):
        with patch.object(verifier, "verify_archive", return_value=self.record), \
                patch.object(verifier, "read_ustar", return_value={"lwiki": b"fixture executable", "build-info.json": b"{}\n"}), \
                patch.object(qualification.platforms, "inspect_file", return_value=self.record["platform_requirements"]):
            return qualification.prepare_archive(self.archive, self.evidence, COMMIT, VERSION, MAC, self.host, self.assets)

    def test_extracts_verified_bytes_and_records_hashes(self):
        archive, binary, record = self.prepare()
        self.assertEqual(binary.read_bytes(), b"fixture executable")
        self.assertEqual(record, self.record)
        self.assertEqual(archive, self.archive.resolve())
        self.assertTrue(binary.stat().st_mode & 0o111)
        qualification.verify_unchanged(binary, archive, record)

    def test_rejects_host_below_artifact_minimum(self):
        self.host["macos_version"] = "14.7.0"
        with self.assertRaisesRegex(qualification.QualificationError, "newer operating system"):
            self.prepare()
        self.assertFalse((self.evidence / "lwiki").exists())

    def test_rejects_selected_manifest_record_change(self):
        self.manifest["targets"] = copy.deepcopy(self.manifest["targets"])
        self.manifest["targets"][0]["binary_sha256"] = "b" * 64
        self.save_manifest()
        with self.assertRaisesRegex(qualification.QualificationError, "does not match"):
            self.prepare()

    def test_rejects_incomplete_or_inconsistent_install_manifest(self):
        self.manifest["targets"].pop()
        self.save_manifest()
        with self.assertRaisesRegex(qualification.QualificationError, "both real"):
            self.prepare()

    def test_rejects_archive_symlink_before_resolving(self):
        link = self.root / "linked.tar.gz"
        link.symlink_to(self.archive)
        self.archive = link
        with self.assertRaisesRegex(qualification.QualificationError, "symlink"):
            self.prepare()

    def test_changed_executable_archive_and_install_are_each_refused(self):
        archive, binary, record = self.prepare()
        installed = self.evidence / "installed"
        installed.write_bytes(binary.read_bytes())
        for selected in (binary, archive, installed):
            with self.subTest(path=selected):
                original = selected.read_bytes()
                selected.write_bytes(original + b"changed")
                with self.assertRaises(qualification.QualificationError):
                    qualification.verify_unchanged(binary, archive, record, installed)
                selected.write_bytes(original)


class HostTests(unittest.TestCase):
    def test_mac_label_does_not_override_actual_os(self):
        with patch.object(qualification.platform, "system", return_value="Darwin"), \
                patch.object(qualification.platform, "machine", return_value="arm64"), \
                patch.object(qualification, "command_text", side_effect=["26.4", "fixture-build"]):
            with self.assertRaisesRegex(qualification.QualificationError, "required macOS version"):
                qualification.host_information(MAC, "xcode-27", 27)

    def test_mac_requires_native_architecture(self):
        with patch.object(qualification.platform, "system", return_value="Darwin"), \
                patch.object(qualification.platform, "machine", return_value="x86_64"):
            with self.assertRaisesRegex(qualification.QualificationError, "Apple Silicon"):
                qualification.host_information(MAC, "macos-15", 15)


class ExistingArchiveTests(unittest.TestCase):
    def test_existing_archive_never_invokes_builder_and_all_cli_suites_pin_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            repo = root / "repo"
            repo.mkdir()
            (repo / "Cargo.toml").write_text('[package]\nversion = "' + VERSION + '"\n')
            evidence = root / "new evidence"
            archive = root / "final.tar.gz"
            archive.write_bytes(b"fixture archive")
            logroot = root / "testlogs"
            fixture = logroot / "retrieval_baseline_test/test.outputs/outputs.zip"
            fixture.parent.mkdir(parents=True)
            fixture.write_bytes(b"fixture outputs")
            record = artifact_record(MAC)
            calls = []

            def run(runner, stage, command, env=None, cwd=None, timeout=5400):
                runner.stage = stage
                calls.append((stage, command))
                path = runner.evidence / (stage + ".log")
                path.write_text("{}\n")
                return path

            def prepare(archive, evidence, *args):
                binary = evidence / "lwiki"
                binary.write_bytes(b"fixture executable")
                return archive, binary, record

            def text(command, cwd=None):
                return str(logroot) if "bazel-testlogs" in command else MAC

            args = argparse.Namespace(evidence=str(evidence), archive=archive, release_dir=root,
                                      runner_label="macos-15", expected_macos_major=15)
            with patch.object(qualification, "source_identity", return_value=COMMIT), \
                    patch.object(qualification, "host_information", return_value={"macos_version": "15.7.0"}), \
                    patch.object(qualification, "command_text", side_effect=text), \
                    patch.object(qualification, "prepare_archive", side_effect=prepare), \
                    patch.object(qualification.Runner, "run", run), \
                    patch.object(qualification, "smoke", return_value=evidence / "lwiki"):
                self.assertEqual(qualification.qualify(args, repo), 0)
            self.assertFalse(any("build_artifact.py" in str(value) for _, command in calls for value in command))
            for stage in ("tests", "release-skill", "artifact-tests"):
                command = next(command for name, command in calls if name == stage)
                self.assertIn("--test_env=LWIKI_QUALIFY_BINARY=" + str(evidence / "lwiki"), command)
                self.assertIn("--nocache_test_results", command)
                self.assertIn("--test_strategy=local", command)
            report = json.loads((evidence / "qualification.json").read_text())
            self.assertEqual(report["status"], "passed")
            self.assertTrue(report["source_clean_after"])
            self.assertEqual(report["binary_sha256_after"], record["binary_sha256"])
            self.assertEqual(report["installed_binary_sha256"], record["binary_sha256"])


class InstalledNoteTests(unittest.TestCase):
    def run_smoke(self, mutation=None):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name).resolve()
        repo = root / "repo"
        (repo / "scripts").mkdir(parents=True)
        evidence = root / "evidence"
        evidence.mkdir()
        assets = root / "assets"
        assets.mkdir()
        for path in (repo / "scripts/install.sh", assets / "install.sh"):
            path.write_text("fixture installer\n")
        binary = evidence / "lwiki"
        binary.write_bytes(b"fixture executable")
        record = artifact_record(MAC)
        calls = []

        def run(runner, stage, command, env=None, cwd=None, timeout=5400):
            runner.stage = stage
            calls.append((stage, command))
            self.assertEqual(set(env), {"PATH", "HOME", "XDG_CONFIG_HOME", "LC_ALL", "TZ"})
            self.assertEqual(env["PATH"], "/usr/bin:/bin")
            output = evidence / (stage + ".log")
            output.write_text("fixture stage\n")
            if stage == "install-preview" and mutation == "preview-write":
                (Path(env["HOME"]) / ".local").mkdir()
            elif stage == "install":
                installed = Path(env["HOME"]) / ".local/bin/lwiki"
                installed.parent.mkdir(parents=True)
                installed.write_bytes(binary.read_bytes() if mutation != "installed-bytes" else b"replaced")
            elif stage == "version":
                output.write_text("lwiki " + VERSION + "\n")
            elif stage not in ("install-preview", "install"):
                data = {}
                if stage.startswith("agent-"):
                    home = Path(env["HOME"])
                    discovery = home / ".agents/skills/llm-wiki"
                    data = {"target": "codex", "scope": "user", "destination": str(discovery),
                            "installed": stage not in ("agent-preview", "agent-remove", "agent-status-removed"),
                            "durable": True, "cleanup_pending": False, "failure": None, "created": [], "changed": False}
                    if stage == "agent-preview":
                        data.update(dry_run=True, applied=False)
                    elif stage == "agent-install":
                        payload = home / ".local/share/lwiki/skills/versions/fixture"
                        payload.mkdir(parents=True)
                        (payload / "SKILL.md").write_text("fixture skill\n")
                        qualification.write_json(payload / "manifest.json", {"schema": "lwiki.skill-manifest.v1", "version": VERSION, "target": "shared"})
                        discovery.symlink_to(payload, target_is_directory=True)
                    elif stage == "agent-install-repeat" and mutation == "agent-repeat":
                        (discovery / "SKILL.md").write_text("changed skill\n")
                    elif stage == "agent-remove":
                        discovery.unlink()
                        if mutation == "agent-unrelated":
                            (discovery.parent / "unrelated.txt").write_text("changed\n")
                        elif mutation == "agent-wiki":
                            (home / "My wiki/notes.md").unlink()
                elif stage == "setup":
                    home = Path(env["HOME"])
                    (home / "My wiki").mkdir()
                    (home / "My wiki/notes.md").write_text("captured note\n")
                    (home / ".config").mkdir()
                    (home / ".config/preferences").write_text("saved settings\n")
                elif stage == "capture":
                    data = {"allocated_ids": {"source": "source-fixture", "revision": "revision-fixture"}}
                elif stage == "search":
                    data = {"hits": [{"excerpt": {"text": "QUAL-731"}}]}
                elif stage == "context":
                    body = (Path(env["HOME"]) / "My meeting notes.md").read_text()
                    span = {"start": 0, "end": len(body.encode("utf-8"))}
                    reference = {"source_id": "source-fixture", "source_revision": "revision-fixture", "span": span}
                    if mutation == "citation":
                        reference["source_revision"] = "another-revision"
                    data = {"text": body, "passages": [{"text": body + ("wrong" if mutation == "passage" else ""),
                            "span": span, "citations": [{"kind": "source", "reference": reference}]}]}
                elif stage == "check":
                    data = {"error_count": 0}
                output.write_text(json.dumps({"ok": True, "meta": {"network_used": False}, "data": data}))
            return output

        runner = qualification.Runner(evidence, repo)
        with patch.object(qualification.Runner, "run", run):
            installed = qualification.smoke(runner, binary, VERSION, assets, record)
        return installed, calls

    def test_installed_note_workflow_uses_private_home_and_exact_citation(self):
        installed, calls = self.run_smoke()
        self.assertTrue(installed.is_file())
        for name, command in calls:
            if name not in ("install-preview", "install"):
                self.assertEqual(command[0], str(installed))
        self.assertIn("--non-interactive", next(command for name, command in calls if name == "setup"))

    def test_wrong_note_revision_or_passage_is_rejected(self):
        for mutation in ("citation", "passage"):
            with self.subTest(mutation=mutation), self.assertRaises(qualification.QualificationError):
                self.run_smoke(mutation)

    def test_preview_writes_and_substituted_install_are_rejected(self):
        for mutation in ("preview-write", "installed-bytes"):
            with self.subTest(mutation=mutation), self.assertRaises(qualification.QualificationError):
                self.run_smoke(mutation)

    def test_agent_repeated_install_and_removal_preserve_user_files(self):
        for mutation in ("agent-repeat", "agent-unrelated", "agent-wiki"):
            with self.subTest(mutation=mutation), self.assertRaises(qualification.QualificationError):
                self.run_smoke(mutation)


if __name__ == "__main__":
    unittest.main()
