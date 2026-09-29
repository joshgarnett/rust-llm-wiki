set default-list
set shell := ["bash", "-euo", "pipefail", "-c"]
set windows-shell := ["bash", "-euo", "pipefail", "-c"]

python := if os() == "windows" { "python" } else { "python3" }

# Report installed tools; this does not install or fetch dependencies.
doctor:
    just --version
    rustup show active-toolchain
    rustc -vV
    cargo -vV
    {{ python }} --version
    {{ python }} scripts/bazel.py -- version --gnu_format

# Fetch the pinned Bazel modules, toolchains and Cargo-derived dependencies.
deps-fetch:
    {{ python }} scripts/bazel.py -- fetch //...

# Regenerate Bazel's Cargo mapping after intentional Cargo manifest/lock edits.
deps-repin:
    CARGO_BAZEL_REPIN=1 {{ python }} scripts/bazel.py -- mod deps --lockfile_mode=update

# Check Rust formatting.
format:
    {{ python }} scripts/bazel.py -- test --nofetch //:format

# Apply Rust formatting.
format-fix:
    cargo fmt --all

# Build the development executable.
build:
    {{ python }} scripts/bazel.py -- build --nofetch //:build

# Build the optimized executable.
release:
    {{ python }} scripts/bazel.py -- build --nofetch --config=release //:lwiki

# Compile every target and deny Clippy warnings.
check:
    {{ python }} scripts/bazel.py -- test --nofetch //:check

# Run the full Bazel suite, including doctests (disposable vaults/mock providers).
test:
    {{ python }} scripts/bazel.py -- build --nofetch //:test_build
    {{ python }} scripts/bazel.py -- test --nofetch //:test

# Fast CLI/schema/offline/export checks; excludes long recovery matrices.
test-smoke:
    {{ python }} scripts/bazel.py -- test --nofetch //:smoke

# Check build scripts without compiling Rust or using the network.
build-tools-test:
    {{ python }} -m unittest discover -s scripts -p 'test_*.py' -v

# Same short gate as Unix PR CI. Full acceptance is `just qualify`.
ci: check build-tools-test test-smoke

# Build and smoke a native candidate archive in a new directory; no publication.
candidate output:
    {{ python }} scripts/build_artifact.py {{ quote(output) }}

# Full native Unix qualification, with a fresh evidence directory by default.
[unix]
qualify output="":
    scripts/qualify-local.sh {{ quote(output) }}
