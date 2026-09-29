"""First-party Rust targets sharing the locked Cargo dependency graph."""

load("@crates//:defs.bzl", "aliases", "all_crate_deps")
load("@rules_rust//rust:defs.bzl", "rust_binary", "rust_library", "rust_test")

_EDITION = "2024"
_VERSION = "0.1.2"
_CARGO_ENV = {
    # Bazel starts tests in the main repository's runfiles directory.
    "CARGO_MANIFEST_DIR": ".",
    "CARGO_PKG_NAME": "rust-llm-wiki",
    "CARGO_PKG_VERSION": _VERSION,
}

def owned_library(name, crate_name, srcs, compile_data = []):
    """Defines the library with Cargo's normal and procedural-macro dependencies."""
    rust_library(
        name = name,
        aliases = aliases(),
        compile_data = compile_data,
        crate_name = crate_name,
        crate_root = "src/lib.rs",
        deps = all_crate_deps(normal = True),
        edition = _EDITION,
        proc_macro_deps = all_crate_deps(proc_macro = True),
        rustc_env = _CARGO_ENV,
        srcs = srcs,
        version = _VERSION,
    )

def owned_binary(name, src, deps = []):
    """Defines a CLI or example executable using the same Cargo package."""
    rust_binary(
        name = name,
        aliases = aliases(),
        crate_name = name,
        crate_root = src,
        deps = all_crate_deps(normal = True) + deps,
        edition = _EDITION,
        proc_macro_deps = all_crate_deps(proc_macro = True),
        rustc_env = _CARGO_ENV,
        srcs = [src],
        version = _VERSION,
    )

def owned_crate_test(name, crate, compile_data = [], data = [], timeout = "long"):
    """Recompiles a first-party crate under cfg(test), including dev dependencies."""
    rust_test(
        name = name,
        aliases = aliases(normal = True, normal_dev = True),
        compile_data = compile_data,
        crate = crate,
        data = data,
        deps = all_crate_deps(normal = True, normal_dev = True),
        proc_macro_deps = all_crate_deps(proc_macro = True, proc_macro_dev = True),
        rustc_env = _CARGO_ENV,
        timeout = timeout,
        version = _VERSION,
    )

def owned_integration_test(name, src, shared_srcs = [], compile_data = [], data = []):
    """Defines one Cargo integration-test root with runfiles for its CLI/fixtures."""
    rust_test(
        name = name,
        aliases = aliases(normal = True, normal_dev = True),
        compile_data = compile_data,
        crate_name = name,
        crate_root = src,
        data = data + [":lwiki"],
        deps = all_crate_deps(normal = True, normal_dev = True) + [":lwiki_lib"],
        edition = _EDITION,
        proc_macro_deps = all_crate_deps(proc_macro = True, proc_macro_dev = True),
        # rules_rust exposes the executable data dependency as
        # CARGO_BIN_EXE_lwiki. Shared test support resolves it before child cwd
        # changes, preserving Cargo behavior in Bazel's runfiles layout.
        rustc_env = _CARGO_ENV,
        srcs = [src] + shared_srcs,
        timeout = "long",
        version = _VERSION,
    )
