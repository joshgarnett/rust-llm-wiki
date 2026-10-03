#!/usr/bin/env python3
"""Disposable adapter fixtures; no remote acquisition or product/model execution."""
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("prepare_qasper", Path(__file__).with_name("prepare_qasper.py"))
ADAPTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ADAPTER)


def paper(paper_id):
    fact = "前置 café 🐦 support is exact."
    answer = {"unanswerable": False, "extractive_spans": ["support is exact"], "yes_no": None,
              "free_form_answer": "", "evidence": [fact], "highlighted_evidence": ["café 🐦 support is exact."]}
    return {"title": "Sample " + paper_id, "abstract": "Introductory metadata.",
            "full_text": [{"section_name": "Findings", "paragraphs": ["Distractor paragraph.", fact]}], "figures_and_tables": [],
            "qas": [{"question_id": paper_id + "-q", "question": "What support is exact?",
                     "answers": [{"annotation_id": paper_id + "-a", "answer": answer}]}]}


class AdapterTests(unittest.TestCase):
    def archives(self, root, overrides=None):
        overrides = overrides or {}
        payloads = {
            "qasper-train-v0.3.json": json.dumps({"1901.00001": paper("1901.00001")}).encode(),
            "qasper-dev-v0.3.json": json.dumps({"1901.00002": paper("1901.00002")}).encode(),
            "qasper-test-v0.3.json": json.dumps({"1901.00003": paper("1901.00003")}).encode(),
            "README.md": b"Original train/dev notice.", "README-test.md": b"Original test notice.",
            "qasper_evaluator.py": b"raise RuntimeError('Never execute upstream evaluator')\n",
        }
        payloads.update(overrides)
        pins = {}
        for name, members in ADAPTER.MEMBERS.items():
            with tarfile.open(root / name, "w:gz") as bundle:
                for member in members:
                    raw = payloads[member]
                    info = tarfile.TarInfo(member)
                    info.size = len(raw)
                    bundle.addfile(info, io.BytesIO(raw))
            pins[name] = hashlib.sha256((root / name).read_bytes()).hexdigest()
        return pins

    def prepare(self, root, output, **kwargs):
        with contextlib.redirect_stdout(io.StringIO()):
            return ADAPTER.prepare(root / "qasper-train-dev-v0.3.tgz", root / "qasper-test-and-evaluator-v0.3.tgz",
                                   output, limits=(1, 1, 1), acquired_date="2026-10-03", **kwargs)

    def test_deterministic_byte_alignment_and_notice_preservation(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            pins = self.archives(root)
            with patch.object(ADAPTER, "ARCHIVES", pins):
                self.prepare(root, root / "one")
                self.prepare(root, root / "two")
            self.assertEqual((root / "one/manifest.json").read_bytes(), (root / "two/manifest.json").read_bytes())
            q = json.loads((root / "one/train/questions.jsonl").read_text())
            blob = (root / "one/train/documents/1901.00001.md").read_bytes()
            location = q["reference_answers_any_of"][0]["required_evidence_all_of"][0]["locations_any_of"][0]
            self.assertEqual(blob[location["start_byte"]:location["end_byte"]], location["quote"].encode())
            self.assertNotIn(q["original_question"].encode(), blob)
            self.assertEqual((root / "one/notices/README.md").read_bytes(), b"Original train/dev notice.")
            self.assertFalse((root / "one/qasper_evaluator.py").exists())

    def test_wrong_pin_rejected_before_any_output(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            pins = self.archives(root)
            pins["qasper-train-dev-v0.3.tgz"] = "0" * 64
            with patch.object(ADAPTER, "ARCHIVES", pins), self.assertRaisesRegex(ValueError, "SHA256 mismatch"):
                self.prepare(root, root / "output")
            self.assertFalse((root / "output").exists())

    def test_existing_output_refused_without_changing_it(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            output = root / "output"
            output.mkdir()
            (output / "sentinel").write_bytes(b"preserve")
            with self.assertRaisesRegex(ValueError, "new directory"):
                self.prepare(root, output)
            self.assertEqual((output / "sentinel").read_bytes(), b"preserve")

    def test_split_overlap_rejected_before_output(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            duplicate = json.dumps({"1901.00001": paper("1901.00001")}).encode()
            pins = self.archives(root, {"qasper-dev-v0.3.json": duplicate})
            with patch.object(ADAPTER, "ARCHIVES", pins), self.assertRaisesRegex(ValueError, "overlap"):
                self.prepare(root, root / "output")
            self.assertFalse((root / "output").exists())

    def test_invalid_counts_and_dataset_names(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            for dataset, limits in (("../outside", (1, 1, 1)), ("valid", (1, 0, 1))):
                with self.assertRaises(ValueError):
                    ADAPTER.prepare(root / "missing", root / "missing", root / "output", dataset=dataset, limits=limits)
            self.assertFalse((root / "output").exists())

    def test_bounded_archive_read(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            pins = self.archives(root)
            with patch.object(ADAPTER, "ARCHIVES", pins), patch.object(ADAPTER, "MAX_ARCHIVE_BYTES", 3), self.assertRaisesRegex(ValueError, "bounded size"):
                self.prepare(root, root / "output")
            self.assertFalse((root / "output").exists())

    def test_supplied_card_is_preserved_and_hashed(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            pins = self.archives(root)
            card = root / "card.md"
            card.write_bytes(b"license: cc-by-4.0\nOfficial fixture notice.\n")
            with patch.object(ADAPTER, "ARCHIVES", pins):
                manifest = self.prepare(root, root / "output", dataset_card=card)
            self.assertEqual(manifest["card_sha256"], hashlib.sha256(card.read_bytes()).hexdigest())
            self.assertEqual((root / "output/notices/qasper-dataset-card.md").read_bytes(), card.read_bytes())


if __name__ == "__main__":
    unittest.main()
