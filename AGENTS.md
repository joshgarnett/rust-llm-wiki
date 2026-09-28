# Repository agent guidance

This repository implements a Rust CLI, provisionally `lwiki`, for a local Markdown wiki. Implementation contracts live in [docs/technical](docs/technical/README.md). The autonomous implementation run is defined by [docs/execution](docs/execution/README.md).

For that run, the orchestrator is `gpt-6-sol`. Delegate bounded implementation to `gpt-6-sol`; use `gpt-6-astra` for difficult storage/accounting invariants and independent review. The user explicitly authorizes this delegation. Model settings belong to the session/harness; a Markdown instruction cannot change the running root model.

On implementation startup/resume, read the execution playbook and [STATE.md](docs/execution/STATE.md), then only the selected work package and its linked contracts. Do not load all research or previous conversations. For a small unrelated edit, read only relevant guidance.

The orchestrator owns shared interfaces, Cargo files, CLI registry, schemas, commits, and execution state. Workers edit only leased paths and their assigned report. No nested delegation unless the orchestrator grants it. Local tests use disposable fixture vaults and mock providers, with no production access.

Keep canonical knowledge in Markdown, indexes rebuildable, source revisions immutable, headings independent of identity, and model-derived assertions traceable to evidence. Semantic embeddings are remote API calls; local models and required external database services are out of scope. Preserve the technical designs' freshness, recovery, offline/dry-run, and budget contracts.

During an authorized implementation goal, continue through M0–M4 without routine confirmation. Use the reversible defaults and decision log in [DECISIONS.md](docs/execution/DECISIONS.md); defer external credentials, live-host qualification, unavailable-platform checks, naming, licensing, and publishing decisions. Deferral does not waive core implementation or local correctness tests. Preserve user edits and obey platform permissions. Stop only when no safe in-scope work remains, or an explicit user/system limit requires it.

Update the execution state after integration, before compaction, and before stopping. Report actual checks and their limits. Never claim a mock demonstrates live-provider compatibility or a cross-build demonstrates native crash safety.
