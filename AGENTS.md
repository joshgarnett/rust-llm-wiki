# Repository agent guidance

This repository implements a Rust CLI, provisionally `lwiki`, for a local Markdown wiki. Implementation contracts live in [docs/technical](docs/technical/README.md). The autonomous implementation run is defined by [docs/execution](docs/execution/README.md).

The original M0–M4 run assigned orchestration and implementation to `gpt-6-sol`. For subsequent work, prefer `gpt-6.1-sol` for bounded implementation, research and test work; use `gpt-6-astra` for architectural challenges, difficult storage/accounting invariants and independent review. The user explicitly authorizes this delegation. Follow current user model preferences and available session capabilities. Model settings belong to the session/harness; a Markdown instruction cannot change the running root model.

On implementation startup/resume, read the execution playbook and the local `docs/execution/STATE.md` checkpoint if present, then only the selected work package and its linked contracts. A fresh checkout has no session checkpoint: inspect Git and the [current contract map](docs/current-contracts.md), then create local state for the authorized task. Do not load all research or previous conversations. For a small unrelated edit, read only relevant guidance.

The orchestrator owns shared interfaces, Cargo files, CLI registry, schemas, commits, and execution state. Workers edit only leased paths and their assigned report. No nested delegation unless the orchestrator grants it. Local tests use disposable fixture vaults and mock providers, with no production access.

Keep canonical knowledge in Markdown, indexes rebuildable, source revisions immutable, headings independent of identity, and model-derived assertions traceable to evidence. Semantic embeddings are remote API calls; local models and required external database services are out of scope. Preserve the technical designs' freshness, recovery, offline/dry-run, and budget contracts.

During an authorized implementation goal, continue through M0–M4 without routine confirmation. Use the reversible defaults and decision log in [DECISIONS.md](docs/execution/DECISIONS.md); defer external credentials, live-host qualification, unavailable-platform checks, naming, licensing, and publishing decisions. Deferral does not waive core implementation or local correctness tests. Preserve user edits and obey platform permissions. Stop only when no safe in-scope work remains, or an explicit user/system limit requires it.

Update the execution state after integration, before compaction, and before stopping. Report actual checks and their limits. Never claim a mock demonstrates live-provider compatibility or a cross-build demonstrates native crash safety.

Keep session checkpoints and detailed agent/run reports local under the ignored execution paths or `.artifacts/`. Keep reusable instructions, implementation decisions and curated validation summaries tracked. Promote stable findings into those maintained documents; do not add raw logs, per-attempt reports or temporary state to Git. Preserve local evidence when changing tracking, and ensure tracked documentation works in a fresh checkout without ignored files.

## Delegate for independent judgment

Use subagents when a task benefits from independent research, a bounded implementation, adversarial testing or a fresh architectural perspective. Give each agent a concrete question, relevant files, exclusive write paths, constraints and a required evidence artifact. Prefer fresh context for critics and broad design reviews; implementation history can anchor their judgment. Respect available slots, serialize shared builds and keep integration with the orchestrator. Do not create parallel workers for tightly dependent edits or let a worker's self-review substitute for independent acceptance.

For consequential quality work, establish a critic before implementing. Agree on representative user tasks, observable success criteria, correctness blockers and an acceptance threshold before seeing candidate results. Have the critic assess actual commands and returned content, document missing facts or failed tasks, and replay fixes. Keep task completion, citation correctness, retrieval relevance and answer completeness separate. If the user requires a high critic score, keep the goal active until the declared gate passes; do not lower the gate or inflate claims to finish.

## Research failures and reassess the architecture

When an unexpected failure appears, identify which pipeline stage loses the required information. Research how comparable tools address that failure using primary documentation, source code and original papers. Record the mechanism, fit, tradeoffs and testable hypothesis in the relevant report or decision log. A vendor's benchmark is evidence for investigating an approach, not proof it will work here or permission to add a conflicting dependency.

After two iterations without a clear improvement in whole-task success, or when fixes repeatedly regress other cases, stop local parameter tuning and request a fresh architecture review. Examine the complete data flow, information discarded between stages, budgets, defaults and the user workflow. Compare a coherent redesign with the current approach. Run controlled experiments that distinguish likely causes before adding more heuristics. Continue independent useful work while reviewing; this is not a reason to stop the authorized goal or request routine permission.

## Preserve trustworthy evaluation

Use the [critic workflow](docs/testing-usability.md) and [context evaluation protocol](docs/evaluating-context.md). Prefer suitable public datasets with provenance, licenses and immutable input hashes, supplemented by independently authored realistic and synthetic cases. Include paraphrases, distant and multiple-source facts, distractors, absent information and operational failures. Keep labels out of indexed content.

Separate development from unseen acceptance questions. Freeze the binary, protocol and limits before exposing a holdout; any question used to guide a fix becomes development data. Compare baseline and candidate on the same source revisions, caches and budgets. Acquire all authorized query embeddings before paired offline evaluation, since provider history can change the index. Record every error, regression and exclusion; do not cherry-pick modes, budgets or questions. Inspect the evidence actually returned, not just source-hit rates or passing unit tests.

When a solution adds an agent/model stage or changes the user workflow, declare a separate evaluation protocol before opening the holdout. Count its additional input, calls, latency and known cost; mark unavailable usage as unavailable. A selector must see only its task and candidate evidence, while the independent critic retains the expected answers. Do not present a model-assisted workflow's score as proof that the original deterministic command passed.

Local mocks test mechanics. Live calls require existing user authorization, explicit finite limits and protected credentials; never print secrets or discard unknown accounting holds to make an experiment fit. Report experimental changes and residual limitations plainly. Update contracts, user documentation and execution state only to match the behavior actually implemented and verified.
