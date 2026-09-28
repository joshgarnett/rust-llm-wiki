# Implementation evidence and completion gates

All gates are **pending** at planning time. This matrix defines the evidence needed for M0–M4 local implementation completion. The work-package reports record actual results. External qualification is tracked separately in [DECISIONS.md](DECISIONS.md), never silently converted into a pass.

## Gate matrix

| Gate | Required behavioral evidence | Relevant design |
|---|---|---|
| V01 Build and contracts | Real library/binary build; pinned usable toolchain/lockfile; exact JSON envelope/errors/schema versions; unknown commands/capabilities not advertised | CLI/skills, handoff |
| V02 Lossless records | Comments/unknown fields/body/BOM/CRLF survive authorized edits; invalid/duplicate YAML keys, aliases, unsupported versions, duplicate IDs and ambiguous links diagnosed | Storage, record schemas |
| V03 Recoverable writes | Faults at actual stage/fsync/journal/replace/publish boundaries; recover old/new states, preserve third-hash edits; lock serialization; abort/rollback checks; no false multi-file atomicity | Storage |
| V04 Capture and citations | Immutable originals/revisions; multibyte UTF-8 spans and hashes; ambiguous quotation rejection; direct source citation without fabricated assertion; exact successor revalidation | Storage, schemas |
| V05 Catalog and freshness | FTS5 built in; transactionally published generation/FTS view; active reader consistency; new duplicate ID/decision detection; same-size/same-timestamp edits; cache deletion/rebuild without model calls | Storage |
| V06 Offline retrieval | Literal identifiers and FTS-safe query escaping; deterministic pagination/ties; bounded graph traversal; direction/qualifier preservation; byte/token context caps; no invented transitive assertion | Retrieval, CLI |
| V07 Evidence lifecycle | Loss of one/all supports; stale source head; withdrawal/tampering; full-note dependency invalidation; entity identity versus description; honest historical/snapshot/current scopes | Storage, retrieval |
| V08 Host graph workflow | Persist packet → import → apply → resolve → apply → review → apply; homonyms separate; malformed/unknown packet fails; repeated pre/post-apply import idempotent; merge/split/alias remaps; complete active-evidence review | Retrieval, schemas |
| V09 Skill and machine UX | Real skill export, maintained examples execute against binary; capability-aware commands; JSON/JSONL parse cleanly; documented errors/cancellation; no overwrite of existing host instruction files | CLI/skills, agent integration |
| V10 Dispatch/auth | Real HTTP encode/decode over test transport; exact URL/path/query/model; static key/env/file and dynamic helper; custom headers/TTL/401; no credential leakage; profile trust; redirects and TLS policy | Providers/jobs |
| V11 Budgets and replay | Separate reservation per attempt; concurrent final-slot race; unknown billing retained; checked monetary bounds; invalid/truncated response receipts; response-spool replay; no hidden retries; cancel/resume/deadline | Providers/jobs |
| V12 Embeddings/hybrid | Validate reordered/missing/nonfinite/wrong-dimension vectors; short notes whole; deterministic long splits; exact cosine order; RRF; cache reuse; no stale/mixed spaces; active-space query reproducibility | Retrieval, providers |
| V13 Direct extraction | Production generation adapter uses the same strict packet/import/review pipeline; malformed/refused/truncated output cannot activate knowledge; resume/cache reuse without losing decisions | Retrieval, providers |
| V14 Acquisition | Search pagination/dedup; explicit URL path; real bounded HTML/text normalization; immutable original capture; DNS/redirect/IP and expansion-size checks; unsupported sources reported | Providers/jobs, storage |
| V15 Research lifecycle | Plan/run/status/resume/report through real planner/dispatcher/storage using mock endpoints; versioned bounded frontier/gap/synthesis outputs; reject malformed/unknown fields, fabricated citations, over-limit outputs and scope/budget/apply changes; bounded rounds/tasks/deadline; no-progress stop; deterministic partial report when budget exhausted; explicit apply behavior | Providers/jobs, CLI |
| V16 Network absence | Counting transport/DNS/helper proves no external work for ordinary local commands, `--offline`, or `--dry-run`; dry-run also leaves filesystem/index/journal unchanged | All components |
| V17 Final local qualification | Full supported command workflow, relevant fault suite, format/lint/tests, current-platform release build/artifact smoke test, clean capability/requirement coverage, independent review findings resolved | All components |

