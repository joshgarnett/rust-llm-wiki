# Astra M3 telemetry review

2026-09-28; independent, read-only source review of dirty M3 integration over `cbaf990`. Only this report was written; source/Cargo leases remain with root. Report lease returned.

Scope: `WikiError` serde boundary, dispatcher activity/retry/recovery, native transport entry classification, native runtime lifetime, CLI success/error propagation, `remote_cli` cases; narrowly followed embedding fallback and API recovery callers. No historical P17–P19 invariant re-review. Reviewer executed no tests/Cargo; inspected root's test logs. No live-provider, TLS interoperability, crash, or platform qualification claimed.

## R1 — successful fallback loses prior network activity: closed

`src/cli/dispatch.rs:1132,1165` annotates only errors. `src/app/embeddings.rs:1618–1632` (likewise semantic graph/context) catches `ProfileUntrusted` and returns a lexical result with `network_used=false`.

Concrete path: uncached semantic query sends; provider responds 503 (or command-auth 401); private provider config bytes change before retry; `Dispatcher::execute_inner` rechecks the service at line 340 and returns `ProfileUntrusted`; explicit lexical fallback succeeds. The dispatcher correctly retains true, but the success result and CLI envelope report false.

Closure: private `EmbeddingNetworkResult` now ORs dispatcher activity into successful HitSet (including nested graph), GraphResult and ContextResult. Error propagation remains intact. Embedding report/API success also use actual invocation activity. Native regression receives a request, changes config bytes, returns 503, then verifies successful lexical fallback with data/meta telemetry true. `/private/tmp/lwiki-m3-native-fallback.log`: 1/1 PASS, 2.99s. Paid malformed API/offline refusal and successful reuse cases: 2/2 PASS, 6.71s, `lwiki-m3-native-api-telemetry-second.log`. These are root-executed logs, independently inspected.

Six reviewed files (domain types, dispatcher, native transport, CLI dispatch, remote runtime, remote CLI tests) match both `/private/tmp/lwiki-m3-regression-before-hashes.json` and the current integration snapshot. This establishes current-source identity, not retrospective hashes for earlier test runs.

## Other observations and acceptance checks

`#[serde(skip)] pub(crate) network_used` prevents serialized error content from granting telemetry authority. Error `details` are not consulted for network activity. Native transport sets `not_entered` from its actual first-poll boundary; dispatcher preserves false for proven pre-entry refusal, true for entered failures, and monotone true across retries. Retained decoding does not set activity. Native CLI runtimes construct fresh dispatchers; no process-global leakage found.

Pre-entry refusal, forged serialized telemetry, and successive in-process invocation isolation remain source-reviewed here, not separately executed reviewer tests. `execute_public` bypasses this counter; wire it before P20 exposes public-fetch CLI telemetry (known future integration dependency).

Disposition: R1 closed by source and targeted native regression evidence; no new bounded-scope blocker found. Report lease returned.
