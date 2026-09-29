# Builds and CI

Just provides the command interface; Bazel builds, tests, checks formatting
and runs Clippy. This follows the sibling `rust-ai-gateway` and `rustleaks`
workflows. `crate_universe` derives the Bazel dependency mapping from the
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

`deps-fetch` hydrates the locked Bazel modules, toolchains and Cargo-derived
dependency graph. Normal build/test commands pass `--nofetch`; this prevents
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
| `just test-smoke` | Runfile paths, contracts, machine output, offline CLI and skill export |
| `just build-tools-test` | Build orchestration regressions with fake tools |
| `just ci` | Check, build-tool regressions and CLI smoke subset |
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
Cargo remains available for metadata, manifest/lock maintenance and `format-fix`.

## Candidate artifacts

The candidate helper builds `//:lwiki` through the native Bazel wrapper.
It queries `cquery --output=files` and `info execution_root` with the same
optimized configuration to locate the executable, including paths with spaces
or non-ASCII characters. It does not assume a fixed `bazel-bin` location.

Every invocation requires a new output directory. It retains build logs, runs
the copied binary's version and capability commands, and creates
`lwiki-VERSION-TARGET.tar.gz` and a SHA-256 sidecar. The archive contains the executable, `version.txt`, `capabilities.json`
and `build-info.json`. The archive preserves executable permissions. The info
file records the native target, actual Bazel version, Rust/module pins, build
command, existing lockfile/build-contract and binary SHA-256, Git commit and
dirty status. Failed builds retain diagnostics without a successful
manifest/archive. Qualification adds offline capture, literal
search, context, index rebuild, vault check and skill-export assertions.

Source metadata and hashes are informational provenance, not dependency audits
or reproducibility proofs. HEAD/status and build-contract hashes are compared
before/after; unchanged dirty status cannot detect every concurrent content edit.
Keep the checkout quiescent while collecting evidence. Qualification additionally
runs the copied binary with a minimal system PATH. Name, license and distribution
decisions remain in [external qualification](qualification.md).

## GitHub workflows

- `CI` runs on pull requests, pushes to `main`, and manual dispatch. Linux and
  macOS run the short gate and build candidate archives. Windows checks all
  targets and produces a build/capability candidate; it does not qualify
  durable writes or crash recovery. Windows has a 60-minute job allowance for
  setup, full source compilation and an optimized candidate; Unix jobs use 30 minutes.
- `Opt-in native qualification` remains manual. It runs full Unix qualification
  and retains its logs and candidate archive. Windows remains build-only.
- `Release build` manually builds six native targets from one workflow commit:
  Linux GNU, macOS and Windows, each on x64 and ARM64. Unix jobs run the short
  gate and optimized skill recipe; Windows jobs compile all test sources and
  smoke the copied binary's capabilities. Each job uploads a versioned archive,
  checksum and build information. Different commits may build concurrently;
  duplicate runs of the same commit serialize. Release creation remains a
  separate step after all six artifacts from one successful run are verified.
- Shared setup installs pinned Rust and Just, downloads the pinned native
  Bazelisk binary with its checksum, caches downloaded dependency archives and Just by OS,
  architecture, toolchain and lockfiles, then fetches the locked Bazel graph.
  Compiled targets are not cached across hosted runs.
- Actions are pinned to full commit IDs, checkout credentials are not persisted,
  and workflows have read-only repository permissions. Candidate artifacts
  from regular CI expire after 7 days; release-build and full qualification
  evidence after 14 days. Draft release assets are separate from that retention.
- Dependabot groups weekly Cargo and GitHub Actions updates, with at most one
  open update per ecosystem. Updates still require review; no automatic merge
  or extensive dependency-audit process is configured.

The repository is [joshgarnett/rust-llm-wiki](https://github.com/joshgarnett/rust-llm-wiki).
All six native jobs passed in [release run 36564578331](https://github.com/joshgarnett/rust-llm-wiki/actions/runs/36564578331)
at commit `7f1c03d829d587763a82e7f660cc7b19f2a40d57`. The
[draft v0.1.0 release](https://github.com/joshgarnett/rust-llm-wiki/releases/tag/untagged-1fb57f8f77dbb46a0898)
targets that commit and contains six archives plus six SHA-256 sidecars.
Uploaded GitHub digests and downloaded copies match the verified build files.
The [build report](execution/reports/BUILD-RELEASE.md) and
[machine-readable checks](execution/reports/BUILD-RELEASE-checks.json) record
the jobs, hashes and limits. These hosted gates do not run the full recovery
suite; Windows vault writes remain unsupported. Publishing, signing and
branch-protection settings remain owner decisions.

For subsequent releases, create a **draft** only after all six release jobs pass and the downloaded archives match
their checksums, embedded binary hashes, target, version and workflow commit.
Artifacts retained from failed jobs are diagnostics, not accepted releases.
Keep the current Windows write limitation explicit in release notes until it is
implemented and qualified.

Download a successful release run with `gh run download RUN_ID --dir NEW_DIR`.
The directory must contain the six separate job artifact directories. Run
`python3 scripts/verify_release.py NEW_DIR --commit FULL_COMMIT --version 0.1.0`
to check all six archives and print the twelve archive/checksum asset paths.
The verifier checks archive members without extracting or executing them. It
does not query GitHub job status; confirm every job succeeded before using its
output, and target the draft release at that same full commit.
