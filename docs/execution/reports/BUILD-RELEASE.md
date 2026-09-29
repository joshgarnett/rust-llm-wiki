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
| Build orchestration and release verification regressions | 41 Python tests passed, including Windows discovery environment regression |
| Workflow syntax | Five YAML files parsed; this is not hosted execution |
| Full qualification attempt 001 | Failed under concurrent disk-heavy test targets: 24 passed, three failed, 26 skipped after explicit interruption |
| Serialized affected CLI workflows | Both targets passed uncached in 55.7 seconds |
| Full qualification attempt 002 | All stages passed; all 53 uncached test targets passed in 1862.16 seconds, optimized build in 69.64 seconds, optimized skill test in 5.5 seconds, copied-binary offline workflow and linkage checks passed |

Attempt 001 exhausted the existing elapsed budgets in skill/release workflows
and a mock listener's accept deadline in semantic retrieval. Tests now compile
before execution and Bazel schedules one test target at a time. Each Rust
harness retains its internal concurrency and all existing deadlines/assertions.
The complete serialized run passed, including all three previously affected
targets. The clean native candidate records commit
`06aa1a9526dc59569f637001d59493ebaebd213c`; its binary SHA-256 is
`538a5bd2baddf069531f6edf059b7401458639a07c75065ac8190665fd3728fe`.

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

GitHub Actions is enabled on the private repository. Commit `0e53983` was
pushed to `main`; release run [36559473261](https://github.com/joshgarnett/rust-llm-wiki/actions/runs/36559473261)
found two clean-host setup defects: macOS had no Go for the Bazelisk install,
and Bazel rejected `--version` after startup options. Shared setup now downloads
the pinned native Bazelisk asset and verifies its checksum; doctor and candidate
metadata use `version --gnu_format`. All 40 Python regressions still pass.
The corrected run [36559942359](https://github.com/joshgarnett/rust-llm-wiki/actions/runs/36559942359)
passed macOS ARM64, but Windows x64/ARM64 could not locate their MSVC tools.
Astra reviewed the tool-discovery code: setup now selects Visual Studio through
`vswhere`, checks the native compiler/linker paths and exports `BAZEL_VC` before
Bazel starts. The wrapper also retains Windows installation directory variables.
Unix jobs continue; hosted acceptance remains pending.
The following Windows CI run confirmed compiler discovery but exposed a
264-character Rust standard-library linker input (`LNK1181`). Windows setup
now uses a short output root on the runner's system drive, as recommended by
the Bazel Windows guide. This changes orchestration, not Rust source/tests.
Release run [36562096258](https://github.com/joshgarnett/rust-llm-wiki/actions/runs/36562096258)
confirmed Windows ARM64 compiles past bootstrap, then exposed `ring` selecting
Clang while receiving MSVC C flags. Sol and Astra identified a scoped fix:
patch only ring 0.17.14's build script, guarded by Windows/ARM64/MSVC plus an
explicit marker, to clear generic CFLAGS before compiler detection. SDK,
librarian and linker settings remain intact. Bazel's mapping was regenerated;
Cargo.toml/Cargo.lock and all dependency versions are unchanged. The updated
native macOS `just ci` gate passed, including 41 tooling tests and all smoke
targets. Native Windows ARM64 confirmation remains pending.
Windows x64 completed all Rust source/test compilation in run36562096258, then
found a fixture-only packaging regression: text-mode writes made a mock lockfile
CRLF while its fixed expected digest assumed LF. The fixture now writes exact
bytes; production hashing already used the correct on-disk bytes.
A release is accepted only after all six jobs succeed, downloaded
archives pass checksum/member/metadata/binary checks, and the draft targets
that exact validated commit. No failed-job artifact is promoted. The draft
will remain unpublished; live providers, installed host applications, signing
and Windows durability remain outside this build acceptance.
