"""The staging gate rejects absent, failed, replaced and wrong-OS native evidence."""

import copy
import json
from pathlib import Path
import tempfile
import unittest

import qualify_artifact as qualification
from test_qualify_artifact import COMMIT, VERSION, MAC, LINUX, artifact_record, release_manifest, compiler_graph
import verify_native_qualification as gate
import verify_release as verifier


class NativeGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.assets = self.root / "assets"
        self.assets.mkdir()
        (self.assets / "install.sh").write_bytes(b"fixture installer\n")
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
            raw = directory / qualification.PROFILE_FILE
            qualification.write_json(raw, compiler_graph())
            report["build_profile"] = qualification.rust_build_profile(raw)
            self.save(label)
            stages = []
            for name in sorted(gate.REQUIRED_STAGES):
                command = ["fixture"]
                if name in gate.BAZEL_TARGETS:
                    command += [gate.BAZEL_TARGETS[name]]
                if name == "compiler-profile":
                    command += qualification.PROFILE_FLAGS
                elif name in ("test-build", "tests", "release-skill", "artifact-tests"):
                    command += ["--nofetch", "--config=release"]
                if name in ("tests", "release-skill", "artifact-tests"):
                    command += ["--test_env=LWIKI_QUALIFY_BINARY=/fixture/lwiki", "--test_env=RUST_TEST_THREADS=1",
                                "--test_strategy=standalone", "--nocache_test_results"]
                if name == "tests":
                    command += ["--zip_undeclared_test_outputs"]
                if name == "release-skill":
                    command += ["--test_arg=--test-threads=1"]
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
        return gate.verify(self.evidence, self.assets, COMMIT, VERSION, verifier_commit="c" * 40)

    def set_mode(self, label, mode):
        report = self.reports[label]
        report.update(schema_version="lwiki.native-qualification.v2", qualification_mode=mode,
                      test_scope=qualification.TEST_SCOPES[mode], harness_commit=COMMIT if mode == "full" else "b" * 40,
                      harness_clean_before=True, harness_clean_after=True,
                      installer_sha256=qualification.digest(self.assets / "install.sh"))
        if mode == "installed-smoke":
            report.pop("build_profile")
            (self.evidence / label / qualification.PROFILE_FILE).unlink()
            path = self.evidence / label / "stages.jsonl"
            stages = [json.loads(line) for line in path.read_text().splitlines()]
            path.write_text("".join(json.dumps(stage) + "\n" for stage in stages if stage['stage'] in gate.INSTALLED_STAGES))
        self.save(label)

    def test_legacy_full_and_new_smoke_combine_under_explicit_policy(self):
        self.set_mode("xcode-27", "installed-smoke")
        result = self.verify()
        self.assertEqual(result['schema_version'], 'lwiki.release-native-qualification.v2')
        self.assertEqual(result['verification_policy'], gate.POLICY)
        self.assertEqual(result['verifier_commit'], 'c' * 40)
        self.assertEqual(result['source_commit'], COMMIT)
        rows = {row['runner_label']: row for row in result['runners']}
        self.assertEqual(rows['macos-26']['required_mode'], 'installed-smoke')
        self.assertEqual(rows['macos-26']['qualification_mode'], 'full')
        self.assertIn('build_profile', rows['macos-26'])
        self.assertEqual(rows['xcode-27']['qualification_mode'], 'installed-smoke')
        self.assertEqual(rows['xcode-27']['harness_commit'], 'b' * 40)
        self.assertNotIn('build_profile', rows['xcode-27'])

    def test_smoke_cannot_replace_either_full_row(self):
        for label in ('ubuntu-24.04', 'macos-15'):
            original_report = copy.deepcopy(self.reports[label])
            original_stages = (self.evidence / label / 'stages.jsonl').read_bytes()
            original_profile = (self.evidence / label / qualification.PROFILE_FILE).read_bytes()
            self.set_mode(label, 'installed-smoke')
            with self.subTest(label=label), self.assertRaisesRegex(qualification.QualificationError, 'full native suites are required'):
                self.verify()
            self.reports[label] = original_report
            self.save(label)
            (self.evidence / label / 'stages.jsonl').write_bytes(original_stages)
            (self.evidence / label / qualification.PROFILE_FILE).write_bytes(original_profile)

    def test_both_newer_mac_smokes_preserve_the_two_full_rows(self):
        self.set_mode('macos-26', 'installed-smoke')
        self.set_mode('xcode-27', 'installed-smoke')
        result = self.verify()
        modes = {row['runner_label']: row['qualification_mode'] for row in result['runners']}
        self.assertEqual(modes, gate.REQUIRED_MODES)
        self.assertEqual(len(gate.REQUIRED_STAGES), 28)
        self.assertEqual(len(gate.INSTALLED_STAGES), 20)

    def test_smoke_requires_exact_host_and_installed_command_identity(self):
        self.set_mode('xcode-27', 'installed-smoke')
        report = self.reports['xcode-27']
        original = copy.deepcopy(report['host'])
        for field, value in [('macos_version', '26.0.1'), ('machine', 'x86_64'), ('runner_label', 'macos-26')]:
            report['host'] = dict(original, **{field: value})
            self.save('xcode-27')
            with self.subTest(field=field), self.assertRaises(qualification.QualificationError):
                self.verify()
        report['host'] = original
        self.save('xcode-27')
        path = self.evidence / 'xcode-27/stages.jsonl'
        path.write_text(path.read_text().replace('/fixture/home/.local/bin/lwiki', '/unqualified/lwiki'))
        with self.assertRaisesRegex(qualification.QualificationError, 'installed executable'):
            self.verify()

    def test_smoke_mode_identity_and_installer_mutations_are_rejected(self):
        self.set_mode('xcode-27', 'installed-smoke')
        original = copy.deepcopy(self.reports['xcode-27'])
        for field, value in [('qualification_mode', 'unknown'), ('qualification_mode', 'full'),
                             ('schema_version', 'lwiki.native-qualification.v1'), ('harness_commit', 'short'),
                             ('harness_clean_before', False), ('harness_clean_after', False), ('test_scope', 'Full suites passed'),
                             ('installer_sha256', 'f' * 64), ('source_commit', 'f' * 40),
                             ('installed_binary_sha256', 'f' * 64), ('archive_sha256_after', 'f' * 64)]:
            self.reports['xcode-27'] = dict(original, **{field: value})
            self.save('xcode-27')
            with self.subTest(field=field, value=value), self.assertRaises((qualification.QualificationError, verifier.VerificationError, OSError)):
                self.verify()

    def test_smoke_exact_stages_reject_missing_extra_duplicate_failed_and_fake_profile(self):
        self.set_mode('xcode-27', 'installed-smoke')
        path = self.evidence / 'xcode-27/stages.jsonl'
        stages = [json.loads(line) for line in path.read_text().splitlines()]
        failed = copy.deepcopy(stages); failed[0]['exit_code'] = 1
        for changed in (stages[1:], stages + [stages[0]], stages + [{'stage': 'tests', 'argv': ['fixture'], 'exit_code': 0}], failed):
            path.write_text(''.join(json.dumps(stage) + '\n' for stage in changed))
            with self.subTest(changed=len(changed)), self.assertRaisesRegex(qualification.QualificationError, 'incomplete'):
                self.verify()
        path.write_text(''.join(json.dumps(stage) + '\n' for stage in stages))
        self.reports['xcode-27']['build_profile'] = {'configuration': 'release'}
        self.save('xcode-27')
        with self.assertRaisesRegex(qualification.QualificationError, 'must not claim compiler'):
            self.verify()

    def test_legacy_missing_full_stages_are_not_inferred_as_smoke(self):
        path = self.evidence / 'macos-26/stages.jsonl'
        stages = [json.loads(line) for line in path.read_text().splitlines()]
        path.write_text(''.join(json.dumps(stage) + '\n' for stage in stages if stage['stage'] in gate.INSTALLED_STAGES))
        with self.assertRaisesRegex(qualification.QualificationError, 'incomplete'):
            self.verify()

    def test_new_full_reports_bind_compiled_harness_to_product_source(self):
        self.set_mode('macos-15', 'full')
        self.verify()
        self.reports['macos-15']['harness_commit'] = 'b' * 40
        self.save('macos-15')
        with self.assertRaisesRegex(qualification.QualificationError, 'product, harness or installer'):
            self.verify()

    def test_four_matching_native_rows_pass(self):
        result = self.verify()
        self.assertEqual(result["status"], "passed")
        self.assertEqual({row["runner_label"] for row in result["runners"]}, set(gate.RUNNERS))
        mac = [row for row in result["runners"] if row["target"] == MAC]
        self.assertEqual(len({row["archive_sha256"] for row in mac}), 1)
        self.assertEqual(len({row["binary_sha256"] for row in mac}), 1)

    def test_appended_release_overrides_block_staging(self):
        import json
        path = self.evidence / "macos-15/stages.jsonl"
        original = path.read_text()
        for name in ("test-build", "tests", "release-skill", "artifact-tests"):
            for extra in (["--compilation_mode=fastbuild"], ["-c", "fastbuild"],
                          ["--config=unreviewed"], ["--config=release"], ["--"],
                          ["--@rules_rust//rust/settings:rustc_flag=-Copt-level=0"]):
                with self.subTest(stage=name, extra=extra):
                    stages = [json.loads(line) for line in original.splitlines()]
                    next(stage for stage in stages if stage["stage"] == name)["argv"] += extra
                    path.write_text("".join(json.dumps(stage) + "\n" for stage in stages))
                    with self.assertRaisesRegex(qualification.QualificationError, "stage options"):
                        self.verify()

    def test_each_runtime_suite_requires_exact_serial_environment_once(self):
        import json
        path = self.evidence / "macos-15/stages.jsonl"
        original = [json.loads(line) for line in path.read_text().splitlines()]
        pin = "--test_env=RUST_TEST_THREADS=1"
        for name in ("tests", "release-skill", "artifact-tests"):
            argv = next(stage for stage in original if stage["stage"] == name)["argv"]
            index = argv.index(pin)
            mutations = {
                "missing": argv[:index] + argv[index + 1:],
                "parallel": argv[:index] + ["--test_env=RUST_TEST_THREADS=2"] + argv[index + 1:],
                "inherited": argv[:index] + ["--test_env=RUST_TEST_THREADS"] + argv[index + 1:],
                "split": argv[:index] + ["--test_env", "RUST_TEST_THREADS=1"] + argv[index + 1:],
                "duplicate": argv + [pin],
                "conflict_after": argv + ["--test_env=RUST_TEST_THREADS=2"],
                "conflict_before": argv[:index] + ["--test_env=RUST_TEST_THREADS=2"] + argv[index:],
                "inherit_after": argv + ["--test_env=RUST_TEST_THREADS"],
            }
            for mutation, arguments in mutations.items():
                with self.subTest(stage=name, mutation=mutation):
                    stages = copy.deepcopy(original)
                    next(stage for stage in stages if stage["stage"] == name)["argv"] = arguments
                    path.write_text("".join(json.dumps(stage) + "\n" for stage in stages))
                    with self.assertRaisesRegex(qualification.QualificationError, "serial harness scheduling|stage options"):
                        self.verify()

    def test_serial_environment_cannot_be_overridden_by_runtime_thread_or_mode_arguments(self):
        import json
        path = self.evidence / "macos-15/stages.jsonl"
        original = [json.loads(line) for line in path.read_text().splitlines()]
        for name in ("tests", "release-skill", "artifact-tests"):
            for extra in (["--test_arg=--test-threads=2"],
                          ["--test_arg=--test-threads", "--test_arg=2"],
                          ["--test_arg=--test-threads=1"],
                          ["--test_arg=--ignored"], ["--test_arg=--include-ignored"],
                          ["--test_arg=--list"], ["--test_arg=--skip=source_relation"],
                          ["--test_arg=source_relation"]):
                with self.subTest(stage=name, extra=extra):
                    stages = copy.deepcopy(original)
                    next(stage for stage in stages if stage["stage"] == name)["argv"] += extra
                    path.write_text("".join(json.dumps(stage) + "\n" for stage in stages))
                    with self.assertRaisesRegex(qualification.QualificationError, "stage options"):
                        self.verify()

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

    def test_invalid_or_unpinned_strategy_blocks_staging(self):
        path = self.evidence / "macos-15/stages.jsonl"
        original = path.read_text()
        for strategy in ["local", "remote"]:
            with self.subTest(strategy=strategy):
                path.write_text(original.replace("--test_strategy=standalone", "--test_strategy=" + strategy))
                with self.assertRaisesRegex(qualification.QualificationError, "pin"):
                    self.verify()

    def test_missing_release_configuration_blocks_staging(self):
        path = self.evidence / "macos-15/stages.jsonl"
        original = path.read_text()
        for stage in ("compiler-profile", "test-build", "tests", "release-skill", "artifact-tests"):
            import json
            rows = [json.loads(line) for line in original.splitlines()]
            next(row for row in rows if row["stage"] == stage)["argv"].remove("--config=release")
            path.write_text("".join(json.dumps(row) + "\n" for row in rows))
            with self.subTest(stage=stage), self.assertRaisesRegex(qualification.QualificationError, "release configuration"):
                self.verify()

    def test_missing_or_declared_only_optimization_provenance_is_rejected(self):
        report = self.reports["macos-26"]
        original = copy.deepcopy(report["build_profile"])
        for value in [None, dict(original, rust_opt_level="0"), dict(original, aquery_sha256="f" * 64)]:
            with self.subTest(value=value):
                report["build_profile"] = value
                self.save("macos-26")
                with self.assertRaisesRegex(qualification.QualificationError, "provenance"):
                    self.verify()
        report["build_profile"] = original
        self.save("macos-26")
        (self.evidence / "macos-26" / qualification.PROFILE_FILE).unlink()
        with self.assertRaises(OSError):
            self.verify()

    def test_raw_unoptimized_arguments_override_a_passing_declaration(self):
        raw = self.evidence / "ubuntu-24.04" / qualification.PROFILE_FILE
        graph = compiler_graph()
        graph["actions"][1]["arguments"].append("-Copt-level=0")
        qualification.write_json(raw, graph)
        with self.assertRaisesRegex(qualification.QualificationError, "opt-level"):
            self.verify()


if __name__ == "__main__":
    unittest.main()
