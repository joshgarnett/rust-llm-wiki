"""The staging gate rejects absent, failed, replaced and wrong-OS native evidence."""

import copy
from pathlib import Path
import tempfile
import unittest

import qualify_artifact as qualification
from test_qualify_artifact import COMMIT, VERSION, MAC, LINUX, artifact_record, release_manifest
import verify_native_qualification as gate
import verify_release as verifier


class NativeGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.assets = self.root / "assets"
        self.assets.mkdir()
        self.evidence = self.root / "native"
        self.evidence.mkdir()
        records = [artifact_record(MAC), artifact_record(LINUX)]
        manifest = release_manifest(records)
        qualification.write_json(self.assets / "release-manifest.json", manifest)
        (self.assets / "release-manifest.txt").write_text(verifier.manifest_text(manifest))
        self.records = {record["target"]: record for record in records}
        self.reports = {}
        for label, (target, system, machine, major) in gate.RUNNERS.items():
            record = self.records[target]
            host = {"runner_label": label, "system": system, "machine": machine}
            if major:
                host["macos_version"] = str(major) + ".0.1"
            else:
                host.update(glibc_version="2.39", os_release='ID=ubuntu\nVERSION_ID="24.04"\n')
            report = {"schema_version": "lwiki.native-qualification.v1", "status": "passed", "host": host,
                      "source_commit": COMMIT, "package_version": VERSION, "source_clean_before": True,
                      "source_clean_after": True, "target": target, "artifact": record, "existing_archive": True,
                      "agent_lifecycle": True,
                      "installed_workflow": True, "binary_sha256_after": record["binary_sha256"],
                      "installed_binary_sha256": record["binary_sha256"], "archive_sha256_after": record["archive_sha256"],
                      "binary_path": "/fixture/lwiki", "installed_binary_path": "/fixture/home/.local/bin/lwiki"}
            self.reports[label] = report
            directory = self.evidence / label
            directory.mkdir()
            self.save(label)
            stages = []
            for name in sorted(gate.REQUIRED_STAGES):
                command = ["fixture"]
                if name in gate.BAZEL_TARGETS:
                    command += [gate.BAZEL_TARGETS[name]]
                if name in ("tests", "release-skill", "artifact-tests"):
                    command += ["--test_env=LWIKI_QUALIFY_BINARY=/fixture/lwiki", "--test_strategy=local", "--nocache_test_results"]
                if name in gate.CLI_COMMANDS:
                    command = [report["installed_binary_path"]] + (["--json", "--offline"] if name != "version" else []) + gate.CLI_COMMANDS[name]
                stages.append({"stage": name, "argv": command, "exit_code": 0})
            with (directory / "stages.jsonl").open("w") as output:
                import json
                for stage in stages:
                    output.write(json.dumps(stage) + "\n")

    def save(self, label):
        qualification.write_json(self.evidence / label / "qualification.json", self.reports[label])

    def verify(self):
        return gate.verify(self.evidence, self.assets, COMMIT, VERSION)

    def test_four_matching_native_rows_pass(self):
        result = self.verify()
        self.assertEqual(result["status"], "passed")
        self.assertEqual({row["runner_label"] for row in result["runners"]}, set(gate.RUNNERS))
        mac = [row for row in result["runners"] if row["target"] == MAC]
        self.assertEqual(len({row["archive_sha256"] for row in mac}), 1)
        self.assertEqual(len({row["binary_sha256"] for row in mac}), 1)

    def test_missing_and_failed_rows_block_staging(self):
        path = self.evidence / "xcode-27/qualification.json"
        original = path.read_bytes()
        path.unlink()
        with self.assertRaisesRegex(qualification.QualificationError, "exactly four"):
            self.verify()
        path.write_bytes(original)
        self.reports["xcode-27"]["status"] = "failed"
        self.save("xcode-27")
        with self.assertRaises(qualification.QualificationError):
            self.verify()

    def test_identity_mutations_each_block_staging(self):
        original = copy.deepcopy(self.reports["macos-26"])
        for field, value in [("binary_sha256_after", "b" * 64), ("archive_sha256_after", "b" * 64),
                             ("installed_binary_sha256", "b" * 64), ("source_commit", "b" * 40),
                             ("source_clean_after", False), ("existing_archive", False), ("installed_workflow", False), ("agent_lifecycle", False)]:
            with self.subTest(field=field):
                self.reports["macos-26"] = dict(original, **{field: value})
                self.save("macos-26")
                with self.assertRaises(qualification.QualificationError):
                    self.verify()

    def test_actual_macos_major_and_duplicate_labels_are_checked(self):
        report = self.reports["xcode-27"]
        report["host"]["macos_version"] = "26.5.0"
        self.save("xcode-27")
        with self.assertRaisesRegex(qualification.QualificationError, "version"):
            self.verify()
        report["host"]["macos_version"] = "27.0.1"
        report["host"]["runner_label"] = "macos-26"
        self.save("xcode-27")
        with self.assertRaises(qualification.QualificationError):
            self.verify()

    def test_missing_suite_override_or_recovery_stage_blocks_staging(self):
        path = self.evidence / "macos-15/stages.jsonl"
        original = path.read_text()
        path.write_text(original.replace("--test_env=LWIKI_QUALIFY_BINARY=/fixture/lwiki", "--unrelated-flag"))
        with self.assertRaisesRegex(qualification.QualificationError, "pin"):
            self.verify()
        path.write_text("\n".join(line for line in original.splitlines() if '"stage": "recover"' not in line) + "\n")
        with self.assertRaisesRegex(qualification.QualificationError, "incomplete"):
            self.verify()

    def test_own_note_commands_must_use_installed_executable(self):
        path = self.evidence / "ubuntu-24.04/stages.jsonl"
        path.write_text(path.read_text().replace("/fixture/home/.local/bin/lwiki", "/fixture/another/lwiki"))
        with self.assertRaisesRegex(qualification.QualificationError, "installed executable"):
            self.verify()


if __name__ == "__main__":
    unittest.main()
