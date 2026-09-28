# P00 independent contract review

Reviewed 2026-09-28 after root's freeze signal on `impl/autonomous-v1`, baseline `a97679c`. Scope: shared primitives/records, public record/output schemas, CLI envelopes, tests and bootstrap fixtures. No source/test mutation or Cargo invocation by reviewer.

## Findings requiring root disposition

1. **P2 — malformed CLI arguments can panic.** `src/main.rs:37` uses `std::env::args()` while handling clap errors. On this Unix host, invoking the built binary with arguments `[b'\xff', b'--json']` exits **101**, prints a Rust panic to stderr, and emits no JSON. The same happens without `--json`; placing `--json` first happens to avoid it through iterator short circuit. Use `args_os()` for flag detection. Add Unix tests for invalid UTF-8 before/after `--json` and human mode; require exit 2 and the expected protocol.

2. **P2 — public evidence-list schema accepts malformed values.** `schemas/record-v1.json:310` validates `wiki_evidence` items as unrestricted strings. A valid assertion amended with `wiki_evidence: ["not a wikilink"]` passes the actual `jsonschema` validator while `CanonicalRecord::from_value` rejects it. Reproduced against frozen compiled dependencies using a temporary Rust program. Apply the companion-link pattern to list items and add shared negative cases. Portable-path and cross-field constraints explicitly documented as semantic-only are not findings here.

3. **P2 — fixture vault sentinel has wrong case.** `scripts/seed-fixture.py:154` emits `vault/wiki.md`, and the manifest records that exact spelling. The CLI contract requires `WIKI.md`. Observed on disk; failure of future discovery on a case-sensitive filesystem is inferred, not tested. Emit exact `WIKI.md`, regenerate, and test the directory-entry spelling independently of the host's case sensitivity.

## Evidence and boundaries

The deterministic fixture check passed: `python3 scripts/seed-fixture.py --hash-command target/debug/examples/fixture_hash --check` verified 34 files, manifest SHA-256 `a7f5997914cf3499e85b160420dcc29de302b2d26a8649e437f238d6529aa6c8`. Root reported all eight tests, clippy, and build passing; reviewer did not repeat those suites. No advertised command stubs found.

P01 integration note: raw `serde_json::from_str::<CanonicalRecord>` silently chooses the last duplicate key (observed `wiki_id` first/last → `last`), because its map deserializer loses duplicates. P01 must reject duplicates before map construction, including nested unknown metadata; this is not a demand to implement the YAML parser in P00.

No lifecycle, cross-record activation, native crash-safety, or live-provider qualification is claimed. Root owns finding disposition and post-fix verification.

## Focused fix re-review

All three P00 findings are **resolved** by source inspection on 2026-09-28:

1. JSON flag detection now uses `args_os()`, avoiding UTF-8 conversion. The Unix regression places an invalid UTF-8 schema argument before `--json` and checks exit 2, empty stderr, USAGE, and schema-valid JSON.
2. `wiki_evidence.items` now carries the companion-wikilink pattern. The new test checks both public-schema and canonical-validator acceptance/rejection of a valid link and plain string.
3. Generator, actual directory-entry spelling, and manifest now use exact `WIKI.md`. The fixture test checks `read_dir().file_name()` against that spelling, so a case-insensitive host cannot mask the original mistake.

Root reports the expanded all-targets suite passing ten tests; this bounded re-review inspected source, test assertions, and the actual fixture filename/manifest without rerunning Cargo. No P00 findings remain open. The P01 duplicate-key integration note remains applicable to that later parser package.