Every gate must map to concrete test names and package reports by final qualification. A single meaningful test may cover multiple gates; avoid duplicating it solely to fill rows. No test is required for an inconsequential prose edit, but persistent-state/network/cost logic needs behavioral tests.

## Test architecture

Use isolated temporary directories for vault/config/home fixtures; never repurpose the real `HOME` or `CODEX_HOME` in shell setup. Pass explicit task-specific test paths/config objects. Fake credentials are fixed test strings; dynamic-auth tests use a bounded helper fixture whose invocation count can be asserted. No test discovers ambient keys or falls back to a live provider.

Inject transport, clock, jitter, cancellation, and filesystem faults at the narrow production interfaces. Also test real serialization/HTTP handling against a local mock listener so a pure mock trait does not hide wire errors. The explicit loopback test policy is not a relaxation of production public-fetch rules. Test public destination validation through controlled DNS/connection abstractions without fetching arbitrary public/private services.

Use known-order synthetic vectors to test math, space isolation, invalidation, and fusion. They do not establish real semantic recall. Keep a representative small labeled vault for lexical and graph evaluation; label exact original spans and multi-edge evidence sets. Real embedding/model evaluation is an E01/E04 qualification item, not a reason to ship untested vector math.

Research models return the versioned bounded contracts specified in P20: frontier query/URL proposals, gap/coverage/continuation proposals, and claim-to-`CitationRef` report/change proposals. Root integrates their public schemas; production validators enforce them before dispatching follow-up work or staging output. Test through the real generation adapter with local mock responses. Prompt wording and canned fixture branches are not validators. Citation checks establish exact referenced bytes and allowed source identity, not semantic entailment; report that limitation and retain unverified claims as proposals/gaps rather than treating a valid hash as proof of truth.

Failure injection must exercise the production state machine, including interruptions after durable intent but before completion. Unit-only simulations are useful but cannot replace current-platform subprocess/real-file recovery checks. The external-editor post-check race remains a documented limit; tests must not assert a guarantee the filesystem cannot provide.

## Practical commands and evidence

Once Cargo exists, the expected root-owned integration checks are:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
```

Add `--offline` for reproducible test/build runs after dependencies are resolved when feasible. A missing cache is a dependency setup issue, not a failing application test. Do not advertise successful runs before they execute. Optional features, doctests, platform-specific suites, and schema/skill checks must receive explicit commands when introduced; `--all-targets` alone does not certify every feature configuration or doctest. Use the default supported build path first; avoid an untested feature maze.

Run targeted tests during work, package integration tests at acceptance, and full checks at meaningful milestone/final integration on a quiescent tree. Root records both pre-commit tree identity and resulting commit; checkpoint-only documentation edits need no full Rust rerun. If concurrent edits changed tested code, rerun the affected checks against the accepted tree. Preserve exit codes and meaningful output, not just “passed.”

At P00 create `reports/coverage.md`: gate, work package, requirement/command, test names, evidence, status. Keep it updated at integration. `capabilities` lists only implemented commands; the coverage report must still show all target commands. Missing target commands cannot disappear from the objective by omission from runtime capabilities.

Before final acceptance, inspect every planned command against the CLI contract, including error paths. Search for reachable `todo!`, `unimplemented!`, fixture-specific returns, unconditional success, disabled required tests, and skipped suites. A static search is an aid; reviewers determine whether each finding is acceptable or a real missing implementation. Ignore only documented optional/live checks, never a core requirement.

## Local completion versus release qualification

The local implementation is complete only when:

1. All P00–P21 implementation packages are accepted; V01–V17 have current evidence and no unresolved core finding.
2. One disposable end-to-end CLI run demonstrates offline capture/search/graph/rebuild, the complete host packet workflow, remote embedding/generation via mock HTTP, and research interruption/resume/budget stop.
3. Required behavior is exercised through production adapters and state machines; fixture data replaces external services, not application logic.
4. User documentation, executable skill examples, configuration examples, and capability/schema output match the built binary.
5. `reports/FINAL.md` identifies implemented commands, test environment/results, commits or preserved dirty files, known limitations, and external qualification entries that remain unverified.

When an unavailable native target, real host app, real provider, or license decision remains open, say **local implementation complete; external qualification pending**. Do not claim those design release gates passed. If any core gate fails, report partial/blocked work instead. This distinction reconciles autonomous development with truthful validation; it is not permission to defer implementation.

Publishable release readiness additionally needs the relevant E01–E05 checks and owner decisions. This run prepares CI and opt-in procedures but does not push, create releases, install into real hosts, buy provider work, or consume private documents.
