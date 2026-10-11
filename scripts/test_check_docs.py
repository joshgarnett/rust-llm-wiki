#!/usr/bin/env python3
"""Disposable policy regressions; no builds, providers or production documents."""
import tempfile
from pathlib import Path
import unittest
from check_docs import heading_anchors, validate


class DocumentationPolicyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.visible = set()
        self.manifest = {'schema_version': 'lwiki.documentation-policy.v1', 'documents': [],
                         'migration_paths': {'docs/old.md': 'docs/developer/current.md'}}

    def add(self, path, text, classification='developer_protocol', reviewed=True, visible=True):
        destination = self.root / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(text)
        if visible:
            self.visible.add(path)
        if reviewed:
            self.manifest['documents'].append({'path': path, 'classification': classification,
                                              'purpose': 'Explain a reusable procedure and its evidence limits.'})

    def check(self):
        return validate(self.root, self.visible, self.manifest)

    def test_curated_protocols_and_release_changelog_are_allowed(self):
        self.add('README.md', '# Start here\n\n[Protocol](docs/developer/current.md#recovery-checkpoints)\n', 'user_guide')
        self.add('docs/developer/current.md', '# Protocol\n\n## Recovery checkpoints\n\nPin the candidate before a benchmark. Record progress in local evidence.\n')
        self.add('docs/developer/releases.md', '# Changelog\n\n## 2026-01-01\nFixed a refusal.\n\n## 2026-02-01\nAdded a command.\n\n## 2026-03-01\nUpdated a schema.\n', 'changelog')
        self.assertEqual(self.check(), [])

    def test_reviewed_names_do_not_hide_a_run_diary(self):
        self.add('docs/developer/architecture.md', '# Architecture\n\n## Attempt 1\nOne result.\n\n## Attempt 2\nA retry.\n\n## Attempt 3\nAnother retry.\n\nWorker: example\nNext step: run again\n')
        errors = self.check()
        self.assertTrue(any('run diary' in error for error in errors))
        self.assertTrue(any('metadata ledger' in error for error in errors))

    def test_raw_state_and_report_trees_refuse_even_if_allowlisted(self):
        self.add('docs/developer/STATE.md', '# Current state\n')
        self.add('docs/developer/reports/latest.md', '# Result\n')
        self.assertEqual(sum('cannot be published' in e for e in self.check()), 2)

    def test_unreviewed_document_refuses_but_ignored_local_evidence_is_excluded(self):
        self.add('docs/developer/new.md', '# Guide\n', reviewed=False)
        self.add('docs/execution/STATE.md', '# Session checkpoint\n', reviewed=False, visible=False)
        errors = self.check()
        self.assertEqual(len(errors), 1)
        self.assertIn('absent from reviewed', errors[0])

    def test_existing_ignored_file_cannot_satisfy_a_fresh_clone_link(self):
        self.add('README.md', '# Guide\n\n[Evidence](.work/private.md)\n', 'user_guide')
        self.add('.work/private.md', '# Local evidence\n', reviewed=False, visible=False)
        self.assertTrue(any('unavailable in fresh clone' in e for e in self.check()))

    def test_missing_path_redirect_and_missing_anchor_are_actionable(self):
        self.add('README.md', '# Guide\n\n[Old](docs/old.md)\n[Missing heading](docs/developer/current.md#absent)\n', 'user_guide')
        self.add('docs/developer/current.md', '# Current\n')
        errors = self.check()
        self.assertTrue(any('use docs/developer/current.md' in e for e in errors))
        self.assertTrue(any('missing heading anchor' in e for e in errors))

    def test_duplicate_unicode_headings_keep_individual_anchors(self):
        self.add('README.md', '# Guide\n\n[Second](docs/developer/current.md#café-東京-1)\n', 'user_guide')
        self.add('docs/developer/current.md', '# Current\n\n## Café 東京\nOne.\n\n## Café 東京\nTwo.\n')
        self.assertIn('café-東京-1', heading_anchors((self.root / 'docs/developer/current.md').read_text()))
        self.assertEqual(self.check(), [])

    def test_prose_checks_preserve_exact_code_and_quoted_evidence(self):
        exact = '# Guide\n\n```text\nLiteral—text 😀\n[fixture](missing.md)\n```\n\n> Quoted—evidence 😀\n\nUse `literal—bytes😀 [fixture](missing.md)`. Preserve “quoted—text😀”.\n'
        self.add('README.md', exact, 'user_guide')
        self.assertEqual(self.check(), [])
        (self.root / 'README.md').write_text(exact + '\nA thought—an aside.\nA smile 😀\n')
        errors = self.check()
        self.assertEqual(sum('em dash in prose' in e for e in errors), 1)
        self.assertEqual(sum('emoji or pictograph' in e for e in errors), 1)

    def test_inventory_requires_a_unique_enduring_purpose(self):
        self.add('README.md', '# Guide\n', 'user_guide')
        self.manifest['documents'][0]['purpose'] = 'log'
        self.manifest['documents'].append(dict(self.manifest['documents'][0]))
        errors = self.check()
        self.assertTrue(any('classification and purpose required' in e for e in errors))
        self.assertTrue(any('duplicate inventory path' in e for e in errors))


if __name__ == '__main__':
    unittest.main()
