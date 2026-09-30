# CLEANUP C18–C20 extraction/provider diagnostics

Current root acceptance: required local cleanup is complete at source `9e20065db7e4bf8f4830dc4d0ed45fd01ded5884`, frozen 343-file SHA256 `78cc134d94ad7f1e271312f2414ba8a6ba9280fb7e13172ac11f9e25c3f2f97c`. The native aggregate passes49 targets / 642 Rust parents / 7 ignored helpers; final format/strict Clippy, optimized release skill4 tests / 42 steps, copied-binary offline checks and linked-wiki backup/restore/undo pass. Routine41 Python tooling results are explicitly reused on unchanged tooling. Astra source findings are closed; actual logs, failures, input-reuse scope and artifact fingerprints are in [CLEANUP-checks.json](CLEANUP-checks.json). Dated pending/failed notes below are historical and are superseded by this verdict. External provider/billing/host/platform/quality/publication limits remain in [the register](../CLEANUP.md).

Status: implementation submitted for root integration; targeted checks pending the shared-interface build lease. No live calls, credentials, real vaults, or host installation.

## Implemented

- `src/graph/packet.rs` now instructs unique per-window quotations and absolute, zero-based, half-open UTF-8 byte spans. `src/graph/wire.rs` reports a fixed absent/ambiguous/invalid-span reason, item kind/local ID/index, window ID, exact match count, and at most three candidate absolute spans. It still rejects the whole output, with no automatic salvage or repair spend.
- `src/jobs/diagnostics.rs` and narrow `src/vault/operational.rs` additions retain a bounded per-attempt private sidecar, bound to the ledger attempt and body hash. Semantic rejected output is capped at 256 KiB; opted-in HTTP error bodies at 16 KiB. Metadata records observed/retained lengths and truncation. Existing private 0700/0600 and Windows protected-file primitives are reused. Normal inspection returns only metadata; explicit raw inspection returns UTF-8 text or hex. Prune requires a settled receipt and known billing under the run lock, and refuses unknown reservations. Sidecars are outside canonical knowledge and do not affect receipt authority.
- `src/graph/api_extract.rs` saves a semantic rejection sidecar before receipt publication, retaining its fixed reason and paid response for host-assisted correction through existing `graph import --file`. `src/providers/dispatcher.rs` adds an explicit HTTP diagnostic opt-in builder; ordinary errors remain redacted. Its C19 fixed `uncertain_retry_requires_opt_in` reason names same-run `--retry-uncertain` and possible prior billing.
- `src/app/probe.rs` adds an explicit paid extraction-schema probe using the actual `schemas/extraction-v1.json` output contract, the extraction instruction family, and a synthetic window. It reports `output_contract`. Success proves that one probe request/schema was accepted; it does not establish semantic grounding or broader live compatibility.
- `checkpoint_generation` returns immediately for an empty checkpoint draft, per S03 integration request.

## Root-owned integration seams

Root exported `jobs::diagnostics`. Root is wiring `jobs diagnostics inspect|prune --run --attempt --kind [--raw]`, `--retain-http-error-body` into `Dispatcher::with_http_error_diagnostics(bool)`, and `doctor --probe --role generation --extraction-schema` into `OfflineApp::probe_extraction_schema(&RemoteRuntime)`. `JobLedger` methods are `retain_diagnostic`, `inspect_diagnostic`, and `prune_diagnostic`; `DiagnosticKind` serializes `semantic_rejection`/`http_error`. Human rendering should surface the nested API `recovery_action` and quotation details.

## Regressions and checks

- Added `tests/api_extraction.rs` cases for ambiguous/absent/wrong-span paid rejection, private raw inspection and host import without another model call, and the actual extraction-schema probe fixture. Added a Unicode second-window absolute-span unit test in `wire.rs`.
- Added `tests/provider_dispatch.rs` HTTP 400 mock with and without diagnostic opt-in; normal errors must not echo the private body. The sidecar is protected under unknown billing.
- Ran `rustfmt --edition 2024` on leased Rust files. No compile/build/test command was run while the root held the build lease. Root's `ApiExtractionRequest.requested_limits` addition requires fixture literal updates before the targeted build. The new schema-probe test constructs a `RemoteRuntime` from fixture fields and should be checked for Rust partial-move ordering at compile.

## Limits and follow-up

The sidecar uses fixed per-attempt caps, not a global storage quota. Unknown-charge holds protect diagnostics from pruning until reconciliation; S04 should inventory/protect these bytes and expose reclaimable/protected amounts. F08 partial salvage remains deferred: explicit raw inspection plus strict host import provides repair without weakening atomic grounding or spending for another model call. Mock tests cannot diagnose the owner's Haiku HTTP 400; an opted-in real response inspection is separate external qualification.
