# Astra P19 bounded acquisition review

2026-09-28. Independent review of the P19 contracts/plan/report; public fetch, Brave search, normalization and acquisition leaves; and the narrow shared metadata/dispatcher/native-search wiring. No source changes, Cargo/test execution, Git operations, nested delegation or live requests. Lease: this report only. P20 research scheduling and unrelated P17/P18 implementation were not reviewed.

The initially supplied acceptance snapshot was `/var/folders/g0/jdy1y8615k30fs6sbf4x1wt40000gn/T/lwiki-p19-acceptance-k0qmzz3v`. The worker was actively testing and fixing leased paths during review. Findings below identify both the initial defects and inspected source fixes; test qualification is recorded separately rather than inferred from a passing older snapshot.

## Findings and resolution

### R1 — Redirect settlement did not complete its dependency

Initial `research/acquire.rs::fetch_url` settled the dispatcher's default Unknown receipt for a complete redirect, then made the destination task depend on that redirect task. `materialize(source=None)` did not change that disposition. Ledger automatic task repair completes only Validated receipts, so the redirect remained Running and the destination could not be admitted. This was a real two-hop failure, not a hypothetical retry issue.

**Source-closed:** `acquire.rs:329`, `settle_validated_redirect`, now authenticates the retained capture, requires a valid redirect, builds the actual Validated receipt with empty canonical/cache outputs, and acknowledges/settles it. The existing canonical-output task repair completes that validated hop; the destination remains a separate task/reservation. Unknown and failed responses continue through ordinary conservative receipt settlement. The worker added `two_hop_capture_completes_redirect_and_retains_bound_origin_chain`, requiring two admitted requests, both tasks Completed/attempts Settled, and the final immutable source's actual origin/query/redirect hashes.

### R2 — HTML entity lookahead was quadratic

Initial `web_normalize.rs::append_entities` ran `find(';')` over the entire remaining string for each ampersand and only afterward rejected distances over 16. Ampersand-heavy HTML without semicolons therefore caused quadratic scanning at the permitted multi-megabyte input size, outside the network timer.

**Source-closed:** `web_normalize.rs:268` now limits the byte iterator itself to 17 bytes before looking for `;`. The eventual string slice ends at the located ASCII separator, preserving UTF-8 boundaries. The worker added a 128 KiB ampersand-heavy/Unicode fixture. This is a bound on parser work, not a claim about interruptible blocking syscalls or a benchmark-derived deadline.

### R3 — Post-response planning failure discarded known recovery identity

Initial `public_fetch.rs` returned `Rejected` without the known `SpoolRef` if `materialization_plan` failed after `record_response` had succeeded. A canonical-run conflict or local read/lock failure does not establish provider-output rejection, and the caller needs the paid response identity for recovery.

**Source-closed:** `public_fetch.rs:1070` now returns OutcomeUnknown with the original attempt and `Some(spool.clone())`; it does not fabricate a rejected receipt. `local_materialization_failure_retains_paid_raw_spool_for_recovery` changes the disposable canonical run note at AfterReceived, asserts the failed planning result retains the paid raw spool, restores the note, then recovers/captures with DNS/connector/request counts still one.

### R4 — High-level dry run wrote an acquisition descriptor

Initial `fetch_url` called `plan_task`, which creates `.wiki/state/acquisition-inputs` and a retained descriptor, before `ledger.add_tasks` rejected dry-run mutation. The original offline/dry-run test called `execute_public` with a pre-created task and did not cover this application entry point.

**Source-closed:** `acquire.rs:425` now reads the ledger's execution policy and returns before planning/writes for offline/dry-run, checks cancellation, and binds the supplied filesystem to the ledger's vault. The added `high_level_dry_run_and_offline_leave_full_vault_tree_unchanged` compares directory membership and every file hash before/after a fresh URL request, plus zero DNS/connector calls. Offline recovery of already retained data remains available through the separate retained-response APIs; this fresh-fetch entry gate creates no remote permission.

