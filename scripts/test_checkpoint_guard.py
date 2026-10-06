#!/usr/bin/env python3
import importlib.util
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('checkpoint_guard', Path(__file__).with_name('checkpoint_guard.py'))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


class CheckpointGuardTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.addCleanup(patch.stopall)
        patch.object(guard, 'repository', return_value=self.root).start()
        patch.object(guard.subprocess, 'check_output', side_effect=lambda *a, **k:
            b'\0'.join(str(p.relative_to(self.root)).encode() for p in self.root.rglob('*')
                       if p.is_file())).start()
        self.write('.gitignore', '.artifacts/\n')
        self.write('src/lib.rs', 'pub fn original() {}\n')
        self.write('tests/fixtures/example.md', 'exact fixture\n')

    def write(self, name, text):
        file = self.root / name
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(text)

    def payload(self, event, command='cargo test --release --lib', response=None, use=None):
        return {'cwd': str(self.root), 'session_id': 'fixture-session', 'tool_use_id': use or 'test-use',
                'hook_event_name': event, 'tool_input': {'command': command}, 'tool_response': response}

    def pre(self, command='cargo test --release --lib'):
        return guard.handle(self.payload('PreToolUse', command)).get('hookSpecificOutput', {})

    def complete(self, response, command='cargo test --release --lib'):
        self.pre(command)
        return guard.handle(self.payload('PostToolUse', command, response))

    def assert_allowed(self, command='cargo test --release --lib'):
        self.assertNotEqual(self.pre(command).get('permissionDecision'), 'deny')

    def test_unchanged_explicit_success_is_denied(self):
        self.complete({'exit_code': 0})
        self.assertEqual(self.pre()['permissionDecision'], 'deny')

    def test_source_new_test_and_fixture_changes_allow(self):
        for name in ['src/lib.rs', 'tests/new.rs', 'tests/fixtures/example.md', 'Cargo.toml',
                     'skills/demo/SKILL.md', 'test_support/fixture.txt', 'bazel/rules.bzl',
                     'platforms/native.json', 'cargo-bazel-lock.json']:
            with self.subTest(name=name):
                self.complete({'exit_code': 0})
                self.write(name, 'new source or fixture ' + name)
                self.assert_allowed()

    def test_docs_and_artifacts_do_not_unlock(self):
        self.complete({'exit_code': 0})
        self.write('docs/new.md', 'Documentation edit')
        self.write('.artifacts/run.txt', 'Run output')
        self.assertEqual(self.pre()['permissionDecision'], 'deny')

    def test_failed_unknown_or_async_attempts_allow(self):
        for response in [{'exit_code': 1}, {}, 'passed', {'exit_code': False},
                         {'exit_code': 0, 'session_id': 123}, {'exit_code': 0, 'running': True},
                         {'exit_code': 0, 'status': 'in_progress'}]:
            with self.subTest(response=response):
                self.complete(response)
                self.assert_allowed()

    def test_filters_profiles_and_sessions_are_distinct(self):
        self.complete({'exit_code': 0})
        self.assert_allowed('cargo test --release --lib literal')
        self.assert_allowed('cargo test --profile dev --lib')
        payload = self.payload('PreToolUse')
        payload['session_id'] = 'new-session'
        self.assertNotEqual(guard.handle(payload)['hookSpecificOutput'].get('permissionDecision'), 'deny')

    def test_reason_override_and_cmd_fallback(self):
        self.complete({'exit_code': 0})
        decision = self.pre("LWIKI_CHECKPOINT_REASON='final acceptance' cargo test --release --lib")
        self.assertNotEqual(decision.get('permissionDecision'), 'deny')
        self.assertIn('final acceptance', decision['additionalContext'])
        payload = self.payload('PreToolUse')
        payload['tool_input'] = {'cmd': 'cargo test --release --lib'}
        self.assertEqual(guard.handle(payload)['hookSpecificOutput']['permissionDecision'], 'deny')

    def test_unrelated_complex_shell_and_other_events_are_ignored(self):
        for command in ['echo cargo test', 'cargo fmt', 'cargo test && echo done', 'bazel run //:tool test']:
            self.assertIsNone(guard.checkpoint(command))
            self.assertEqual(guard.handle(self.payload('PreToolUse', command)), {})
        self.assertEqual(guard.handle({'hook_event_name': 'Stop'}), {})

    def test_subdirectory_hash_still_covers_repository_sources(self):
        subdir = self.root / 'subdir'
        subdir.mkdir()
        before = guard.input_hash(subdir)
        self.write('src/lib.rs', 'changed outside cwd')
        self.assertNotEqual(before, guard.input_hash(subdir))

    def test_bazel_wrapper_and_direct_binaries_are_recognized(self):
        for command in ['bazel test //:unit_tests --config=release',
                        'python3 scripts/bazel.py -- test //:unit_tests --test_arg=literal',
                        '.build-cache/unit_tests literal', '/tmp/offline_cli_test --exact literal']:
            self.assertIsNotNone(guard.checkpoint(command))

    def test_inputs_changed_during_run_and_missing_pre_do_not_record_success(self):
        self.pre()
        self.write('src/lib.rs', 'changed during run')
        guard.handle(self.payload('PostToolUse', response={'exit_code': 0}))
        self.assert_allowed()
        guard.handle(self.payload('PostToolUse', response={'exit_code': 0}, use='missing-pre'))
        self.assert_allowed()


if __name__ == '__main__':
    unittest.main()
