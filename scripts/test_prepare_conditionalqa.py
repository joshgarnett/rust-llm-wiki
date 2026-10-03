#!/usr/bin/env python3
"""Disposable ConditionalQA adapter fixtures; no providers or product execution."""
import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


def module(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


ADAPTER = module("prepare_conditionalqa")
EVALUATOR = module("context_eval")
FACT = "<p>前置 café 🐦 eligible.</p>"
CONDITION = "<li>You must be 18 or over.</li>"


def fixtures():
    documents = [{"title": "Sample source " + str(i), "url": "https://www.gov.uk/fixture-" + str(i),
                  "contents": ["<h1>Source overview</h1>", FACT, CONDITION, FACT]} for i in range(3)]
    questions = []
    for i in range(3):
        questions.append({"id": "train-" + str(i), "url": documents[i]["url"],
                          "scenario": "I am currently 19 and live in Wales.", "question": "What applies to me?",
                          "not_answerable": i == 2,
                          "answers": [] if i == 2 else [["eligible", [CONDITION] if i == 0 else []]],
                          "evidences": [] if i == 2 else [FACT]})
    return questions, documents


class AdapterTests(unittest.TestCase):
    def inputs(self, root, questions=None, documents=None):
        default_questions, default_documents = fixtures()
        payloads = {"conditionalqa-v1_0-train.json": ADAPTER.encoded(default_questions if questions is None else questions),
                    "conditionalqa-v1_0-documents.json": ADAPTER.encoded(default_documents if documents is None else documents),
                    "conditionalqa-README.md": b"Original research-use notice.\n",
                    "conditionalqa-LISCENSE": b"BSD-2-Clause original software copyright fixture.\n"}
        for name, blob in payloads.items():
            (root / name).write_bytes(blob)
        (root / "conditionalqa-project.html").write_bytes(b'ConditionalQA <a href="https://creativecommons.org/licenses/by-sa/4.0/">CC BY-SA 4.0</a>')
        return {name: ADAPTER.sha(blob) for name, blob in payloads.items()}

    def prepare(self, root, name="output", **kwargs):
        with contextlib.redirect_stdout(io.StringIO()):
            return ADAPTER.prepare(root / "conditionalqa-v1_0-train.json", root / "conditionalqa-v1_0-documents.json",
                                   root / name, counts=(1, 1, 1), acquired_date="2026-10-03", notices_dir=root, **kwargs)

    def test_exact_unicode_repeated_spans_scenario_conditions_and_unanswerability(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            with patch.object(ADAPTER, "PINS", self.inputs(root)):
                self.prepare(root)
            manifest, docs, questions, _ = EVALUATOR.load_dataset(root / "output", ["train"])
            self.assertEqual(manifest["splits"]["train"]["selected_questions"], 3)
            positive = next(q for q in questions if q["stratum"] == "with_conditions")
            self.assertIn(positive["scenario"], positive["query"])
            self.assertIn(positive["upstream_question"], positive["query"])
            self.assertEqual(positive["original_question"], positive["query"])
            ref = positive["reference_answers_any_of"][0]
            condition_group = ref["answer_condition_associations"][0]["conditions_all_of"][0]
            self.assertEqual(condition_group["upstream_quote"], CONDITION)
            self.assertEqual(condition_group["quote"], "- You must be 18 or over.")
            blob = docs[positive["doc_id"]]["blob"]
            self.assertNotIn(positive["scenario"].encode(), blob)
            self.assertEqual(len(ref["required_evidence_all_of"][0]["locations_any_of"]), 2)
            for group in ref["required_evidence_all_of"]:
                for location in group["locations_any_of"]:
                    EVALUATOR.validate_location(blob, location)
            condition_start = blob.index(condition_group["quote"].encode())
            self.assertNotEqual(condition_start, blob.decode().index(condition_group["quote"]))
            negative = next(q for q in questions if q["answerability"] == "unanswerable")
            self.assertEqual(negative["reference_answers_any_of"][0]["answer"]["upstream_answers"], [])
            self.assertEqual(negative["reference_answers_any_of"][0]["required_evidence_all_of"], [])
            self.assertEqual(negative["scope"], "designated_document")
            self.assertEqual((root / "output/upstream/conditionalqa-v1_0-documents.json").read_bytes(),
                             (root / "conditionalqa-v1_0-documents.json").read_bytes())
            maps = EVALUATOR.jsonl(root / "output/train/source-map.jsonl")
            upstream = json.loads((root / "conditionalqa-v1_0-documents.json").read_bytes())
            for record in maps:
                for entry in record["contents"]:
                    text = upstream[record["upstream_document_index"]]["contents"][entry["content_index"]]
                    self.assertEqual(entry["upstream_text"], text)
                    self.assertEqual(entry["upstream_text_sha256"], ADAPTER.sha(text.encode()))
                    self.assertEqual(docs[record["doc_id"]]["blob"][entry["start_byte"]:entry["end_byte"]], entry["rendered_text"].encode())

    def test_actual_conditionalqa_html_block_format_becomes_markdown_structure(self):
        # Original source structure from pay-self-assessment-tax-bill, one of
        # the 24 actual records whose v1 CLI run scanned bytes but found 0 blocks.
        document = {"contents": ["<h1>Overview</h1>", "<p>The deadlines for paying your tax bill are usually:</p>",
                                  "<li>31 January - for any tax you owe for the previous tax year (known as a balancing payment) and your first payment on account</li>",
                                  "<li>31 July for your second payment on account</li>"]}
        blob, entries = ADAPTER.render(document)
        expected = ("# Overview\n\nThe deadlines for paying your tax bill are usually:\n\n"
                    "- 31 January - for any tax you owe for the previous tax year (known as a balancing payment) and your first payment on account\n\n"
                    "- 31 July for your second payment on account\n")
        self.assertEqual(blob, expected.encode())
        self.assertNotIn(b"<p>", blob)
        self.assertNotIn(b"<li>", blob)
        for fragment, entry in zip(document["contents"], entries):
            rendered = ADAPTER.render_fragment(fragment)
            self.assertEqual(blob[entry["start_byte"]:entry["end_byte"]].decode(), rendered)
            self.assertTrue(ADAPTER.locate(blob, rendered))

    def test_entities_unicode_links_and_literal_markdown_are_preserved_readably(self):
        fragment = '<p>café &amp; &#x1F426; &lt;18 &gt;3 &nbsp; conditions <a href="https://example.test/a_(b)?x=1&amp;y=2">read [guidance]</a>.</p>'
        expected = 'café & 🐦 \\<18 \\>3 \u00a0 conditions [read \\[guidance\\]](https://example.test/a_%28b%29?x=1&y=2).'
        self.assertEqual(ADAPTER.render_fragment(fragment), expected)
        blob, entries = ADAPTER.render({"contents": [fragment, fragment]})
        locations = ADAPTER.locate(blob, expected)
        self.assertEqual(len(locations), 2)
        for location in locations:
            EVALUATOR.validate_location(blob, location)
        self.assertEqual(entries[0]["upstream_text"], fragment)
        self.assertEqual(ADAPTER.render_fragment("<p># literal [text] *value* &amp;lt;raw&amp;gt;</p>"), "\\# literal \\[text\\] \\*value\\* &lt;raw&gt;")
        self.assertEqual(ADAPTER.render_fragment("<p>- literal\n1. ordered-looking</p>"), "\\- literal\n1\\. ordered-looking")

    def test_lists_headings_inline_code_breaks_and_table_rows(self):
        self.assertEqual(ADAPTER.render_fragment("<h3>Age &amp; eligibility</h3>"), "### Age & eligibility")
        self.assertEqual(ADAPTER.render_fragment('<ul><li>First <strong>rule</strong></li><li>Second <a href="/guidance">step</a></li></ul>'), '- First **rule**\n- Second [step](/guidance)')
        self.assertEqual(ADAPTER.render_fragment("<ol><li>One</li><li>Two</li></ol>"), "1. One\n2. Two")
        self.assertEqual(ADAPTER.render_fragment("<ul><li>Outer<ul><li>Inner</li></ul></li></ul>"), "- Outer\n  - Inner")
        self.assertEqual(ADAPTER.render_fragment("<p>Use <code>a`b</code><br>Next</p>"), "Use ``a`b``  \nNext")
        self.assertEqual(ADAPTER.render_fragment("<tr>Rate | £2 |</tr>"), "Rate | £2 |")
        self.assertEqual(ADAPTER.render_fragment("<table><tr><td>Rate</td><td>£2</td></tr></table>"), "Rate | £2")

    def test_unsupported_or_malformed_source_markup_fails_with_audit(self):
        for fragment in ("<script>hide evidence</script>", "<p>unclosed", "<p>mismatched</li>", "<p>text<!-- hidden --></p>", "<![if IE]>hidden<![endif]>", "<div>" * 65 + "text" + "</div>" * 65):
            with self.subTest(fragment=fragment), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                questions, documents = fixtures()
                documents[0]["contents"][0] = fragment
                with patch.object(ADAPTER, "PINS", self.inputs(root, questions, documents)), self.assertRaisesRegex(ValueError, "no usable manifest"):
                    self.prepare(root)
                audit = json.loads((root / "output/preparation-audit.json").read_bytes())
                self.assertEqual(audit["stats"]["source_render_errors"], 1)
                self.assertEqual(audit["stats"]["selected_questions"], 3)
                self.assertEqual(audit["source_render_errors"][0]["upstream_document"], documents[0])
                self.assertEqual(EVALUATOR.jsonl(root / "output/train/excluded.jsonl")[0]["issues"][0]["reason"], "source_document_render_failed")
                self.assertFalse((root / "output/manifest.json").exists())

    def test_long_scenario_query_is_never_truncated(self):
        question, document = fixtures()[0][0], fixtures()[1][0]
        question["scenario"] = " ".join("scenario" + str(i) for i in range(80))
        row, errors = ADAPTER.adapt_question(question, ADAPTER.render(document)[0], "fixture")
        self.assertEqual(errors, [])
        self.assertIn(question["scenario"], row["query"])
        self.assertIn("scenario79", row["query"])
        self.assertGreater(row["query_input"]["whitespace_terms"], 64)
        self.assertFalse(row["query_input"]["truncated"])
        self.assertEqual(row["query_input"]["utf8_bytes"], len(row["query"].encode()))

    def test_multiple_answers_are_one_complete_set_not_false_alternatives(self):
        questions, _ = fixtures()
        questions[0]["answers"].append(["an additional required step", [FACT]])
        blob, _ = ADAPTER.render(fixtures()[1][0])
        row, errors = ADAPTER.adapt_question(questions[0], blob, "fixture")
        self.assertEqual(errors, [])
        self.assertEqual(len(row["reference_answers_any_of"]), 1)
        self.assertEqual(len(row["reference_answers_any_of"][0]["answer_condition_associations"]), 2)
        self.assertEqual(row["upstream_record"], questions[0])

    def test_unmapped_evidence_and_condition_fail_with_full_audit_without_manifest(self):
        for field in ("evidence", "condition"):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                questions, _ = fixtures()
                if field == "evidence":
                    questions[0]["evidences"].append("Absent evidence")
                else:
                    questions[0]["answers"][0][1].append("Absent condition")
                with patch.object(ADAPTER, "PINS", self.inputs(root, questions=questions)), self.assertRaisesRegex(ValueError, "no usable manifest"):
                    self.prepare(root)
                self.assertFalse((root / "output/manifest.json").exists())
                audit = json.loads((root / "output/preparation-audit.json").read_bytes())
                self.assertEqual(audit["stats"]["selected_questions"], 3)
                self.assertEqual(audit["stats"]["excluded_questions"], 1)
                self.assertEqual(audit["replacement_questions"], 0)
                excluded = EVALUATOR.jsonl(root / "output/train/excluded.jsonl")
                self.assertEqual(excluded[0]["upstream_record"], questions[0])
                self.assertEqual(excluded[0]["issues"][0]["roles"], [field])

    def test_false_negative_annotations_and_missing_source_are_audited(self):
        for failure in ("false_negative", "missing_source"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                questions, _ = fixtures()
                if failure == "false_negative":
                    questions[2]["evidences"] = [FACT]
                else:
                    questions[2]["url"] += "-absent"
                with patch.object(ADAPTER, "PINS", self.inputs(root, questions=questions)), self.assertRaisesRegex(ValueError, "no usable manifest"):
                    self.prepare(root)
                reason = EVALUATOR.jsonl(root / "output/train/excluded.jsonl")[0]["issues"][0]["reason"]
                self.assertEqual(reason, "unanswerable_with_positive_annotations" if failure == "false_negative" else "source_document_missing")

    def test_byte_identical_reproduction_and_default_selection_matches_seed(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            questions, documents = fixtures()
            for i in range(3):
                extra = copy.deepcopy(questions[i])
                extra["id"] += "-other"
                questions.append(extra)
            with patch.object(ADAPTER, "PINS", self.inputs(root, questions, documents)):
                self.prepare(root, "one")
                self.prepare(root, "two")
            for path in (root / "one").rglob("*"):
                if path.is_file():
                    self.assertEqual(path.read_bytes(), (root / "two" / path.relative_to(root / "one")).read_bytes())
            selected = EVALUATOR.jsonl(root / "one/train/selected.raw.jsonl")
            for stratum in ADAPTER.STRATA:
                expected = min((q for q in questions if ADAPTER.category(q) == stratum), key=lambda q: ADAPTER.sha((ADAPTER.SEED + ":" + q["id"]).encode()))
                self.assertIn(expected, selected)

    def test_all_corpus_and_exact_duplicate_family_closure(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            questions, documents = fixtures()
            extra = copy.deepcopy(documents[0])
            extra["url"] += "-duplicate"
            documents.append(extra)
            with patch.object(ADAPTER, "PINS", self.inputs(root, questions, documents)):
                self.prepare(root, corpus="all")
            _, docs, _, _ = EVALUATOR.load_dataset(root / "output", ["train"])
            self.assertEqual(len(docs), 4)
            ledger = json.loads((root / "output/development-family-ledger.json").read_bytes())
            self.assertIn(extra["url"], {row["source_family"] for row in ledger})
            self.assertFalse((root / "output/dev").exists())
            self.assertFalse((root / "output/test").exists())

    def test_input_tamper_and_bounds_fail_before_output(self):
        for failure in ("hash", "input_bound", "notice_bound", "notice_license"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                pins = self.inputs(root)
                if failure == "hash":
                    (root / "conditionalqa-v1_0-train.json").write_bytes(b"[]")
                if failure == "notice_license":
                    (root / "conditionalqa-project.html").write_bytes(b"No license here")
                with patch.object(ADAPTER, "PINS", pins), patch.object(ADAPTER, "MAX_INPUT_BYTES", 3 if failure == "input_bound" else ADAPTER.MAX_INPUT_BYTES), patch.object(ADAPTER, "MAX_NOTICE_BYTES", 3 if failure == "notice_bound" else ADAPTER.MAX_NOTICE_BYTES), self.assertRaises(ValueError):
                    self.prepare(root)
                self.assertFalse((root / "output").exists())

    def test_existing_output_preserved_and_invalid_counts_names_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            output = root / "output"
            output.mkdir()
            (output / "sentinel").write_bytes(b"preserve")
            with self.assertRaisesRegex(ValueError, "new directory"):
                self.prepare(root)
            self.assertEqual((output / "sentinel").read_bytes(), b"preserve")
            for dataset, counts in (("../escape", (1, 1, 1)), ("valid", (1, 0, 1)), ("valid", (True, 1, 1))):
                with self.assertRaises(ValueError):
                    ADAPTER.prepare(root / "missing", root / "missing", root / "new", dataset=dataset, counts=counts)

    def test_loader_rejects_document_and_label_tampering(self):
        for label in (False, True):
            with self.subTest(label=label), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                with patch.object(ADAPTER, "PINS", self.inputs(root)):
                    self.prepare(root)
                path = root / "output/train/questions.jsonl" if label else next((root / "output/train/documents").iterdir())
                path.write_bytes(path.read_bytes() + b" ")
                with self.assertRaisesRegex(ValueError, "hash mismatch"):
                    EVALUATOR.load_dataset(root / "output", ["train"])

    def test_location_bounds_and_empty_quotes(self):
        self.assertEqual(ADAPTER.locate(b"anything", ""), [])
        self.assertEqual(len(ADAPTER.locate(b"aaaa", "aa")), 3)
        blob = FACT.encode()
        valid = ADAPTER.locate(blob, FACT)[0]
        for start, end in ((-1, len(blob)), (0, len(blob) + 1), (0, 0)):
            with self.assertRaisesRegex(ValueError, "outside"):
                EVALUATOR.validate_location(blob, dict(valid, start_byte=start, end_byte=end))


if __name__ == "__main__":
    unittest.main()