## Boundaries inspected without another source blocker

- **URL/destination:** `public_fetch.rs:71` rejects userinfo/password, controls, unsupported schemes and nonpublic IP literals; redirect validation rejects HTTPS downgrade. `:115` supplies a conservative IPv4/IPv6 public-address predicate, including rejection of mapped IPv6 and tunneling/special ranges. Every DNS answer must pass; an empty, mixed-private or excessive answer set fails before connector entry. The chosen validated addresses become private-construction `PinnedRequest` fields.
- **Native connection:** `public_fetch.rs:215` uses public trust roots/TLS hostname verification with no client auth; no proxy, automatic redirect, retry, automatic decompression or cookie store is configured. It pins the resolver results with `resolve_to_addrs`, forces HTTP/1 and creates only a fixed GET with identity encoding and a fixed user agent. Arbitrary auth/cookie/request headers cannot enter through `PinnedRequest`. Retaining the URL hostname while pinning its addresses preserves TLS hostname verification. Each followed redirect creates another admitted task and repeats destination checks; it cannot inherit an authenticated provider profile.
- **Admission/final poll:** `public_fetch.rs:717` validates the canonical persisted descriptor and task bindings, reserves and journals intent, resolves/validates DNS, rechecks task/source bytes, then consumes `begin_send`. `bounded` at `:627` invokes the opaque authorization's `check_before_entry_after(timer.last)` immediately before first connector-future poll, after pure connector setup. UTC/monotonic/live elapsed checks continue during waits and after completion. Pre-entry failure has a not-sent release path; entered uncertainty retains attempt/observed byte accounting without an implicit retry. The existing final-entry fixture intentionally advances the clock during connector preparation. This provides at-most-once local send-authority consumption; it is not remote exactly-once delivery.
- **Bytes and normalization:** native response chunks are counted before retention; exceeding the compressed ceiling records observed bytes without storing an oversized body. The atomic progress counter survives cancellation of a pending connector future. Raw successful bytes are spooled before any normalization. `web_normalize.rs:110` manually expands gzip/zlib through `take(expanded_limit + 1)`, rejects unsupported encodings/charsets/media and invalid UTF-8, and preserves unsupported originals as immutable captures without fabricated content. HTML processing executes nothing, fetches no subresources and suppresses executable/opaque blocks. Bounds on retained bytes do not prove an absolute process-RSS or HTTP-library internal-buffer ceiling.
- **Protected provenance/recovery:** `PublicCaptureMetadata` is strict, versioned, bounded and has a deliberately limited Debug representation. Its selected headers exclude cookies/auth values; only an authentication-challenge boolean is retained. `ResponseMetadata` validates the acquisition DTO and matching status. Existing restricted metadata/body spool hashes protect it. `retained_capture` at `:477` checks exact retained payload lengths/hashes, canonical task input, URL/limits/input/wire binding, status and original hash; it reconstructs data without DNS, credentials or new send authority. `recover_response` permits only a still-unmaterialized Received response. A missing/corrupt retained input is an error, not permission to resend or synthesize provenance.
- **Real source records:** `acquire.rs:347` compares the outcome against the actual paid raw spool and protected metadata, then validates each redirect observation against its retained attempt. `plan_capture` at `:99` validates the URL/Location chain and redirect cap, uses `SourceStore::plan_capture` to create actual source/revision/original/content operations, and appends canonical acquisition provenance to the new immutable revision note before its hash is used in the real receipt. Source/revision output refs are included in the same validated canonical changeset as the usage receipt. Unsupported/auth/robots/unavailable responses remain explicit gaps; search snippets never become source citations.
- **Brave search/accounting:** `search_wire.rs:33` appends only encoded q/count/offset to the configured trusted endpoint and rejects preexisting shadow parameters. Native authenticated transport recomputes that exact derived URL before entry. `:54` seals count/page/request fingerprints and a SearchResult upper bound. `:190` measures result count separately from lead-field decoding, so malformed titles/URLs remain paid ordinary rejection; only an observed count breach invalidates the declared count guarantee. Unparseable usage/count remains unknown. Search/model operations use configured rate cards and do not inherit public-fetch pricing.
- **Public zero fee:** only the explicitly named `unmetered-public-fetch-v1` contract supplies empty billable classes and zero provider API fee for unauthenticated public GET. Requests, observed response bytes, concurrency and lifetime deadlines remain accounted. This says nothing about ISP/network charges or authenticated search/model fees.

