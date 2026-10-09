"""Tiny fixtures only: no native CLI execution, providers, build or corpus."""
import argparse
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import large_vault_packet as packet


class PacketTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.binary = self.root / "fake-native"
        self.binary.write_bytes(b"unit fixture, never executed")
        self.sha = packet.digest(self.binary)
        self.pin = patch.object(packet, "PIN", self.sha)
        self.pin.start()
        self.addCleanup(self.pin.stop)
        notices = []
        for name in ["README.md", "LICENSE-MIT", "LICENSE-APACHE"]:
            path = self.root / name
            path.write_text("Tiny notice fixture.\n", encoding="utf-8")
            notices.append(str(path))
        sources = []
        for name in ["config.md", "features.md"]:
            path = self.root / name
            path.write_text(f"# {name}\nCafé source only.\n", encoding="utf-8")
            sources.append({"path": str(path), "title": name, "sha256": packet.digest(path), "bytes": path.stat().st_size,
                            "origin": f"https://github.com/rust-lang/cargo/blob/{packet.COMMIT}/src/doc/src/reference/{name}",
                            "commit": packet.COMMIT, "license": "MIT OR Apache-2.0", "notices": notices})
        self.overlay = self.root / "overlay.json"
        packet.write_json(self.overlay, {"schema": 1, "sources": sources})

    def args(self, name="planned", tier="smoke", overlay=True):
        return argparse.Namespace(binary=str(self.binary), binary_sha256=self.sha, account_root=str(self.root),
                                  output=str(self.root / name), tier=tier,
                                  overlay_manifest=str(self.overlay) if overlay else None)

    def test_plan_only_missing_and_spaces(self):
        result = packet.plan(self.args())
        output = self.root / "planned"
        self.assertEqual({p.name for p in output.iterdir()}, {"packet.json", "command-plan.json"})
        self.assertFalse(result["cli_launch_admitted"])
        commands = packet.load_json(output / "command-plan.json")
        self.assertIn("vault with spaces", commands["argv"]["init"][-1])
        self.assertIn("--max-groups", commands["argv"]["resume"])
        self.assertEqual(commands["argv"]["run"][-1], "64")

    def test_unavailable_overlay_no_silent_substitution(self):
        packet.plan(self.args(overlay=False))
        saved = packet.load_json(self.root / "planned" / "packet.json")
        self.assertEqual(saved["overlay"], "UNAVAILABLE")
        with self.assertRaisesRegex(ValueError, "overlay UNAVAILABLE"):
            packet.generate(argparse.Namespace(packet=str(self.root / "planned"), admit_input_generation=True))

    def test_no_overwrite_different_tier(self):
        packet.plan(self.args())
        before = packet.digest(self.root / "planned" / "packet.json")
        with self.assertRaisesRegex(ValueError, "absent"):
            packet.plan(self.args(tier="1000"))
        self.assertEqual(before, packet.digest(self.root / "planned" / "packet.json"))

    def test_binary_pin_and_drift(self):
        args = self.args()
        args.binary_sha256 = "0" * 64
        with self.assertRaisesRegex(ValueError, "accepted native pin"):
            packet.plan(args)
        self.binary.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "binary bytes differ"):
            packet.plan(self.args())

    def test_overlay_sha_and_label_separation(self):
        data = packet.load_json(self.overlay)
        data["sources"][0]["gold"] = "forbidden"
        self.overlay.write_text(json.dumps(data), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "query/gold"):
            packet.plan(self.args())
        del data["sources"][0]["gold"]
        data["sources"][0]["sha256"] = "0" * 64
        self.overlay.write_text(json.dumps(data), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "SHA256 mismatch"):
            packet.plan(self.args())

    def test_distribution_exact_full_overlay_bytes(self):
        for count in [10, 1000, 10000, 25000]:
            bases, targets, q, r = packet.sizes(count, 73091)
            self.assertEqual(len(targets), count - 2)
            self.assertEqual(sum(targets) + 73091, count * 100000)
            self.assertEqual(bases.count(80000), count // 5 - 1)
            self.assertEqual(bases.count(100000), count * 3 // 5)
            self.assertEqual(bases.count(120000), count // 5 - 1)
            self.assertEqual(max(target - base for target, base in zip(targets, bases)) - min(target - base for target, base in zip(targets, bases)), bool(r))

    def test_tiny_deterministic_unicode_variety(self):
        self.assertEqual(set(packet.closing_choices()[1]), set(range(461)))
        for ordinal in range(9):
            for length in [2000, 2049, 3000, 4097]:
                payload = b"".join(packet.payload_chunks(ordinal, length))
                self.assertEqual(len(payload), length)
                self.assertEqual(payload, b"".join(packet.payload_chunks(ordinal, length)))
                self.assertIn("café", payload.decode("utf-8"))
                self.assertIn(b"Distant dispatch:", payload)
        self.assertNotEqual(b"".join(packet.payload_chunks(0, 3000)), b"".join(packet.payload_chunks(1, 3000)))

    def test_finite_admission_and_large_refusal(self):
        sample = {"count": 1000, "content_bytes": 100000000,
                  "input_generation": {"large_tier_missing": ["control1K", "control10K", "physical_forecast"]}}
        self.assertLess(packet.generation_admission(sample, True, 40 * packet.GIB, 0), packet.INPUT_CAP)
        for explicit, free, existing in [(False, 40 * packet.GIB, 0), (True, 32 * packet.GIB, 0), (True, 40 * packet.GIB, 100 * packet.GIB)]:
            with self.assertRaises(ValueError):
                packet.generation_admission(sample, explicit, free, existing)
        sample["count"] = 10000
        with self.assertRaisesRegex(ValueError, "control1K, control10K, physical_forecast"):
            packet.generation_admission(sample, True, 100 * packet.GIB, 0)
        sample["count"] = 1000
        sample["content_bytes"] = packet.INPUT_CAP
        with self.assertRaisesRegex(ValueError, "allocated-byte"):
            packet.generation_admission(sample, True, 100 * packet.GIB, 0)

    def test_frozen_functional_commands(self):
        argv = packet.commands(str(self.binary), self.root / "unlaunched", "1000")["argv"]
        expected = ["--mode", "lexical", "--candidates", "80", "--excerpt-bytes", "1024", "--limit", "5", "--verify-selected", "--no-sync"]
        for name in ["search", "page_search", "history"]:
            self.assertEqual(argv[name][-len(expected):], expected)
        self.assertIn("--stage", argv["refresh"])
        self.assertEqual(argv["refresh_show"][-3:], ["changes", "show", "${REFRESH_CHANGE_ID}"])
        self.assertEqual(argv["refresh_apply"][-3:], ["changes", "apply", "${REFRESH_CHANGE_ID}"])
        self.assertNotIn("--stage", argv["noop"])
        self.assertNotIn("--stage", argv["reactivate_history"])
        self.assertEqual(argv["read"][-2:], ["--max-bytes", "4096"])
        for flag, value in {"--scope": "indexed-documents", "--mode": "lexical", "--candidates": "80", "--excerpt-bytes": "1024", "--max-bytes": "6000", "--max-tokens": "1500", "--verification-max-bytes": "67108864", "--verification-max-files": "4096", "--verification-max-entries": "16384", "--verification-max-elapsed-ms": "2000"}.items():
            self.assertEqual(argv["context"][argv["context"].index(flag) + 1], value)

    def tenk_record(self):
        packet.plan(self.args(tier="10000"))
        output = self.root / "planned"
        paths = {"generator": str(Path(packet.__file__).resolve()), "packet": str(output / "packet.json"),
                 "command_plan": str(output / "command-plan.json"), "account_root": str(self.root), "output": str(output)}
        evidence = {}
        for name in ["prior_1k_preparation_result", "prior_1k_import_result", "prior_1k_lifecycle_acceptance", "critic_review"]:
            path = self.root / (name + ".json")
            path.write_text('{"unit_fixture":true}\n', encoding="utf-8")
            evidence[name] = str(path)
        record = {"schema": 1, "scope": "finite-10k-input-preparation", "root_admitted": True,
                  "paths": paths, "tier": "10000", "count": 10000, "content_bytes": 1000000000,
                  "limits": {"input_allocated_bytes": packet.TENK_INPUT_CAP, "account_allocated_bytes": 100 * packet.GIB,
                             "self_rss_bytes": packet.GIB, "deadline_seconds": 1800, "free_floor_bytes": packet.FREE_FLOOR},
                  "prerequisites": {"prior_1k": "VERIFIED", "critic": "GO"}, "evidence": evidence,
                  "pins": {path: packet.digest(Path(path)) for path in
                           [paths[name] for name in ["generator", "packet", "command_plan"]] + list(evidence.values())}}
        admission = self.root / "admission.json"
        packet.write_json(admission, record)
        args = argparse.Namespace(packet=str(output), tenk_admission=str(admission), tenk_admission_sha256=packet.digest(admission))
        return args, record

    def test_tenk_exact_admission_and_existing_limits(self):
        args, _ = self.tenk_record()
        guard = packet.PreparationGuard()
        self.assertTrue(packet.tenk_admission(args, guard))
        self.assertEqual(guard.input_cap, 2 * packet.GIB)
        _, saved = packet.packet_at(args.packet, guard)
        projection = packet.generation_admission(saved, True, 40 * packet.GIB, 0, tenk=True)
        self.assertGreater(projection, packet.INPUT_CAP)
        self.assertLess(projection, packet.TENK_INPUT_CAP)
        for explicit, free, existing in [(False, 40 * packet.GIB, 0), (True, 32 * packet.GIB, 0), (True, 40 * packet.GIB, 100 * packet.GIB)]:
            with self.assertRaises(ValueError):
                packet.generation_admission(saved, explicit, free, existing, tenk=True)
        with self.assertRaises(ValueError):
            packet.generation_admission(saved, True, 100 * packet.GIB, 0)
        saved.update(tier="25000", count=25000, content_bytes=2500000000)
        with self.assertRaises(ValueError):
            packet.generation_admission(saved, True, 100 * packet.GIB, 0, tenk=True)

    def test_tenk_admission_pin_scope_and_evidence_refusals(self):
        args, record = self.tenk_record()
        path = Path(args.tenk_admission)
        for key, value in [("count", 25000), ("content_bytes", 1000000001), ("root_admitted", False),
                           ("scope", "finite-public-import-control"), ("tier", "25000"),
                           ("prerequisites", {"prior_1k": "VERIFIED", "critic": "PENDING"}),
                           ("limits", dict(record["limits"], input_allocated_bytes=4 * packet.GIB)),
                           ("paths", dict(record["paths"], output=str(self.root / "other"))), ("evidence", {})]:
            altered = dict(record, **{key: value})
            path.write_text(json.dumps(altered), encoding="utf-8")
            args.tenk_admission_sha256 = packet.digest(path)
            guard = packet.PreparationGuard()
            with self.assertRaises(ValueError):
                packet.tenk_admission(args, guard)
            self.assertEqual(guard.input_cap, packet.INPUT_CAP)
        path.write_text(json.dumps(record), encoding="utf-8")
        args.tenk_admission_sha256 = "0" * 64
        with self.assertRaisesRegex(ValueError, "SHA256 mismatch"):
            packet.tenk_admission(args, packet.PreparationGuard())
        args.tenk_admission_sha256 = packet.digest(path)
        Path(record["evidence"]["critic_review"]).write_text("drift", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "evidence pin"):
            packet.tenk_admission(args, packet.PreparationGuard())

    def test_tenk_admission_requires_complete_interface_and_guard_ceiling(self):
        self.assertFalse(packet.tenk_admission(argparse.Namespace(packet="unused"), packet.PreparationGuard()))
        for args in [argparse.Namespace(tenk_admission="unused"), argparse.Namespace(tenk_admission_sha256="0" * 64)]:
            with self.assertRaisesRegex(ValueError, "supplied together"):
                packet.tenk_admission(args, packet.PreparationGuard())
        args, _ = self.tenk_record()
        guard = packet.PreparationGuard()
        packet.tenk_admission(args, guard)
        guard.output, guard.account = self.root / "planned", self.root
        with patch.object(packet, "allocation", return_value=packet.TENK_INPUT_CAP + 1), patch.object(packet.shutil, "disk_usage", return_value=type("Disk", (), {"free": 40 * packet.GIB})()):
            with self.assertRaisesRegex(ValueError, "input allocated"):
                guard.check(force=True)

    def test_real_adjusted_payload_samples_in_memory(self):
        bases, targets, _, _ = packet.sizes(1000, 73091)
        for ordinal in [0, 198, 199, 798, 799, 997]:
            payload = b"".join(packet.payload_chunks(ordinal, targets[ordinal]))
            self.assertEqual(len(payload), targets[ordinal])
            text = payload.decode("utf-8", errors="strict")
            self.assertIn("Early dispatch:", text)
            self.assertIn("Distant dispatch:", text)
            self.assertIn("Closing review 19:", text)
            self.assertIn("café", text)
            self.assertEqual(payload, b"".join(packet.payload_chunks(ordinal, targets[ordinal])))
        self.assertEqual({bases[i] for i in [0, 198, 199, 798, 799, 997]}, {80000, 100000, 120000})
    def tiny_generated(self):
        # Use real plan/pin/commands; only reduce synthetic test byte targets.
        packet.plan(self.args())
        output = self.root / "planned"
        data = packet.load_json(output / "packet.json")
        original_packet_at = packet.packet_at
        def tiny_packet_at(path, guard=None):
            directory, result = original_packet_at(path, guard)
            result["content_bytes"] = 8 * 3000 + sum(e["bytes"] for e in result["overlay"])
            return directory, result
        original_sizes = packet.sizes
        def tiny_sizes(count, public_bytes):
            bases, _, q, r = original_sizes(count, public_bytes)
            return bases, [3000] * len(bases), q, r
        patches = [patch.object(packet, "packet_at", side_effect=tiny_packet_at), patch.object(packet, "sizes", side_effect=tiny_sizes),
                   patch.object(packet.shutil, "disk_usage", return_value=type("Disk", (), {"free": 40 * packet.GIB})())]
        for item in patches:
            item.start()
            self.addCleanup(item.stop)
        result = packet.generate(argparse.Namespace(packet=str(output), admit_input_generation=True))
        self.assertTrue(result["verified"])
        self.assertLess(result["content_bytes"], 25000)
        return output

    def test_tiny_generation_verify_and_no_overwrite(self):
        output = self.tiny_generated()
        self.assertTrue(packet.verify(argparse.Namespace(packet=str(output)))["verified"])
        with self.assertRaisesRegex(ValueError, "never overwrites"):
            packet.generate(argparse.Namespace(packet=str(output), admit_input_generation=True))

    def test_verify_deadline_during_regeneration(self):
        output = self.tiny_generated()
        now = [0]
        original = packet.payload_chunks
        def expires(ordinal, target):
            for chunk in original(ordinal, target):
                yield chunk
                now[0] = 1801
        args = argparse.Namespace(packet=str(output))
        with patch.object(packet.time, "monotonic", side_effect=lambda: now[0]), patch.object(packet, "payload_chunks", side_effect=expires):
            with self.assertRaisesRegex(ValueError, "deadline"):
                packet.verify(args)
        self.assertEqual(args._preparation_guard.observed["whole_elapsed_seconds"], 1801)
        self.assertTrue((output / "sources" / "source-000009.md").exists())

    def test_generation_verification_uses_original_clock(self):
        output = self.tiny_generated()
        # Keep original fixture bytes; use a separately fresh planned directory.
        packet.plan(self.args(name="second"))
        now = [0]
        original_verify = packet.verify
        def expires_in_verification(args, guard=None):
            self.assertIsNotNone(guard)
            self.assertEqual(guard.started, 0)
            now[0] = 1801
            return original_verify(args, guard)
        args = argparse.Namespace(packet=str(self.root / "second"), admit_input_generation=True)
        with patch.object(packet.time, "monotonic", side_effect=lambda: now[0]), patch.object(packet, "verify", side_effect=expires_in_verification):
            with self.assertRaisesRegex(ValueError, "deadline"):
                packet.generate(args)
        self.assertTrue((self.root / "second" / "inventory.jsonl").exists())
        with self.assertRaisesRegex(ValueError, "never overwrites"):
            packet.generate(args)

    def test_verify_free_allocation_and_rss_limits(self):
        output = self.tiny_generated()
        args = argparse.Namespace(packet=str(output))
        with patch.object(packet.shutil, "disk_usage", return_value=type("Disk", (), {"free": packet.FREE_FLOOR - 1})()):
            with self.assertRaisesRegex(ValueError, "free floor"):
                packet.verify(args)
        for too_large, expected in [(packet.INPUT_CAP + 1, "input allocated"), (100 * packet.GIB + 1, "input allocated")]:
            with patch.object(packet, "allocation", return_value=too_large):
                with self.assertRaisesRegex(ValueError, expected):
                    packet.verify(args)
        def huge_account(root, check=None):
            return 100 * packet.GIB + 1 if root == self.root else 100
        with patch.object(packet, "allocation", side_effect=huge_account):
            with self.assertRaisesRegex(ValueError, "physical account"):
                packet.verify(args)
        with patch.object(packet.platform, "system", return_value="Darwin"), patch.object(packet.resource, "getrusage", return_value=type("Usage", (), {"ru_maxrss": packet.GIB + 1})()):
            with self.assertRaisesRegex(ValueError, "self RSS"):
                packet.verify(args)

    def test_whole_interval_observations_include_final_verification(self):
        output = self.tiny_generated()
        result = packet.verify(argparse.Namespace(packet=str(output)))
        observed = result["observations"]
        self.assertGreaterEqual(observed["allocation_sample_count"], 2)
        self.assertGreater(observed["self_native_peak_rss_bytes"], 0)
        self.assertGreater(observed["input_peak_allocated_bytes_observed"], 0)
        self.assertGreater(observed["account_peak_allocated_bytes_observed"], observed["input_peak_allocated_bytes_observed"])
        self.assertGreaterEqual(observed["minimum_host_free_bytes_observed"], packet.FREE_FLOOR)
        self.assertGreaterEqual(observed["whole_elapsed_seconds"], 0)

    def test_content_tamper(self):
        output = self.tiny_generated()
        path = output / "sources" / "source-000002.md"
        payload = path.read_bytes()
        path.write_bytes(b"x" + payload[1:])
        with self.assertRaisesRegex(ValueError, "bytes/hash mismatch"):
            packet.verify(argparse.Namespace(packet=str(output)))

    def test_membership_and_input_count(self):
        output = self.tiny_generated()
        extra = output / "sources" / "extra.md"
        extra.write_text("not admitted", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "membership"):
            packet.verify(argparse.Namespace(packet=str(output)))
        extra.unlink()
        with (output / "inputs.jsonl").open("a", encoding="utf-8") as stream:
            stream.write('{}\n')
        with self.assertRaisesRegex(ValueError, "count mismatch"):
            packet.verify(argparse.Namespace(packet=str(output)))

    def test_labels_and_packet_count_tamper(self):
        output = self.tiny_generated()
        labels = output / "answers.json"
        labels.write_text("{}", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "labels/answers forbidden"):
            packet.verify(argparse.Namespace(packet=str(output)))
        labels.unlink()
        data = packet.load_json(output / "packet.json")
        data["count"] = 11
        (output / "packet.json").write_text(json.dumps(data), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "count/tier"):
            packet.verify(argparse.Namespace(packet=str(output)))


if __name__ == "__main__":
    unittest.main()
