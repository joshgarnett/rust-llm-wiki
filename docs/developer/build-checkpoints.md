# Build and test checkpoints

Finish a named, user-visible feature batch before spending time on Rust builds or
integration tests. First inspect whether higher-impact authorized work remains.
Use cheap focused checks during editing, then one grouped checkpoint for the
integrated workflow. Repeating an unchanged successful command is usually wasted
work. Do not make arbitrary edits merely to unlock a build.

`scripts/checkpoint_guard.py` is a conservative PreToolUse/PostToolUse hook. It
reads one JSON event from stdin and returns hook-specific JSON. Configure that
command for both events on Bash in the project's hook definition. Project hooks
must be enabled and the user must trust the exact definition through `/hooks`;
adding this script alone does not activate it. The root agent owns configuration.

The guard recognizes direct Cargo build/test/check/clippy/bench commands, direct
Bazel/Bazelisk build/test, `python3 scripts/bazel.py -- build|test`, and executables
named `unit_tests` or `offline_cli_test`. Recognized checkpoints receive a batch reminder; unrelated commands are ignored.
An identical command is denied only when an earlier matching attempt in the same
working directory and session explicitly returned integer `exit_code: 0`, its
inputs did not change while running, and those inputs remain unchanged now.

Input fingerprints cover tracked and new unignored source, tests, fixtures,
scripts, skills, test support, Bazel/platform inputs, schemas, common build files and configuration, enumerated with read-only
Git commands from the repository root. Docs, artifacts, generated/build trees,
session state and private agent/provider configuration do not unlock a repeat.
Receipts live under ignored `.artifacts/checkpoint-guard/`, separated by working
directory and session. Commands, argument text, tool output and credentials are
not stored; command identities are hashed. Exact arguments, filters, targets,
profiles and executable spelling remain distinct; shell quoting is normalized.

Failed or unknown matching outcomes supersede a prior pass and permit a retry.
Running/session markers, absent exit codes, strings and other unrecognized
responses are unknown. Source changes during execution prevent recording a pass.
Missing identity or guard errors are advisory and never establish success.

An intentional repeat needs a concrete inline reason, for example:

```sh
LWIKI_CHECKPOINT_REASON='final acceptance after environment repair' cargo test --release --lib
```

This permits the command without asking the user for routine permission. The
reason should explain final acceptance, changed environment, or a correctness
regression; it is not a substitute for meaningful feature progress.

This is a guardrail, not a shell parser or a build dependency engine. Compound
shell commands, additional inline environment settings, indirect scripts, Cargo
toolchain wrappers, other test executable names and alternative wrapper spelling
may remain advisory. Ambient compiler/environment changes are not fingerprinted;
use an explicit reason when they justify repeating a command. It does not block
a new test group, enforce a minimum change size, decide what feature to build, or
prove that a reported successful command was sufficient acceptance.

Run the inexpensive verification with:

```sh
python3 scripts/test_checkpoint_guard.py
```

The tests mock Git discovery, use real disposable input bytes and exercise pass
receipts, changes, exclusions, failure/unknown outcomes, distinct arguments,
overrides, subdirectory hashing and recognized/advisory command shapes. They do
not establish hook runtime installation or user trust.
