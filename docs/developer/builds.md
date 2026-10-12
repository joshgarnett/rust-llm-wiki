# Builds and CI

Just provides the command interface; Bazel builds, tests, checks formatting
and runs Clippy. The sibling `rustleaks` repository provides the release
workflow reference. `crate_universe` derives the Bazel dependency mapping from the
existing Cargo manifest and lockfile.

## Local commands

Install rustup, Python 3.9 or newer, Just 1.58.0 and Bazelisk 1.29.0.
The repository pins Bazel 9.2.0, rules_rust 0.72.0 and Rust 1.98.0.
A native C compiler/linker and platform SDK are required for bundled SQLite
and ring. Windows commands also require Git Bash and the MSVC/Windows SDK.
Python, Just and Bazelisk are development tools, not binary runtime dependencies.
On Windows, set `LWIKI_BAZEL_OUTPUT_ROOT` to a short writable path such as
`C:/lwb` before invoking Just. MSVC can reject long linker input paths; this
follows [Bazel's Windows guidance](https://bazel.build/configure/windows).
GitHub setup selects that short output root and discovers MSVC explicitly.

```sh
just doctor
just deps-fetch
just ci
just candidate .artifacts/candidate-001
```

`deps-fetch` downloads locked Cargo archives for dependency notices, then the
locked Bazel modules, toolchains and Cargo-derived dependency graph. Normal build/test commands pass `--nofetch`; this prevents
repository fetching but is not an absolute network sandbox for actions or tests.
After intentional Cargo manifest/lock edits, use `just deps-repin` to regenerate
the Bazel mapping, then commit the updated lockfiles. This is lock maintenance,
without an additional dependency-audit process.
Cargo updates proposed by Dependabot also need this mapping refresh.
The Bazel graph carries one small `ring` 0.17.14 build-script patch for Windows
ARM64: that crate selects Clang, so its build discards inherited MSVC C flags
while preserving SDK/linker settings. The patch requires an explicit target
marker and leaves other targets and ordinary Cargo dependency sources unchanged.

| Command | Scope |
| --- | --- |
| `just` | List maintained commands |
| `just build` / `just release` | Development / optimized Bazel build |
| `just format` / `just format-fix` | Bazel formatting check / Cargo formatting edit |
| `just check` | Formatting and strict Clippy for all targets |
| `just test-smoke` | Optimized runfile paths, contracts, machine output, offline CLI and skill export |
| `just build-tools-test` | Build orchestration regressions with fake tools |
| `just ci` | Check, build tools, CLI smoke and consequential regression subsets |
| `just test` | Library, binary, integration, example tests and doctests |
| `just candidate NEW_DIR` | Native release archive and copied-binary smoke |
| `just qualify [NEW_DIR]` | Full Unix qualification, including release recipes and disposable-vault checks |

The short CI gate omits the long fault/recovery matrices. Full qualification
runs Bazel tests with cached test results disabled, including the library,
crash/recovery matrices, doctests and release-profile skill recipe. It prints
its fresh evidence directory first, identifies failures by stage and collects
the retrieval baseline archive from Bazel's `TEST_UNDECLARED_OUTPUTS_DIR`.
With no output argument it creates a unique physical temporary directory.
Tests use disposable vaults and mock providers.
Full-suite recipes compile test targets before execution. Bazel runs one test
target at a time so disk-heavy recovery fixtures do not compete for elapsed
budgets; each Rust harness retains its internal thread/concurrency tests.

`scripts/bazel.py` selects explicit native build and host platforms, ignores
ambient Cargo cross-compilation settings and uses the repository's Bazel config.
For one test, run `python3 scripts/bazel.py -- test --nofetch //:offline_cli_test`.
The wrapper pins the macOS deployment target to 15.0 for Rust and native
compilation. Without that setting, Bazel's Apple toolchain defaults to the SDK
version, which can raise the runtime requirement on a newer build machine.
The packaged executable's recorded minimum must match the supported range.
Cargo remains available for metadata, manifest/lock maintenance and `format-fix`.

## Candidate artifacts

The candidate helper builds `//:lwiki` through the native Bazel wrapper.
It queries `cquery --output=files` and `info execution_root` with the same
optimized configuration to locate the executable, including paths with spaces
or non-ASCII characters. It does not assume a fixed `bazel-bin` location.

Every invocation requires a new output directory. It retains build logs, runs
the copied binary's version and capability commands, and creates
`lwiki-VERSION-TARGET.tar.gz` and a SHA-256 sidecar. The archive contains the executable, `version.txt`, `capabilities.json`
and `build-info.json`, plus `LICENSE`, `THIRD-PARTY-NOTICES`, `INSTALL.txt` and
`artifact-info.txt`. The archive preserves executable permissions. The info
file records the native target, actual Bazel version, Rust/module pins, build
command, existing lockfile/build-contract and binary SHA-256, Git commit and
dirty status. Failed builds retain diagnostics without a successful
manifest/archive. Qualification adds offline capture, literal
search, context, index rebuild, vault check and skill-export assertions.

Source metadata and hashes are informational provenance, not dependency audits
or reproducibility proofs. HEAD/status and build-contract hashes are compared
before/after; unchanged dirty status cannot detect every concurrent content edit.
Keep the checkout quiescent while collecting evidence. Qualification additionally
extracts the archived executable, verifies its hash against the staged binary and
runs integration tests against that exact file. It checks the hash again afterward
and runs the offline smoke workflow with a minimal system PATH. See
[release qualification](qualification.md) for native platform and trust checks.

## GitHub workflows

- `CI` runs on pull requests, pushes to `main`, and manual dispatch. Linux and
  macOS run the short gate and build candidate archives. Windows compiles the
  source and test targets. It produces no release archive and does not qualify
  durable writes or crash recovery.
- The short Unix gate includes `just test-regressions`: provider, source,
  graph/context, research, shared workflow and storage cases, plus bounded
  conflict-resolution/checkpoint regressions. `just qualify` adds the full
  native interruption suite.
- `Opt-in native qualification` runs full Unix qualification on manual dispatch
  and retains its logs and archive.
- `Release build` builds the final archives once, from one clean workflow commit,
  on Ubuntu 24.04 and macOS 15. It verifies both archives, then tests the same
  extracted executable on Ubuntu 24.04 and macOS 15, 26 and 27. Ubuntu and Mac 15
  run full correctness and recovery suites plus installation and an own-note
  workflow. Mac 26 and 27 run the installed workflow, agent-file lifecycle and
  linkage checks without Rust/Bazel setup or compiled suites. Final staging
  requires both full rows and both newer-Mac installed rows with exact source and
  executable hashes; a successful full report retains its scope when used for an
  installed row. Different commits may build concurrently;
  duplicate runs of one commit serialize.
- Publication is a separate step after native qualification, installed workflows
  and applicable OS trust checks pass for the final executable bytes. Short CI,
  cross-builds and a separate build from the same commit do not establish those
  results. A successful build does not authorize publication.
- Shared setup installs pinned Rust and Just, downloads the pinned native
  Bazelisk binary with its checksum, caches downloaded dependency archives and Just by OS,
  architecture, toolchain and lockfiles, then fetches the locked Bazel graph.
  Compiled targets are not cached across hosted runs.
- Actions are pinned to full commit IDs, checkout credentials are not persisted,
  and workflows have read-only repository permissions. Candidate artifacts
  from regular CI expire after 7 days. Release-build archives and evidence expire
  after 30 days; separate opt-in qualification evidence expires after 14 days. Draft release assets are separate from that retention.
- Dependabot groups weekly Cargo and GitHub Actions updates, with at most one
  open update per ecosystem. Updates still require review; no automatic merge
  or extensive dependency-audit process is configured.

The repository is [joshgarnett/rust-llm-wiki](https://github.com/joshgarnett/rust-llm-wiki).
Use [validation and limits](validation-status.md) to choose checks for the final
artifact. macOS distribution is unsigned and is not notarized; test normal OS
trust behavior and native workflows against the final executable.

CLI smoke and consequential regression tests use the release configuration.
Lookup deadlines apply to the shipped optimized executable; unoptimized timings
are diagnostic and do not establish supported wiki sizes. Format and lint checks
remain separate.