## Integration and evidence limits

`fetch_url` is currently a fresh-work helper, not the P20 resume scheduler. P20 must inspect/replay existing attempts and use retained-response helpers or acknowledged canonical receipt outputs before scheduling another task. Calling the fresh helper on an already completed task is not proof of resumable acquisition. Before P20 acceptance, test restart after raw Received, redirect settlement, canonical source/receipt commit, acknowledgment and settlement; reuse exact retained work and preserve original URLs without a second send. Do not weaken the ledger to make a fresh-dispatch helper act as replay.

This review does not certify live DNS/socket/TLS behavior, Internet/provider interoperability, native behavior on unavailable platforms, hostile concurrent filesystem replacement, power-loss durability, or enterprise egress authorization. Injected resolvers/connectors exercise the production validation/admission path but do not themselves qualify the native HTTP stack against a live server.

Final source hashes observed after the four fixes:

| Path | SHA-256 |
|---|---|
| `src/providers/public_fetch.rs` | `70039efdd580213a0583c3a0816819e86bafbdd39467cf2aa4b27ca3a1e1d0c8` |
| `src/providers/search_wire.rs` | `98f9440100edd77141210dbbd3dd0ef6c3648da83d1d743a023341a584606307` |
| `src/research/acquire.rs` | `bd02f1c9c434c9be88a6a8215765ee6a72966f28017e314b4ca58c7a70cb2c2b` |
| `src/sources/web_normalize.rs` | `c8011636827d70a0f927ab918eb9e22708d3117e2606787a3664e82234887723` |
| `tests/research_acquisition.rs` | `226811801df649cb7bea22ad5eed2d3d482fae020657820c4391081c0c6fe7a8` |

At report creation the worker reported 16 tests passing before the final high-level policy test, with the final 17-test run pending. That report alone is not final gate acceptance; append inspected log/hash evidence below when available. All four findings are source-closed, with no remaining source blocker identified in this bounded review.

### Final focused evidence inspected

Inspected `/private/tmp/lwiki-p19-targeted.log` and `/private/tmp/lwiki-p19-worker-checks.json` after the final run. The recorded command was `CARGO_TARGET_DIR=/Users/jgarnett/Devleopment/rust-llm-wiki/target cargo test --locked --offline --test research_acquisition -- --nocapture` from the isolated snapshot above. Result: **18 passed, 0 failed, 0 ignored, 17.49 seconds**. Independently recomputed log SHA-256 `a1f74ab8d77991e8271ceae46b28078a8de5e71da587d5d747e32d417b8f709f` and the five source/test hashes both in the workspace and snapshot; all match the final table and worker inventory `dc738a30122f10c2c29868cd132d35a830f717848d602a1490112f705e5a6969`.

The final test addition, `protected_metadata_before_received_replays_without_resend`, interrupts AfterSpoolMetadataSync before Received, verifies inspection initially has no recorded spool, replays the protected orphan, then recovers/normalizes/captures with exactly one DNS call, connector call and admitted request. It exercises existing ledger adoption rather than introducing a second recovery mechanism.

R1–R4 have matching focused test evidence as well as source closure. No remaining blocker is identified within this review scope. This is acceptance evidence for the focused P19 acquisition path only: the snapshot excluded P17/P18 consumers, emitted three shared dead-code warnings, and did not run strict whole-integration lint/build or qualify native live network behavior. Root still owns coherent integration acceptance and P20 resume coverage.
