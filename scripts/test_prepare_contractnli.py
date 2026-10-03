#!/usr/bin/env python3
"""Offline disposable ContractNLI fixtures; never a classifier quality test."""
import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile


def module(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


ADAPTER = module("prepare_contractnli")
EVALUATOR = module("context_eval")


def fixture():
    text = "前置 café 🐦.\r\nEvidence supports a condition.\nRedundant condition."
    first = text.index("Evidence")
    second = text.index("Redundant")
    annotations = {}
    labels = {}
    for index in range(1, 18):
        key = f"nda-{index}"
        label = ADAPTER.LABELS[(index - 1) % 3]
        labels[key] = {"hypothesis": f"Hypothesis external-only {index}.", "short_description": f"Item {index}"}
        annotations[key] = {"choice": label, "spans": [] if label == "NotMentioned" else [0, 1]}
    document = {"id": 42, "text": text, "file_name": "Original.pdf", "url": "https://example.com/original.pdf",
                "document_type": "search-pdf", "spans": [[first, second - 1], [second, len(text)]],
                "annotation_sets": [{"annotations": annotations}]}
    return {"labels": labels, "documents": [document]}


class ContractAdapterTests(unittest.TestCase):
    def archive(self, root, data=None, extras=None):
        path = root / "input.zip"
        with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
            bundle.writestr(ADAPTER.MEMBERS[0], json.dumps(data or fixture(), ensure_ascii=False).encode())
            for name in ADAPTER.MEMBERS[1:]:
                bundle.writestr(name, ("Preserved " + name).encode())
            # Never open or execute upstream test labels, binaries or evaluators.
            bundle.writestr("contract-nli/test.json", "NOT VALID JSON; DO NOT OPEN")
            bundle.writestr("../outside.py", "raise RuntimeError('Do not execute/extract')")
            for name, content in extras or []:
                bundle.writestr(name, content)
        return path, ADAPTER.sha(path.read_bytes())

    def prepare(self, archive, pin, output):
        with patch.object(ADAPTER, "ARCHIVE_SHA256", pin), contextlib.redirect_stdout(io.StringIO()):
            return ADAPTER.prepare(archive, output, contracts=1, acquired_date="2026-10-03")

    def test_unicode_exact_sources_labels_and_annotation_alternatives(self):
        data = fixture()
        data["documents"][0]["annotation_sets"].append(copy.deepcopy(data["documents"][0]["annotation_sets"][0]))
        data["documents"][0]["annotation_sets"][1]["annotations"]["nda-1"]["spans"] = [1]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive, pin = self.archive(root, data)
            manifest = self.prepare(archive, pin, root / "output")
            _, docs, questions, _ = EVALUATOR.load_dataset(root / "output", ["train"])
            self.assertEqual(len(questions), 17)
            self.assertEqual({q["classification"] for q in questions}, set(ADAPTER.LABELS))
            blob = docs["contractnli:42"]["blob"]
            self.assertEqual(blob, data["documents"][0]["text"].encode())
            self.assertNotIn(b"Hypothesis external-only", blob)
            self.assertEqual(manifest["splits"]["train"]["planned_questions"], 17)
            for q in questions:
                self.assertEqual(q["scope"], "designated_contract")
                self.assertEqual(q["answerability"] == "unanswerable", q["classification"] == "NotMentioned")
                self.assertEqual(len(q["reference_answers_any_of"]), 2)
                for ref in q["reference_answers_any_of"]:
                    for evidence in ref["required_evidence_all_of"]:
                        loc = evidence["locations_any_of"][0]
                        self.assertEqual(blob[loc["start_byte"]:loc["end_byte"]], loc["quote"].encode())
                        self.assertGreater(loc["start_byte"], loc["start_char"])
            q = next(q for q in questions if q["hypothesis_id"] == "nda-1")
            self.assertEqual([len(ref["required_evidence_all_of"]) for ref in q["reference_answers_any_of"]], [2, 1])
            self.assertEqual((root / "output/notices/TERMS").read_bytes(), b"Preserved contract-nli/TERMS")
            self.assertFalse((root / "outside.py").exists())
            self.assertFalse((root / "output/test").exists())

    def test_byte_identical_outputs_independent_of_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive, pin = self.archive(root)
            self.prepare(archive, pin, root / "one")
            self.prepare(archive, pin, root / "two")
            first = {str(p.relative_to(root / "one")): p.read_bytes() for p in (root / "one").rglob("*") if p.is_file()}
            second = {str(p.relative_to(root / "two")): p.read_bytes() for p in (root / "two").rglob("*") if p.is_file()}
            self.assertEqual(first, second)

    def test_span_endpoints_reject_invalid_and_boolean_indices(self):
        text = "é🐦abc"
        offsets = ADAPTER.char_to_byte(text)
        self.assertEqual(offsets, [0, 2, 6, 7, 8, 9])
        for span in ([-1, 1], [0, 6], [2, 1], [1, 1], [True, 3], [0, False], [0, 1, 2]):
            with self.subTest(span=span), self.assertRaises(ValueError):
                ADAPTER.location(text, offsets, span)
        self.assertEqual(ADAPTER.location(text, offsets, [0, 5])["end_byte"], 9)

    def test_mapping_failure_retains_whole_denominator_and_no_manifest(self):
        for change in ("bounds", "index", "missing", "negative", "conflict"):
            data = fixture()
            doc = data["documents"][0]
            answer = doc["annotation_sets"][0]["annotations"]["nda-1"]
            if change == "bounds":
                doc["spans"][0] = [0, len(doc["text"]) + 1]
            elif change == "index":
                answer["spans"] = [False]
            elif change == "missing":
                answer["spans"] = []
            elif change == "negative":
                doc["annotation_sets"][0]["annotations"]["nda-3"]["spans"] = [0]
            else:
                doc["annotation_sets"].append(copy.deepcopy(doc["annotation_sets"][0]))
                doc["annotation_sets"][1]["annotations"]["nda-1"]["choice"] = "Contradiction"
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                archive, pin = self.archive(root, data)
                with self.assertRaisesRegex(ValueError, "retained audit, no usable manifest"):
                    self.prepare(archive, pin, root / "output")
                audit = json.loads((root / "output/preparation-audit.json").read_text())
                self.assertEqual(audit["stats"]["planned_questions"], 17)
                self.assertEqual(audit["stats"]["questions"] + audit["stats"]["failed_mapping_questions"], 17)
                self.assertEqual(len((root / "output/train/all-records.jsonl").read_text().splitlines()), 17)
                self.assertFalse((root / "output/manifest.json").exists())

    def test_archive_tamper_and_bounds_rejected_before_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive, pin = self.archive(root)
            with self.assertRaisesRegex(ValueError, "SHA256 mismatch"):
                self.prepare(archive, "0" * 64, root / "output")
            with patch.object(ADAPTER, "MAX_ARCHIVE_BYTES", 3), self.assertRaisesRegex(ValueError, "bounded size"):
                self.prepare(archive, pin, root / "output")
            with patch.object(ADAPTER, "MAX_MEMBER_BYTES", 3), self.assertRaisesRegex(ValueError, "bounded ZIP"):
                self.prepare(archive, pin, root / "output")
            self.assertFalse((root / "output").exists())

    def test_source_and_label_tamper_detected_by_existing_loader(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive, pin = self.archive(root)
            self.prepare(archive, pin, root / "output")
            for relative in ("train/documents/42.md", "train/questions.jsonl"):
                path = root / "output" / relative
                original = path.read_bytes()
                path.write_bytes(original + b"tamper")
                with self.assertRaisesRegex(ValueError, "hash mismatch"):
                    EVALUATOR.load_dataset(root / "output", ["train"])
                path.write_bytes(original)

    def test_existing_output_and_bad_configuration_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "output"
            output.mkdir()
            (output / "sentinel").write_bytes(b"preserve")
            with self.assertRaisesRegex(ValueError, "new directory"):
                ADAPTER.prepare(root / "absent", output)
            self.assertEqual((output / "sentinel").read_bytes(), b"preserve")
            for kwargs in ({"contracts": 0}, {"contracts": True}, {"dataset": "../unsafe"}, {"acquired_date": "2026-1-1"}):
                with self.subTest(kwargs=kwargs), self.assertRaises(ValueError):
                    ADAPTER.prepare(root / "absent", root / "new", **kwargs)

    def test_unknown_label_rejected_and_duplicate_members_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            data = fixture()
            data["documents"][0]["annotation_sets"][0]["annotations"]["nda-1"]["choice"] = "Neutral"
            archive, pin = self.archive(root, data)
            with self.assertRaisesRegex(ValueError, "three-way label"):
                self.prepare(archive, pin, root / "output")
            with contextlib.redirect_stderr(io.StringIO()):
                archive, pin = self.archive(root, extras=[(ADAPTER.MEMBERS[0], b"{}")])
            with self.assertRaisesRegex(ValueError, "Duplicate ZIP"):
                self.prepare(archive, pin, root / "output")
            self.assertFalse((root / "output").exists())


if __name__ == "__main__":
    unittest.main()
