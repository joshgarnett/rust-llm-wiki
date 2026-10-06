#!/usr/bin/env python3
"""Conservative Codex hook for repeated successful Rust checkpoints."""
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys

REMINDER = ('Finish the named user-visible feature batch before a Rust checkpoint; '
            'inspect whether higher-impact authorized work remains. Do not make arbitrary '
            'edits merely to unlock a build. Use LWIKI_CHECKPOINT_REASON with a concrete '
            'acceptance, environment, or correctness-regression reason when repetition is needed.')


def checkpoint(command):
    try:
        words = shlex.split(command)
    except ValueError:
        return None
    if not words or any(x in command for x in ('\n', ';', '&&', '||', '|', '`', '$(')):
        return None
    reason = None
    while words and '=' in words[0] and not words[0].startswith('-'):
        key, value = words.pop(0).split('=', 1)
        if key == 'LWIKI_CHECKPOINT_REASON' and value.strip():
            reason = value.strip()
        else:
            # Environment settings affect compilation/execution identity.
            return None
    if not words:
        return None
    executable = Path(words[0]).name
    recognized = (executable == 'cargo' and len(words) > 1
                  and words[1] in ('build', 'test', 'check', 'clippy', 'bench'))
    offset = 1
    if executable in ('python', 'python3') and len(words) > 1 and words[1] == 'scripts/bazel.py':
        offset = 2
        executable = 'bazel'
    if executable in ('bazel', 'bazelisk'):
        tail = words[offset:]
        while tail and tail[0].startswith('--'):
            tail = tail[1:]
        recognized = bool(tail) and tail[0] in ('build', 'test')
    if executable in ('unit_tests', 'unit_tests.exe', 'offline_cli_test', 'offline_cli_test.exe'):
        recognized = True
    return (json.dumps(words, separators=(',', ':')), reason) if recognized else None


def relevant(name):
    path = Path(name)
    if path.parts[0] in ('docs', '.artifacts', '.git', 'target', '.build-cache', '.codex', '.agents', '.aws'):
        return False
    if path.parts[0].startswith('bazel-') or path.name in ('README.md', 'AGENTS.md'):
        return False
    return (path.parts[0] in ('src', 'tests', 'scripts', 'fixtures', 'benches', 'examples',
                             'schemas', 'build_support', 'native', 'third_party', '.cargo',
                             'skills', 'test_support', 'bazel', 'platforms')
            or path.suffix in ('.rs', '.toml', '.bzl')
            or path.name in ('Cargo.lock', 'cargo-bazel-lock.json', 'build.rs', 'BUILD', 'BUILD.bazel', 'MODULE.bazel',
                             'MODULE.bazel.lock', 'WORKSPACE', 'WORKSPACE.bazel',
                             '.bazelrc', '.bazelversion', 'rust-toolchain'))


def repository(cwd):
    return Path(subprocess.check_output(['git', 'rev-parse', '--show-toplevel'],
                cwd=cwd, stderr=subprocess.DEVNULL).decode().strip())


def input_hash(cwd):
    cwd = repository(cwd)
    names = subprocess.check_output(['git', 'ls-files', '-c', '-o', '--exclude-standard', '-z'],
                                    cwd=cwd, stderr=subprocess.DEVNULL).split(b'\0')
    digest = hashlib.sha256()
    for raw in sorted(set(names)):
        if not raw or not relevant(os.fsdecode(raw)):
            continue
        path = cwd / os.fsdecode(raw)
        if path.is_symlink():
            raise ValueError('symlinked checkpoint input needs manual review')
        data = path.read_bytes() if path.exists() else b'\0DELETED'
        digest.update(len(raw).to_bytes(8, 'big') + raw)
        digest.update(len(data).to_bytes(8, 'big') + data)
    return digest.hexdigest()


def atomic_json(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, separators=(',', ':')), encoding='utf-8')
    os.replace(temporary, path)


def response(event, message, deny=False):
    specific = {'hookEventName': event, 'additionalContext': message}
    if deny:
        specific.update(permissionDecision='deny', permissionDecisionReason=message)
    return {'hookSpecificOutput': specific}


def handle(payload):
    event = payload.get('hook_event_name')
    if event not in ('PreToolUse', 'PostToolUse'):
        return {}
    tool = payload.get('tool_input') or {}
    command = tool.get('command', tool.get('cmd', '')) if isinstance(tool, dict) else ''
    found = checkpoint(command) if isinstance(command, str) else None
    if found is None:
        return {}
    key, reason = found
    cwd = Path(payload.get('cwd', '.')).resolve()
    session = str(payload.get('session_id', 'missing'))
    use = payload.get('tool_use_id')
    if not use or session == 'missing':
        return response(event, REMINDER + ' Guard has no invocation/session identity; advisory only.')
    digest_key = lambda value: hashlib.sha256(value.encode()).hexdigest()
    key = digest_key(key)  # Receipts retain no command text or potentially private arguments.
    directory = repository(cwd) / '.artifacts' / 'checkpoint-guard' / digest_key(str(cwd)) / digest_key(session)
    directory.mkdir(parents=True, exist_ok=True)
    pending = directory / ('pending-' + digest_key(str(use)) + '.json')
    passed = directory / ('passed-' + digest_key(key) + '.json')
    if event == 'PreToolUse':
        fingerprint = input_hash(cwd)
        if reason:
            atomic_json(pending, {'key': key, 'fingerprint': fingerprint})
            return response(event, REMINDER + ' Explicit checkpoint reason accepted: ' + reason[:512])
        previous = json.loads(passed.read_text()) if passed.exists() else None
        if previous and previous.get('fingerprint') == fingerprint:
            return response(event, REMINDER + ' Repeated identical checkpoint denied: the same command '
                            'explicitly passed with unchanged source/build inputs in this session.', True)
        atomic_json(pending, {'key': key, 'fingerprint': fingerprint})
        return response(event, REMINDER)
    if not pending.exists():
        return response(event, REMINDER + ' No matching pre-checkpoint receipt; outcome unknown.')
    receipt = json.loads(pending.read_text())
    pending.unlink()
    result = payload.get('tool_response')
    # A session/running marker means completion is unknown, even with an apparent zero.
    asynchronous = isinstance(result, dict) and (
        any(result.get(k) is not None for k in ('session_id', 'cell_id'))
        or any(result.get(k) for k in ('running', 'is_running'))
        or result.get('status') in ('running', 'pending', 'started', 'in_progress', 'async'))
    success = (isinstance(result, dict) and type(result.get('exit_code')) is int
               and result['exit_code'] == 0 and not asynchronous)
    if receipt.get('key') != key or not success:
        # A failed/unknown newer attempt supersedes a prior pass; retries remain allowed.
        passed.unlink(missing_ok=True)
        return response(event, REMINDER + ' Checkpoint failed or completion is unknown; retry remains allowed.')
    if input_hash(cwd) != receipt['fingerprint']:
        passed.unlink(missing_ok=True)
        return response(event, REMINDER + ' Inputs changed during the checkpoint; no successful receipt retained.')
    atomic_json(passed, receipt)
    return response(event, REMINDER + ' Explicit successful checkpoint recorded for these inputs and arguments.')


def main():
    try:
        payload = json.load(sys.stdin)
        output = handle(payload)
    except Exception:
        event = payload.get('hook_event_name', 'PreToolUse') if 'payload' in locals() and isinstance(payload, dict) else 'PreToolUse'
        output = response(event, REMINDER + ' Checkpoint guard could not verify state; advisory only, no pass inferred.')
    print(json.dumps(output))


if __name__ == '__main__':
    main()
