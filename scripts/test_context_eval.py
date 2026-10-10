#!/usr/bin/env python3
"""Offline evidence evaluator contract tests; no model/provider compatibility claim."""
import importlib.util
import copy
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("context_eval", Path(__file__).with_name("context_eval.py"))
EVAL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EVAL)


class EvidenceTests(unittest.TestCase):
    def location(self, blob, quote):
        encoded = quote.encode("utf-8")
        start = blob.index(encoded)
        return {"start_byte": start, "end_byte": start + len(encoded), "quote": quote,
                "quote_sha256": EVAL.sha(encoded)}

    def test_unicode_offsets_are_bytes(self):
        blob = "前置 café 🐦 support".encode("utf-8")
        loc = self.location(blob, "café 🐦")
        EVAL.validate_location(blob, loc)
        self.assertNotEqual(loc["start_byte"], "前置 café 🐦 support".index("café"))
        bad = dict(loc, end_byte=loc["end_byte"] - 1)
        with self.assertRaisesRegex(ValueError, "UTF-8 bytes"):
            EVAL.validate_location(blob, bad)

    def test_union_handles_overlap_duplicates_and_adjacency(self):
        self.assertEqual(EVAL.union([(4, 8), (0, 5), (8, 10), (1, 3), (4, 8)]), [(0, 10)])
        self.assertEqual(EVAL.covered(2, 9, [(0, 5), (4, 7), (8, 12)]), 6)

    def test_invalid_and_empty_intervals(self):
        self.assertEqual(EVAL.union([(3, 3)]), [])
        for span in [(-1, 2), (4, 2), (1.5, 2)]:
            with self.assertRaises(ValueError):
                EVAL.union([span])

    def test_correct_source_wrong_span_has_no_evidence_credit(self):
        blob = b"header noise supported fact"
        groups = [{"doc_id": "d", "locations_any_of": [self.location(blob, "supported fact")]}]
        score = EVAL.group_coverage(groups, {"d": [(0, 12)]})
        self.assertEqual(score["byte_coverage"], 0)
        self.assertFalse(score["all_groups_complete"])

    def test_multiple_passages_can_cover_one_evidence_span(self):
        groups = [{"doc_id": "d", "locations_any_of": [{"start_byte": 10, "end_byte": 20}]}]
        score = EVAL.group_coverage(groups, {"d": [(10, 15), (13, 20)]})
        self.assertEqual(score["covered_bytes"], 10)
        self.assertTrue(score["all_groups_complete"])
        self.assertNotIn("semantic_complete", score)

    def test_repeated_evidence_is_alternative_not_double_requirement(self):
        groups = [{"doc_id": "d", "locations_any_of": [{"start_byte": 0, "end_byte": 4}, {"start_byte": 20, "end_byte": 24}]}]
        score = EVAL.group_coverage(groups, {"d": [(20, 24)]})
        self.assertTrue(score["all_groups_complete"])
        self.assertEqual(score["gold_bytes"], 4)

    def test_missing_highlights_cannot_be_complete(self):
        groups = [{"doc_id": "d", "locations_any_of": []}]
        score = EVAL.group_coverage(groups, {})
        self.assertIsNone(score["byte_coverage"])
        self.assertEqual(score["unmapped_groups"], 1)
        self.assertFalse(score["all_groups_complete"])

    def test_paths_cannot_escape(self):
        with tempfile.TemporaryDirectory() as folder:
            with self.assertRaises(ValueError):
                EVAL.within(Path(folder), "../outside")

    def test_citation_checks_original_bytes_hash_and_revision(self):
        blob = "café 🐦 fact".encode("utf-8")
        loc = self.location(blob, "🐦 fact")
        ref = {"source_id": "s", "source_revision": "r", "span": {"start": loc["start_byte"], "end": loc["end_byte"]},
               "quote_hash": "blake3:test-fixture"}
        data = {"text": "prefix\n🐦 fact", "passages": [{"text": "🐦 fact", "span": dict(ref["span"]), "locator": {"path": "content.md"},
                "citations": [{"kind": "source", "reference": ref}]}]}
        mapping = {"documents": {"d": {"source_id": "s", "revision_id": "r", "vault_content_path": "content.md"}}}
        with tempfile.TemporaryDirectory() as folder, patch.object(EVAL.subprocess, "run") as run:
            run.return_value = subprocess.CompletedProcess([], 0, b"blake3:test-fixture\n", b"")
            intervals, count = EVAL.citation_intervals(data, mapping, {"d": {"blob": blob}}, Path("mock-helper"), Path(folder), "q", 1)
            self.assertEqual(count, 1)
            self.assertEqual(intervals, {"d": [(loc["start_byte"], loc["end_byte"])]})
            self.assertEqual(run.call_args.kwargs["input"], "🐦 fact".encode())
            run.return_value = subprocess.CompletedProcess([], 0, b"blake3:wrong\n", b"")
            with self.assertRaisesRegex(ValueError, "hash mismatch"):
                EVAL.citation_intervals(data, mapping, {"d": {"blob": blob}}, Path("mock-helper"), Path(folder), "bad", 1)
            ref["source_revision"] = "stale"
            with self.assertRaisesRegex(ValueError, "revision"):
                EVAL.citation_intervals(data, mapping, {"d": {"blob": blob}}, Path("mock-helper"), Path(folder), "stale", 1)

    def test_failures_cannot_improve_fixed_highlight_denominator(self):
        good = {"answerability": "answerable", "status": "ok", "answer_highlight_eligible": True, "source_rank": 1,
                "best_gold_span_byte_coverage": 1, "strict_gold_span_complete": True, "best_answer_highlight_byte_coverage": 1,
                "strict_answer_highlight_complete": True, "context_seconds": 1, "citation_count": 1}
        failure = {"answerability": "answerable", "status": "error", "answer_highlight_eligible": True}
        result = EVAL.summarize([good, failure])
        self.assertEqual(result["source_hit_at_1"], 0.5)
        self.assertEqual(result["strict_gold_span_complete_rate"], 0.5)
        self.assertEqual(result["mean_answer_highlight_byte_coverage_errors_zero"], 0.5)
        self.assertEqual(result["strict_answer_highlight_complete_rate_errors_zero"], 0.5)
        self.assertEqual(result["mean_answer_highlight_byte_coverage_scored_queries"], 1)
        self.assertEqual(result["answer_highlight_eligible_queries"], 2)
        self.assertIsNone(result["semantic_complete_rate"])

    def test_merged_passage_accepts_subspan_and_mirrored_source_citations(self):
        blob = "café 🐦 fact trailing".encode("utf-8")
        loc = self.location(blob, "🐦 fact")
        span = {"start": loc["start_byte"], "end": loc["end_byte"]}
        citation = {"kind": "source", "reference": {"source_id": "mirror-s", "source_revision": "mirror-r",
                                                    "span": span, "quote_hash": "blake3:test-fixture"}}
        data = {"text": blob.decode(), "passages": [{"text": blob.decode(), "span": {"start": 0, "end": len(blob)},
                "locator": {"path": "owner.md"}, "citations": [citation]}]}
        mapping = {"documents": {"owner": {"source_id": "s", "revision_id": "r", "vault_content_path": "owner.md"},
                                 "mirror": {"source_id": "mirror-s", "revision_id": "mirror-r", "vault_content_path": "mirror.md"}}}
        with tempfile.TemporaryDirectory() as folder, patch.object(EVAL.subprocess, "run") as run:
            run.return_value = subprocess.CompletedProcess([], 0, b"blake3:test-fixture\n", b"")
            intervals, count = EVAL.citation_intervals(data, mapping, {"owner": {"blob": blob}, "mirror": {"blob": blob}}, Path("helper"), Path(folder), "merged", 1)
            self.assertEqual(count, 1)
            self.assertEqual(intervals, {"mirror": [(span["start"], span["end"])]})
            self.assertNotIn("owner", intervals)  # Uncited surrounding text earns no span credit.
            with self.assertRaisesRegex(ValueError, "Citation owner differs"):
                EVAL.citation_intervals(data, mapping, {"owner": {"blob": blob}, "mirror": {"blob": blob + b" unrelated owner"}},
                                       Path("helper"), Path(folder), "wrong-owner", 1)
            data["passages"][0]["span"]["end"] = span["end"] - 1
            data["passages"][0]["text"] = blob[:span["end"] - 1].decode()
            data["text"] = data["passages"][0]["text"]
            with self.assertRaisesRegex(ValueError, "outside containing passage"):
                EVAL.citation_intervals(data, mapping, {"owner": {"blob": blob}, "mirror": {"blob": blob}}, Path("helper"), Path(folder), "outside", 1)

    def test_empty_and_negative_only_summaries_have_no_positive_score(self):
        negative = {"answerability": "unanswerable", "status": "error", "answer_highlight_eligible": False}
        for rows in ([], [negative]):
            result = EVAL.summarize(rows)
            self.assertIsNone(result["source_hit_at_1"])
            self.assertIsNone(result["strict_gold_span_complete_rate"])
            self.assertIsNone(result["mean_answer_highlight_byte_coverage_errors_zero"])
            self.assertIsNone(result["semantic_complete_rate"])

    def test_offline_invocations_retain_failure_output(self):
        with tempfile.TemporaryDirectory() as folder, patch.object(EVAL.subprocess, "run") as run:
            run.return_value = subprocess.CompletedProcess([], 2, b'{"ok":false}', b"missing vector")
            with self.assertRaisesRegex(ValueError, "failed"):
                EVAL.invoke(Path("mock-cli"), Path("wiki"), ["context", "q"], Path(folder), "failure", 1)
            self.assertIn("--offline", run.call_args.args[0])
            self.assertEqual((Path(folder) / "failure.stderr").read_bytes(), b"missing vector")

    def test_offline_claim_is_verified_not_assumed(self):
        envelope = {"ok": True, "meta": {"network_used": True}}
        with tempfile.TemporaryDirectory() as folder, patch.object(EVAL.subprocess, "run") as run:
            run.return_value = subprocess.CompletedProcess([], 0, json.dumps(envelope).encode(), b"")
            with self.assertRaisesRegex(ValueError, "network"):
                EVAL.invoke(Path("mock-cli"), Path("wiki"), ["context", "q"], Path(folder), "network", 1)

    def test_selected_freshness_rejects_cached_or_inconsistent_claims(self):
        envelope = {"meta": {"freshness": "indexed_evidence", "index_generation": 1, "verified_at": "2026-10-10T00:00:00Z"},
                    "data": {"verification": {"mode": "indexed_evidence", "evidence_domain": "selected_documents",
                              "global_membership_verified": False, "discovery_generation": 1, "verified_at": "2026-10-10T00:00:00Z"},
                             "snapshot": {"generation": 1}, "dependency_fingerprint": "blake3:" + "a" * 64}}
        EVAL.check_context_freshness(envelope)
        for path, value in [(('meta', 'freshness'), 'verified_snapshot'),
                            (('data', 'verification', 'mode'), 'index_snapshot'),
                            (('data', 'verification', 'evidence_domain'), 'global'),
                            (('data', 'verification', 'global_membership_verified'), True),
                            (('data', 'verification', 'discovery_generation'), True),
                            (('meta', 'index_generation'), True),
                            (('data', 'snapshot', 'generation'), 2),
                            (('meta', 'verified_at'), 'another observation'),
                            (('data', 'dependency_fingerprint'), '')]:
            bad = copy.deepcopy(envelope)
            parent = bad
            for part in path[:-1]:
                parent = parent[part]
            parent[path[-1]] = value
            with self.subTest(path=path), self.assertRaises(ValueError):
                EVAL.check_context_freshness(bad)

    def test_dataset_alignment_and_hash_gate(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "dev" / "documents").mkdir(parents=True)
            blob = "前置 exact support".encode()
            doc = {"doc_id": "d", "path": "documents/d.md", "sha256": EVAL.sha(blob), "bytes": len(blob)}
            ref = {"answer": {"unanswerable": False}, "required_evidence_all_of": [{"doc_id": "d", "locations_any_of": [self.location(blob, "exact support")]}],
                   "highlighted_evidence": []}
            q = {"question_id": "q", "doc_id": "d", "answerability": "answerable", "reference_answers_any_of": [ref]}
            (root / "dev" / "documents/d.md").write_bytes(blob)
            (root / "dev" / "documents.jsonl").write_text(json.dumps(doc) + "\n")
            (root / "dev" / "questions.jsonl").write_text(json.dumps(q) + "\n")
            manifest = {"schema_version": 1, "splits": {"dev": {}}, "artifacts": [{"path": str(p.relative_to(root)), "sha256": EVAL.sha(p.read_bytes())}
                         for p in sorted((root / "dev").rglob("*")) if p.is_file()]}
            EVAL.save(root / "manifest.json", manifest)
            self.assertEqual(len(EVAL.load_dataset(root, ["dev"])[2]), 1)
            (root / "dev" / "documents/d.md").write_bytes(blob + b" changed")
            with self.assertRaisesRegex(ValueError, "hash mismatch"):
                EVAL.load_dataset(root, ["dev"])


class HostSelectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.wiki = self.root / "wiki"
        (self.wiki / "sources/s").mkdir(parents=True)
        self.blob = "café 🐦 exact support".encode()
        (self.wiki / "content.md").write_bytes(self.blob)
        (self.wiki / "sources/s/source.md").write_text('wiki_current_revision: "r"\n')
        self.docs = {"d": {"blob": self.blob}}
        self.hashes = {"manifest.json": "manifest-fixture"}
        self.mapping = {"wiki": str(self.wiki.resolve()), "dataset_manifest_sha256": "manifest-fixture",
                        "documents": {"d": {"source_id": "s", "revision_id": "r", "vault_content_path": "content.md"}}}
        self.fingerprint = "blake3:" + "a" * 64
        self.passage = {"text": self.blob.decode(), "span": {"start": 0, "end": len(self.blob)},
                        "locator": {"path": "content.md"}, "citations": [{"kind": "source", "reference": {
                            "source_id": "s", "source_revision": "r", "span": {"start": 0, "end": len(self.blob)},
                            "quote_hash": self.fingerprint}}]}
        self.manifest = {"dataset": "fixture", "license": "fixture-only"}
        self.questions = [{"question_id": f"q{i}", "split": "dev", "doc_id": "d", "query": f"ONLY_QUESTION_{i}",
                           "original_question": f"ORIGINAL_{i}", "answerability": "answerable",
                           "reference_answers_any_of": [{"annotation_id": f"gold-{i}", "answer": {"unanswerable": False,
                               "free_form_answer": "SECRET_GOLD_LABEL_NEVER_IN_TASK"},
                               "required_evidence_all_of": [{"doc_id": "d", "locations_any_of": [{"start_byte": 0, "end_byte": len(self.blob)}]}],
                               "highlighted_evidence": [{"doc_id": "d", "locations_any_of": [{"start_byte": 0, "end_byte": len(self.blob)}]}]}]}
                          for i in range(2)]
        self.args = SimpleNamespace(binary=self.root / "binary", hash_binary=self.root / "helper", wiki=self.wiki,
                                    dataset=self.root / "dataset", mapping=self.root / "mapping.json", output=self.root / "output",
                                    split=["dev"], mode="hybrid", query_style="prefixed", paper_local=False, max_bytes=6000,
                                    max_tokens=1500, limit=5, candidates=80, excerpt_bytes=1024, verification_max_entries=65536,
                                    verification_max_elapsed_ms=30000, timeout=1, selection_dir=None)
        self.args.binary.write_bytes(b"mock-binary")
        self.args.hash_binary.write_bytes(b"mock-helper")
        EVAL.save(self.args.mapping, self.mapping)
        self.mock_load = patch.object(EVAL, "load_dataset", return_value=(self.manifest, self.docs, self.questions, self.hashes)).start()
        self.addCleanup(patch.stopall)
        patch.object(EVAL, "print", create=True).start()
        self.mock_hash = patch.object(EVAL.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, (self.fingerprint + "\n").encode(), b"")).start()

    def packet(self, query):
        task = json.dumps({"instructions": "Use only supplied evidence; no tools.", "packet_fingerprint": self.fingerprint,
                           "payload": {"version": "lwiki.context-selection.v1", "binding": {"query": query},
                                       "cards": [{"id": "c0000", "passage": self.passage}]}}, ensure_ascii=False)
        size = len(task.encode())
        return {"fingerprint": self.fingerprint, "selector_input": task, "candidate_count": 1,
                "input_bytes": size, "estimated_tokens": (size + 3) // 4, "omitted_candidates": 0}

    def compact_payload(self):
        return {"version": "lwiki.context-selection.v2", "binding": {"query": "fixture query"},
                "authority_commitment": self.fingerprint,
                "sources": [{"id": "s0", "owner": "o0", "title": "Fixture source", "path": "content.md",
                             "label": "captured_source", "eligibility": "current"}],
                "cards": [{"id": "c0000", "source": "s0", "span": dict(self.passage["span"]),
                           "child_span": {"start": self.blob.index("🐦".encode()), "end": len(self.blob)},
                           "text": self.blob.decode(), "rendered_bytes": len(self.blob) + 300}]}

    def preparation_for(self, payload):
        task = json.dumps({"instructions": "Use only supplied evidence; no tools.",
                           "packet_fingerprint": self.fingerprint, "payload": payload}, ensure_ascii=False)
        size = len(task.encode())
        return self.envelope({"network_used": False, "verification": {"mode": "verified_snapshot"},
                              "text": "", "passages": [], "selection_packet": {
                                  "fingerprint": self.fingerprint, "selector_input": task,
                                  "candidate_count": len(payload["cards"]), "input_bytes": size,
                                  "estimated_tokens": (size + 3) // 4, "omitted_candidates": 0}})

    def envelope(self, data):
        return {"ok": True, "meta": {"freshness": "verified_snapshot", "network_used": False}, "data": data}

    def mock_invoke(self, binary, wiki, command, output, label, timeout):
        if command[0] == "search":
            return self.envelope({"hits": [{"source_id": "s"}]}), 0.1
        data = {"network_used": False, "verification": {"mode": "verified_snapshot"}, "text": "", "passages": []}
        if "--prepare-selection" in command:
            data["selection_packet"] = self.packet(command[1])
        else:
            data.update({"text": self.blob.decode(), "passages": [self.passage], "usage": {"rendered_bytes": len(self.blob),
                         "estimated_tokens": (len(self.blob) + 3) // 4, "token_accounting": "estimated_utf8_bytes_div4_ceil"},
                         "omissions": [], "warnings": [], "truncated": False})
        return self.envelope(data), 0.25

    def test_preparation_exports_exact_isolated_tasks_and_provenance(self):
        with patch.object(EVAL, "invoke", side_effect=self.mock_invoke) as invoke:
            self.assertEqual(EVAL.run_prepare_selection(self.args, self.manifest, self.docs, self.questions, self.hashes), 0)
        tasks = json.loads((self.args.output / "tasks.json").read_text())
        self.assertEqual(len(tasks), 2)
        for i, row in enumerate(tasks):
            task = (self.args.output / row["task_path"]).read_bytes()
            self.assertEqual(task, self.packet(self.questions[i]["query"])["selector_input"].encode())
            self.assertNotIn(b"SECRET_GOLD_LABEL", task)
            self.assertNotIn(self.questions[1 - i]["query"].encode(), task)
            self.assertNotIn(b"required_evidence_all_of", task)
            self.assertEqual(row["task_sha256"], EVAL.sha(task))
        self.assertEqual(sorted(p.name for p in (self.args.output / "selector-inputs").iterdir()), ["question-0000.txt", "question-0001.txt"])
        for call in invoke.call_args_list:
            command = call.args[2]
            self.assertEqual(command[0], "context")
            self.assertIn("--prepare-selection", command)
            self.assertEqual(command[command.index("--verification-max-elapsed-ms") + 1], "30000")
        hashes = json.loads((self.args.output / "output-hashes.json").read_text())
        self.assertIn("selector-inputs/question-0000.txt", hashes)
        metadata = json.loads((self.args.output / "run.json").read_text())
        self.assertEqual(metadata["binary_sha256"], EVAL.sha(b"mock-binary"))
        self.assertEqual(metadata["selector_task_max_bytes"], 130048)

    def test_preparation_rejects_wrong_query_and_oversized_unicode_task(self):
        self.args.output.mkdir()
        env, _ = self.mock_invoke(None, None, ["context", "other", "--prepare-selection"], None, None, None)
        with self.assertRaisesRegex(ValueError, "another query"):
            EVAL.validate_preparation(env, "expected", self.args, self.mapping, self.docs, "bad")
        packet = env["data"]["selection_packet"]
        packet["selector_input"] = "🦀" * 33000
        packet["input_bytes"] = len(packet["selector_input"].encode())
        with self.assertRaisesRegex(ValueError, "byte bound"):
            EVAL.validate_preparation(env, "other", self.args, self.mapping, self.docs, "large")

    def test_compact_preparation_checks_exact_bytes_without_inventing_citations(self):
        env = self.preparation_for(self.compact_payload())
        packet, task = EVAL.validate_preparation(env, "fixture query", self.args, self.mapping, self.docs, "v2")
        self.assertEqual(task, packet["selector_input"].encode())
        self.assertNotIn(b'"citations"', task)
        self.assertNotIn(b'"source_revision"', task)
        self.mock_hash.assert_not_called()
        # Preparation writes only the immutable supplied task. Final-context
        # citation_intervals remains responsible for full authority auditing.
        self.assertFalse(self.args.output.exists())

    def test_compact_preparation_export_keeps_exact_task(self):
        def compact(binary, wiki, command, output, label, timeout):
            payload = self.compact_payload()
            payload["binding"]["query"] = command[1]
            return self.preparation_for(payload), 0.1
        with patch.object(EVAL, "invoke", side_effect=compact):
            self.assertEqual(EVAL.run_prepare_selection(self.args, self.manifest, self.docs, self.questions, self.hashes), 0)
        self.mock_hash.assert_not_called()
        tasks = json.loads((self.args.output / "tasks.json").read_text())
        for row, question in zip(tasks, self.questions):
            payload = self.compact_payload()
            payload["binding"]["query"] = question["query"]
            expected = self.preparation_for(payload)["data"]["selection_packet"]["selector_input"].encode()
            self.assertEqual((self.args.output / row["task_path"]).read_bytes(), expected)

    def test_v1_preparation_retains_citation_hash_audit(self):
        self.args.output.mkdir()
        env, _ = self.mock_invoke(None, None, ["context", "fixture query", "--prepare-selection"], None, None, None)
        EVAL.validate_preparation(env, "fixture query", self.args, self.mapping, self.docs, "v1")
        self.mock_hash.assert_called_once()
        self.assertEqual(self.mock_hash.call_args.kwargs["input"], self.blob)

    def test_preparation_rejects_unknown_and_missing_payload_versions(self):
        for version in (None, "lwiki.context-selection.v3", 2):
            payload = self.compact_payload()
            if version is None:
                del payload["version"]
            else:
                payload["version"] = version
            with self.subTest(version=version), self.assertRaisesRegex(ValueError, "payload version"):
                EVAL.validate_preparation(self.preparation_for(payload), "fixture query", self.args, self.mapping, self.docs, "version")

    def test_compact_preparation_rejects_corrupted_metadata_and_evidence(self):
        mutations = [
            ("unknown source", lambda p: p["cards"][0].update(source="missing")),
            ("Duplicate selector source ID", lambda p: p["sources"].append(copy.deepcopy(p["sources"][0]))),
            ("card count or ID", lambda p: p["cards"].append(copy.deepcopy(p["cards"][0]))),
            ("Invalid selector card IDs", lambda p: p["cards"][0].update(id="bad id")),
            ("source or owner ID", lambda p: p["sources"][0].update(owner=True)),
            ("frozen mapping", lambda p: p["sources"][0].update(path="../elsewhere.md")),
            ("current captured source", lambda p: p["sources"][0].update(eligibility="historical")),
            ("current captured source", lambda p: p["sources"][0].update(label="note_text")),
            ("title exceeds UTF-8", lambda p: p["sources"][0].update(title="🐦" * 1025)),
            ("authority commitment", lambda p: p.update(authority_commitment="blake3:invalid")),
            ("original UTF-8 bytes", lambda p: p["cards"][0].update(text="café 🐦 false support")),
            ("span out of bounds", lambda p: p["cards"][0]["span"].update(start=True)),
            ("span out of bounds", lambda p: p["cards"][0]["span"].update(end=len(self.blob) + 1)),
            ("child span outside passage", lambda p: p["cards"][0].update(child_span={"start": 0, "end": 0})),
            ("child span outside passage", lambda p: p["cards"][0].update(child_span={"start": 0, "end": len(self.blob) + 1})),
            ("UTF-8", lambda p: p["cards"][0].update(child_span={"start": self.blob.index("🐦".encode()) + 1, "end": len(self.blob)})),
            ("rendered byte estimate", lambda p: p["cards"][0].update(rendered_bytes=True)),
            ("rendered byte estimate", lambda p: p["cards"][0].update(rendered_bytes=len(self.blob) - 1)),
            ("rendered byte estimate", lambda p: p["cards"][0].update(rendered_bytes=EVAL.SELECTOR_INPUT_MAX_BYTES + 1)),
            ("card fields", lambda p: p["cards"][0].update(citations=[])),
            ("unused sources", lambda p: p["sources"].append(dict(p["sources"][0], id="s1"))),
        ]
        for message, mutate in mutations:
            payload = self.compact_payload()
            mutate(payload)
            with self.subTest(message=message), self.assertRaisesRegex(ValueError, message):
                EVAL.validate_preparation(self.preparation_for(payload), "fixture query", self.args, self.mapping, self.docs, "corrupt")
        self.mock_hash.assert_not_called()

    def test_compact_preparation_caps_source_table_before_iteration(self):
        payload = self.compact_payload()
        payload["sources"] = [dict(payload["sources"][0], id=f"s{i}") for i in range(81)]
        with self.assertRaisesRegex(ValueError, "source count exceeds bound"):
            EVAL.validate_preparation(self.preparation_for(payload), "fixture query", self.args, self.mapping, self.docs, "sources")

    def test_compact_preparation_rejects_character_offsets_and_excerpt_overflow(self):
        payload = self.compact_payload()
        payload["cards"][0]["span"]["end"] = len(self.blob.decode())
        with self.assertRaisesRegex(ValueError, "original UTF-8 bytes"):
            EVAL.validate_preparation(self.preparation_for(payload), "fixture query", self.args, self.mapping, self.docs, "offset")
        self.args.excerpt_bytes = len(self.blob) - 1
        with self.assertRaisesRegex(ValueError, "excerpt bound"):
            EVAL.validate_preparation(self.preparation_for(self.compact_payload()), "fixture query", self.args, self.mapping, self.docs, "excerpt")

    def test_compact_preparation_owner_aliases_cannot_evade_limit(self):
        self.mapping["documents"]["other"] = {"source_id": "other", "revision_id": "other-rev", "vault_content_path": "other.md"}
        self.docs["other"] = {"blob": b"unrelated document"}
        payload = self.compact_payload()
        payload["sources"].append(dict(payload["sources"][0], id="s1", path="other.md"))
        payload["cards"].append(dict(payload["cards"][0], id="c0001", source="s1", span={"start": 0, "end": 18},
                                     child_span=None, text="unrelated document"))
        with self.assertRaisesRegex(ValueError, "merges different frozen content"):
            EVAL.validate_preparation(self.preparation_for(payload), "fixture query", self.args, self.mapping, self.docs, "alias")
        payload["sources"][1]["owner"] = "o1"
        self.args.limit = 1
        with self.assertRaisesRegex(ValueError, "owner bound"):
            EVAL.validate_preparation(self.preparation_for(payload), "fixture query", self.args, self.mapping, self.docs, "owners")
        # Equivalent immutable content at two paths legitimately shares one owner.
        self.docs["other"]["blob"] = self.blob
        payload["sources"][1]["owner"] = "o0"
        payload["cards"][1] = dict(payload["cards"][0], id="c0001", source="s1")
        EVAL.validate_preparation(self.preparation_for(payload), "fixture query", self.args, self.mapping, self.docs, "equivalent")
        payload["sources"][1].update(path="content.md", owner="o1")
        with self.assertRaisesRegex(ValueError, "inconsistent owners"):
            EVAL.validate_preparation(self.preparation_for(payload), "fixture query", self.args, self.mapping, self.docs, "split")

    def test_strict_reply_rejects_duplicate_keys_fields_ids_and_bounds(self):
        good = {"packet_fingerprint": self.fingerprint, "ordered_ids": []}
        self.assertEqual(EVAL.validate_selection_reply(json.dumps(good).encode()), good)
        bad = [b'{"packet_fingerprint":"x","packet_fingerprint":"y","ordered_ids":[]}',
               json.dumps(dict(good, schema="wrong")).encode(), json.dumps(dict(good, ordered_ids=["x", "x"])).encode(),
               json.dumps(dict(good, ordered_ids=[str(i) for i in range(21)])).encode(), b" " * 4097,
               json.dumps(good).encode("utf-16"), json.dumps(dict(good, ordered_ids=[True])).encode()]
        for blob in bad:
            with self.subTest(blob=blob[:40]), self.assertRaises((ValueError, UnicodeError)):
                EVAL.validate_selection_reply(blob)

    def test_missing_and_invalid_replies_score_zero_without_product_retry(self):
        self.args.selection_dir = self.root / "replies"
        self.args.selection_dir.mkdir()
        (self.args.selection_dir / "question-0001.json").write_bytes(b'{"ordered_ids":[]}')
        with patch.object(EVAL, "invoke") as invoke:
            self.assertEqual(EVAL.run_evaluate(self.args, self.manifest, self.docs, self.questions, self.hashes), 1)
        invoke.assert_not_called()
        summary = json.loads((self.args.output / "summary.json").read_text())
        self.assertEqual(summary["errors"], 2)
        self.assertEqual(summary["strict_gold_span_complete_rate"], 0)
        self.assertEqual(summary["mean_answer_highlight_byte_coverage_errors_zero"], 0)
        self.assertIsNone(summary["end_to_end_seconds"])
        self.assertEqual((self.args.output / "question-0001-selection.json").read_bytes(), b'{"ordered_ids":[]}')

    def test_replay_retains_exact_reply_and_uses_only_final_stage_timing(self):
        self.args.selection_dir = self.root / "replies"
        self.args.selection_dir.mkdir()
        raw = (' { "ordered_ids": ["c0000"], "packet_fingerprint": "' + self.fingerprint + '" }\n').encode()
        for i in range(2):
            (self.args.selection_dir / f"question-{i:04d}.json").write_bytes(raw)
        with patch.object(EVAL, "invoke", side_effect=self.mock_invoke) as invoke:
            self.assertEqual(EVAL.run_evaluate(self.args, self.manifest, self.docs, self.questions, self.hashes), 0)
        commands = [call.args[2] for call in invoke.call_args_list if call.args[2][0] == "context"]
        self.assertEqual(len(commands), 2)
        for command in commands:
            copied = Path(command[command.index("--selection") + 1])
            self.assertEqual(copied.read_bytes(), raw)
            self.assertEqual(command[command.index("--verification-max-elapsed-ms") + 1], "30000")
            self.assertNotIn("--prepare-selection", command)
        summary = json.loads((self.args.output / "summary.json").read_text())
        self.assertEqual(summary["strict_gold_span_complete_rate"], 1)
        self.assertEqual(summary["context_latency_median_seconds"], 0.25)
        for key in ("selector_actual_tokens", "selector_cost", "selector_invocations", "end_to_end_seconds"):
            self.assertIsNone(summary[key])

    def test_unknown_or_stale_packet_product_error_is_not_repaired(self):
        self.args.selection_dir = self.root / "replies"
        self.args.selection_dir.mkdir()
        raw = json.dumps({"packet_fingerprint": "blake3:" + "b" * 64, "ordered_ids": ["unknown"]}).encode()
        (self.args.selection_dir / "question-0000.json").write_bytes(raw)
        def rejected(binary, wiki, command, output, label, timeout):
            if command[0] == "context":
                raise ValueError("product rejected wrong packet/unknown ID")
            return self.mock_invoke(binary, wiki, command, output, label, timeout)
        with patch.object(EVAL, "invoke", side_effect=rejected) as invoke:
            self.assertEqual(EVAL.run_evaluate(self.args, self.manifest, self.docs, self.questions, self.hashes), 1)
        self.assertEqual(sum(call.args[2][0] == "context" for call in invoke.call_args_list), 1)
        rows = json.loads((self.args.output / "results.json").read_text())
        self.assertTrue(all(row["status"] == "error" for row in rows))
        self.assertIn("product rejected", rows[0]["error"])

    def test_empty_reply_keeps_zero_support_without_abstention_judgment(self):
        self.args.selection_dir = self.root / "replies"
        self.args.selection_dir.mkdir()
        raw = json.dumps({"packet_fingerprint": self.fingerprint, "ordered_ids": []}).encode()
        for i in range(2):
            (self.args.selection_dir / f"question-{i:04d}.json").write_bytes(raw)
        def empty(binary, wiki, command, output, label, timeout):
            envelope, seconds = self.mock_invoke(binary, wiki, command, output, label, timeout)
            if command[0] == "context":
                envelope["data"].update({"text": "", "passages": [], "usage": {"rendered_bytes": 0, "estimated_tokens": 0,
                                          "token_accounting": "estimated_utf8_bytes_div4_ceil"}})
            return envelope, seconds
        with patch.object(EVAL, "invoke", side_effect=empty):
            self.assertEqual(EVAL.run_evaluate(self.args, self.manifest, self.docs, self.questions, self.hashes), 0)
        summary = json.loads((self.args.output / "summary.json").read_text())
        self.assertEqual(summary["errors"], 0)
        self.assertEqual(summary["strict_gold_span_complete_rate"], 0)
        self.assertIsNone(summary["semantic_complete_rate"])
        self.assertNotIn("abstention_accuracy", summary)

    def test_original_arm_has_no_selection_flag_or_resource_claims(self):
        self.args.verification_max_elapsed_ms = None
        with patch.object(EVAL, "invoke", side_effect=self.mock_invoke) as invoke:
            self.assertEqual(EVAL.run_evaluate(self.args, self.manifest, self.docs, self.questions, self.hashes), 0)
        for call in invoke.call_args_list:
            self.assertNotIn("--selection", call.args[2])
            self.assertNotIn("--prepare-selection", call.args[2])
            self.assertNotIn("--verification-max-elapsed-ms", call.args[2])
        summary = json.loads((self.args.output / "summary.json").read_text())
        self.assertNotIn("workflow", summary)
        self.assertNotIn("selector_cost", summary)

    def test_indexed_evidence_is_scored_without_claiming_global_verification(self):
        def selected(binary, wiki, command, output, label, timeout):
            envelope, seconds = self.mock_invoke(binary, wiki, command, output, label, timeout)
            if command[0] == "context":
                envelope["meta"].update(freshness="indexed_evidence", index_generation=1, verified_at="2026-10-10T00:00:00Z")
                envelope["data"].update(snapshot={"generation": 1}, dependency_fingerprint=self.fingerprint,
                    verification={"mode": "indexed_evidence", "evidence_domain": "selected_documents",
                                  "global_membership_verified": False, "discovery_generation": 1, "verified_at": "2026-10-10T00:00:00Z"})
            return envelope, seconds
        with patch.object(EVAL, "invoke", side_effect=selected):
            self.assertEqual(EVAL.run_evaluate(self.args, self.manifest, self.docs, self.questions, self.hashes), 0)
        rows = json.loads((self.args.output / "results.json").read_text())
        self.assertTrue(all(row["verification"]["mode"] == "indexed_evidence"
                            and row["verification"]["global_membership_verified"] is False for row in rows))
        self.assertTrue(all(row["context_seconds"] == 0.25 for row in rows))

    def test_indexed_preparation_keeps_selected_verification_and_exact_cards(self):
        envelope = self.preparation_for(self.compact_payload())
        envelope["meta"].update(freshness="indexed_evidence", index_generation=1, verified_at="2026-10-10T00:00:00Z")
        envelope["data"].update(snapshot={"generation": 1}, dependency_fingerprint=self.fingerprint,
            verification={"mode": "indexed_evidence", "evidence_domain": "selected_documents",
                          "global_membership_verified": False, "discovery_generation": 1, "verified_at": "2026-10-10T00:00:00Z"})
        packet, task = EVAL.validate_preparation(envelope, "fixture query", self.args, self.mapping, self.docs, "prepared")
        self.assertEqual(packet["candidate_count"], 1)
        self.assertEqual(json.loads(task)["payload"]["cards"][0]["text"], self.blob.decode())
        self.assertFalse(envelope["data"]["verification"]["global_membership_verified"])


if __name__ == "__main__":
    unittest.main()
