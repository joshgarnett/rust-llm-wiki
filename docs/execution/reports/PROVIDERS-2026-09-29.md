# Gateway compatibility and Responses follow-up

Status: locally validated; commit/push and six-platform build follow. Baseline main `2953cd5`. User-provided LiteLLM response samples are reproduced with disposable vaults and mock providers. No corporate gateway, auth helper, provider credential or paid API was used. The existing draft v0.1.0 assets are unchanged.

## Changes

- `responses-v1` is the default for an omitted adapter and the recommended new generation service. Explicit Chat Completions remains supported. Responses uses bounded instructions/user input, store:false, stream:false, no tools and the requested output ceiling. Completed assistant text is validated as a whole; incomplete/refused/tool output remains a paid failure.
- Responses schema mode retains a bounded provider grammar separately from the original local schema. Closed alternatives preserve optional-field omission; local evidence, date, span and conditional checks remain authoritative. Historical codecs preserve the selected surface and both schemas without current credentials/configuration. Existing Chat codec fields/serialization remain compatible.
- HTTP response names accept RFC token punctuation, including gateway underscores. Existing byte/count/control limits remain. Fixed allowlisted diagnostic reasons are recorded before spooling and copied into error details without provider-controlled text. The accounting safety sentinel remains unchanged.
- Embedding and generation usage accept nullable details and bounded extensions. Recognized count/type/sum/subset errors still fail. Unknown extensions or additional cache-write classes never settle as known cost; missing cached/reasoning partitions never become zero. No SDK or dependency changes.
- Generation probes request up to 256 output tokens within service/caller ceilings. Unknown gateway hard token/cost limits remain unenforceable rather than acquiring a false bound. Text-json accepts exactly one outer JSON fence and still rejects prose/fragments.

## Review and evidence

Sol implemented wire/usage fixtures and native probe regressions. Astra reviewed the design, implemented the bounded schema projection, and a separate Astra agent independently reviewed the integration. Independent findings R1 (embedding output subsets without a parent total) and R2 (allocation before schema expansion bounds) were fixed and re-reviewed. See [independent review](PROVIDERS-independent-review.md), [design/projection report](PROVIDERS-review.md), [wire report](PROVIDERS-generation.md), and [usage/probe report](PROVIDERS-usage.md).

- First selected provider unit gate passed in 49.155s: 79 passed, 2 ignored subprocess helpers, 74 filtered. Log `.artifacts/providers-first.log`. This precedes the two final review corrections.
- Full unit/remote_cli/provider_trust gate passed in 809.374s, logged in `.artifacts/providers-integrated.log`: 155 unit cases passed, three explicitly invoked helper tests ignored by the parent harness; native CLI and trust targets also passed. It includes actual loopback HTTP, Responses extraction with projected schema and offline paid-output reuse, probes, metadata reasons, legacy codecs, and ledger crash boundaries.
- The final six-target manual-bug gate passed in 137.839s before this provider expansion; see [manual report](MANUAL-2026-09-29.md).

The new [provider guide](../../providers.md) supplies configuration and bounded manual retests. Mock coverage establishes parsing, accounting and recovery behavior, not live gateway compatibility, semantic quality or billing accuracy. Real Linux/macOS/Windows build results will be recorded against the pushed source; Windows vault writes remain unsupported.

Final broad affected integration/format/strict-Clippy gate is running in `.artifacts/followup-broad.log`. The only product difference after the full-unit gate compiled was rustfmt ordering two private module declarations; no semantic changes.


Final affected gate: 32 of33 targets passed in507.330s; Clippy found one test-only `get(...).is_none()` assertion. Root changed it to `!contains_key(...)`; no product changes. The format/strict-Clippy/retrieval_lexical continuation passed all3 targets in5.930s. Native CLI passed10 cases and provider trust7. The broad gate covered provider dispatch/wire, semantic retrieval, research planning/stages/extraction/reports/workflow, catalog recovery, context, graph review, inverses, and local M1/M2 workflows. Per-target case totals and evidence phases are in PROVIDERS-checks.json. This is affected-scope validation, not a new full historical M0–M4 qualification. No new unresolved source finding remains.
