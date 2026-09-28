# P05 dependency evidence

Root pinned `rusqlite = "=0.40.1"` with `default-features = false` and `bundled`. Permitted Cargo fetch added six lockfile packages; every pre-existing package version stayed unchanged. Dependency-only `cargo check --locked --offline` passed in 4.94s before the catalog modules were wired. This proves the pinned artifacts compile on this machine, not publication correctness.

Exact Cargo registry artifacts and license file SHA256s are retained in [P05-dependencies.json](P05-dependencies.json). The release rusqlite/sys manifests use Rust2021 and declare no MSRV. They differ from the previously inspected upstream master manifest. The sys artifact contains SQLite3.53.2 with FTS5 and THREADSAFE=1 compile flags in its build script. Runtime FTS5/tokenizer/THREADSAFE probes, configured WAL/FK/bounded timeout, pinned-reader, atomic publication and migration gates passed; actual evidence is in [P05-checks.json](P05-checks.json). Disabled default features avoid the wasm FFI/cache options; no external database service is required.

| Added package | Version | Declared license |
|---|---|---|
| rusqlite | 0.40.1 | MIT |
| libsqlite3-sys | 0.38.2 | MIT |
| fallible-iterator | 0.3.0 | MIT/Apache-2.0 |
| fallible-streaming-iterator | 0.1.9 | MIT/Apache-2.0 |
| pkg-config | 0.3.34 | MIT OR Apache-2.0 |
| vcpkg | 0.2.15 | MIT/Apache-2.0 |

SQLite's bundled amalgamation header disclaims copyright and dedicates the source to the public domain; the Rust sys wrapper is MIT. SQLCipher files shipped in that crate are unused by the selected feature set. This is the P05 incremental artifact inventory; P21 still owns the complete selected dependency/linkage and release inventory. No project license has been chosen and no dependency has been installed globally.
