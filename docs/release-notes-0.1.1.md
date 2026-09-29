# lwiki 0.1.1

Research now collaborates with your agent. `research run` returns a saved task packet; your agent chooses its authorized tools and submits collected text or cited answers with `research import`. lwiki validates source bytes, citations, scope and local limits, then saves the next handoff or report. Research makes no provider, search or fetch requests.

## Changes since 0.1.0

- Replaced the built-in research engine with `plan`, `run`, `resume`, `import`, `status` and `report` handoffs. Imports are atomic and repeatable; stale packets require explicit refresh. Reports retain citations and unassessed claims. Host tool usage is unobserved.
- Removed the old research execution/paid-resume code, Brave search/public-fetch adapters and old research schemas. Existing research runs are not migrated. Remove search-service/profile entries from provider configuration.
- Fixed graph resolution and review using evidence extracted after a source refresh. Historical or damaged evidence remains ineligible for current acceptance.
- Excluded extraction packets, extraction summaries and API generation output records from ordinary current retrieval and the embedding corpus. Withdrawn source content remains available only for explicit historical auditing.
- Added recommended/default `responses-v1` generation while retaining explicit Chat Completions compatibility. Requests use `store: false`; reasoning output items are handled without turning them into answer text.
- Fixed gateway compatibility for valid response-header token characters and additive usage fields. Increased the bounded generation-probe allowance and retained safe failure reasons.
- Made shared provider receipt settlement idempotent and retained decoding contracts for embeddings, API extraction and probes. Reopening a paid response must not silently resend it.
- Added command/argument help and clearer source-withdrawal guidance for unsupported source-add rollback.

## Download and test

Archives and SHA-256 files cover native Linux GNU, macOS and Windows, each on x64 and ARM64. Every archive contains the executable, version/capability output and `build-info.json`; check its exact source commit, target, version and clean-source flag.

Use the complete [0.1.1 test-agent handoff](https://github.com/joshgarnett/rust-llm-wiki/blob/v0.1.1/docs/testing-0.1.1.md) for downloads, the offline regression script, agent research submissions, live gateway checks and reporting requirements. The [offline smoke script](https://github.com/joshgarnett/rust-llm-wiki/blob/v0.1.1/scripts/manual_smoke.py) creates a disposable vault and retains every command envelope.

Windows vault writes remain unsupported. Builds and mocks do not establish live gateway quality or native storage qualification on every platform. The old installed-copy macOS SIGKILL has not been diagnosed. The predicate registry still has no dedicated ownership/responsibility relation.

## Verified build

Source: `258689d90d301f3b1ed4f7e12cf8b4f71a296141`. [Six native release builds](https://github.com/joshgarnett/rust-llm-wiki/actions/runs/36631455854) and [all three CI jobs](https://github.com/joshgarnett/rust-llm-wiki/actions/runs/36631438819) passed. All six archives and their checksum files were verified against this clean source, version and native architecture. The downloaded Apple Silicon binary passed strict signature verification and the 30-command offline regression script.

Local acceptance: 557 Rust cases across the complete suite and affected continuations, 41 Python build-tool tests, and the 36-step skill recipe. This release remains a draft for owner inspection; authenticated repository access is required to download draft assets.
