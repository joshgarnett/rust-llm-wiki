# Just, Bazel and the first GitHub release

Status: in progress, 2026-09-29. This follow-up implements the owner's explicit
Just + Bazel choice and authorization to commit, push and create a **draft**
`v0.1.0` release for six native targets. It does not change the frozen P21
acceptance report or repeat the sibling project's dependency audit.

## Scope

- Bazel 9.2.0, rules_rust 0.72.0 and Rust 1.98.0; Cargo manifest and lockfile
  remain the dependency source. Cargo dependency versions are unchanged.
- Just commands own the build interface. Bazel builds, formats, lints and tests
  all first-party Rust targets, including examples and doctests.
- Shared test support resolves Cargo executable paths and Bazel runfiles.
  Product behavior and recovery/accounting assertions are unchanged.
- Candidate packaging discovers the optimized executable through Bazel queries,
  copies and smokes it, and writes target/version archives with SHA-256 sidecars.
- GitHub CI, manual full qualification and manual six-platform release builds
  use pinned actions and read-only repository permissions.
- Linux GNU, macOS and Windows each have x64 and ARM64 native jobs. Windows
  artifacts are build/capability evidence; vault writes remain unsupported.

## Local evidence

| Check | Observation |
| --- | --- |
| Native `//:build` | Passed; 866 actions, 78.57 seconds |
| `//:check //:smoke` | Seven targets passed; formatting, strict Clippy and 32 Rust smoke cases |
| Build orchestration and release verification regressions | 40 Python tests passed |
| Workflow syntax | Five YAML files parsed; this is not hosted execution |
| Full qualification attempt 001 | Failed under concurrent disk-heavy test targets: 24 passed, three failed, 26 skipped after explicit interruption |
| Serialized affected CLI workflows | Both targets passed uncached in 55.7 seconds |
| Full qualification attempt 002 | Running; library 136 passed, zero failed, three ignored helpers; final integration/release result pending |

Attempt 001 exhausted the existing elapsed budgets in skill/release workflows
and a mock listener's accept deadline in semantic retrieval. Tests now compile
before execution and Bazel schedules one test target at a time. Each Rust
harness retains its internal concurrency and all existing deadlines/assertions.
The complete serialized run must pass before local acceptance.

Local logs are retained in `.artifacts/bazel-check-smoke.log`,
`.artifacts/bazel-serial-workflows.log`, `.artifacts/build-tools-tests.log`, and
`/private/tmp/lwiki-bazel-qualification-{001,002}`. These are local evidence,
not tracked binaries. Earlier Cargo-based candidate runs belong to the
superseded build adaptation and are not used for Bazel acceptance.

## Review and hosted acceptance

Astra reviewed the graph, test runfiles, packaging, scheduling failure and
six-platform release workflow. Findings were closed through actual artifact
discovery, fresh output directories, failure diagnostics, explicit UTF-8,
Windows environment-key handling, native argument placement and test-target
serialization. Final release workflow review found no blocking issue. A separate
archive-verifier review added an executable-permission check for Unix binaries;
its regression passes alongside malformed archive and metadata rejection cases.

GitHub Actions is enabled on the private repository. Hosted execution is
pending. A release is accepted only after all six jobs succeed, downloaded
archives pass checksum/member/metadata/binary checks, and the draft targets
that exact validated commit. No failed-job artifact is promoted. The draft
will remain unpublished; live providers, installed host applications, signing
and Windows durability remain outside this build acceptance.
