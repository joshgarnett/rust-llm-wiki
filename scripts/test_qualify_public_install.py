#!/usr/bin/env python3
"""Offline public-install verifier regressions; no download or product process."""
import copy
from pathlib import Path
import tempfile
import unittest

import qualify_public_install as qualification


class PublicInstallEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.body = b"# My note\n\nThe cedar room meeting starts at 14:35 UTC. PUBLIC-INSTALL-482.\n"
        self.content_hash = "blake3:326c29bcbf9538c156d41046fc05868573e491dc8a32b3f1dfdad96b37ea6ca8"
        self.path = "sources/source-id/revisions/revision-id/content.md"
        self.span = {"start": 0, "end": len(self.body)}
        self.citation = {"kind": "source", "reference": {
            "source_id": "source-id", "source_revision": "revision-id",
            "span": self.span, "quote_hash": self.content_hash}}
        self.captured = {"allocated_ids": {"source": "source-id", "revision": "revision-id"},
                         "plan": {"operations": [{"path": self.path,
                             "after": {"state": "hash", "hash": self.content_hash},
                             "byte_len": len(self.body)}]}}
        self.passage = {"locator": {"path": self.path, "observed_hash": self.content_hash,
            "record": {"record_id": "revision-id", "expected_kind": "revision"}},
            "span": self.span, "text": self.body.decode(), "eligibility": "current",
            "citations": [self.citation]}
        self.read = {"path": self.path, "hash": self.content_hash, "range": self.span,
                     "body": self.body.decode(), "truncated": False, "continuation": None,
                     "source_citation": {"citation": self.citation, "eligibility": "current"}}

    def verify(self, passage=None, read=None, captured=None):
        qualification.verify_captured_passage(
            passage if passage is not None else self.passage,
            captured if captured is not None else self.captured,
            read if read is not None else self.read, self.body)

    def test_logical_locator_does_not_require_a_physical_file_at_that_path(self):
        with tempfile.TemporaryDirectory() as directory:
            wiki = Path(directory)
            retained = wiki / ".wiki/retained" / self.path
            retained.parent.mkdir(parents=True)
            retained.write_bytes(self.body)
            self.assertFalse((wiki / self.path).exists())
            self.verify()

    def test_read_must_authenticate_every_payload_and_citation_field(self):
        replacements = [
            ("path", "sources/other/content.md"), ("hash", "blake3:different"),
            ("range", {"start": 1, "end": len(self.body)}),
            ("body", self.body.decode() + "changed"), ("truncated", True),
            ("continuation", self.span), ("source_citation", None),
        ]
        for field, replacement in replacements:
            with self.subTest(field=field):
                read = copy.deepcopy(self.read)
                read[field] = replacement
                with self.assertRaises(AssertionError):
                    self.verify(read=read)
        for field in ("source_id", "source_revision", "span", "quote_hash"):
            with self.subTest(citation_field=field):
                read = copy.deepcopy(self.read)
                read["source_citation"]["citation"]["reference"][field] = "changed"
                with self.assertRaises(AssertionError):
                    self.verify(read=read)
        read = copy.deepcopy(self.read)
        read["source_citation"]["eligibility"] = "historical"
        with self.assertRaises(AssertionError):
            self.verify(read=read)

    def test_passage_must_match_capture_identity_hash_span_and_bytes(self):
        for field, replacement in (
                ("text", "changed"), ("span", {"start": 1, "end": len(self.body)}),
                ("eligibility", "historical"), ("citations", [])):
            with self.subTest(field=field):
                passage = copy.deepcopy(self.passage)
                passage[field] = replacement
                with self.assertRaises(AssertionError):
                    self.verify(passage=passage)
        for field, replacement in (("path", "../outside"), ("observed_hash", "changed"),
                ("record", {"record_id": "other", "expected_kind": "revision"}),
                ("record", {"record_id": "revision-id", "expected_kind": "source"})):
            with self.subTest(locator_field=field, replacement=replacement):
                passage = copy.deepcopy(self.passage)
                passage["locator"][field] = replacement
                with self.assertRaises(AssertionError):
                    self.verify(passage=passage)
        for field, replacement in (("byte_len", len(self.body) + 1),
                ("after", {"state": "hash", "hash": "changed"})):
            with self.subTest(operation_field=field):
                captured = copy.deepcopy(self.captured)
                captured["plan"]["operations"][0][field] = replacement
                with self.assertRaises(AssertionError):
                    self.verify(captured=captured)

    def test_restore_changes_only_the_markdown_link_absolute_root(self):
        wiki, restored = Path("/private/My wiki"), Path("/private/Restored wiki")
        original = {"text": "14:35 UTC [My note](</private/My%20wiki/sources/views/My%20note.md>)"}
        expected = {"text": "14:35 UTC [My note](</private/Restored%20wiki/sources/views/My%20note.md>)"}
        qualification.verify_restored_rendering(original, expected, wiki, restored)
        for text in (original["text"], expected["text"].replace("14:35", "14:36"),
                     expected["text"].replace("My%20note.md", "different.md"),
                     expected["text"] + " extra"):
            with self.subTest(text=text), self.assertRaises(AssertionError):
                qualification.verify_restored_rendering(original, {"text": text}, wiki, restored)
        with self.assertRaises(AssertionError):
            qualification.verify_restored_rendering({"text": "no saved-note link"},
                                                    {"text": "no saved-note link"}, wiki, restored)


if __name__ == "__main__":
    unittest.main()
