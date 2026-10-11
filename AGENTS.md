# Repository agent guidance

This repository implements a Rust CLI, provisionally `lwiki`, for a local Markdown wiki. Implementation contracts live in the [developer reference](docs/developer/contracts/README.md). Follow the [agent workflow](docs/developer/agent-workflow/README.md) for implementation tasks.

The original M0–M4 run assigned orchestration and implementation to `gpt-6-sol`. For subsequent work, prefer `gpt-6.1-sol` for bounded implementation, research and test work; use `gpt-6-astra` for architectural challenges, difficult storage/accounting invariants and independent review. The user explicitly authorizes this delegation. Follow current user model preferences and available session capabilities. Model settings belong to the session/harness; a Markdown instruction cannot change the running root model.

On implementation startup/resume, read the execution playbook and the local `docs/execution/STATE.md` checkpoint if present, then only the selected work package and its linked contracts. A fresh checkout has no session checkpoint: inspect Git and the [current contract map](docs/developer/current-contracts.md), then create local state for the authorized task. Do not load all research or previous conversations. For a small unrelated edit, read only relevant guidance.

The orchestrator owns shared interfaces, Cargo files, CLI registry, schemas, commits, and execution state. Workers edit only leased paths and their assigned report. No nested delegation unless the orchestrator grants it. Local tests use disposable fixture vaults and mock providers, with no production access.

Keep canonical knowledge in Markdown, indexes rebuildable, source revisions immutable, headings independent of identity, and model-derived assertions traceable to evidence. Semantic embeddings are remote API calls; local models and required external database services are out of scope. Preserve the technical designs' freshness, recovery, offline/dry-run, and budget contracts.

During an authorized implementation goal, continue through M0–M4 without routine confirmation. Use the reversible defaults and decision log in [DECISIONS.md](docs/developer/agent-workflow/DECISIONS.md); defer external credentials, live-host qualification, unavailable-platform checks, naming, licensing, and publishing decisions. Deferral does not waive core implementation or local correctness tests. Preserve user edits and obey platform permissions. Stop only when no safe in-scope work remains, or an explicit user/system limit requires it.

Update the execution state after integration, before compaction, and before stopping. Report actual checks and their limits. Never claim a mock demonstrates live-provider compatibility or a cross-build demonstrates native crash safety.

Commit coherent, tested milestones as work progresses. Record the commit, validation evidence, unresolved findings and next step in the local checkpoint; promote stable conclusions into tracked summaries. A checkpoint commit preserves progress and does not imply that an unfinished quality goal has passed acceptance.

## Writing guidelines

Write for people using the tool. User documentation must be understandable to a
reader who has not read the code, design documents or research reports. These
rules also apply to help text, setup prompts, errors and completion messages.

- Start with what the reader wants to do and the result they will get. Give the
  steps in order, explain what they should see, and give a useful next action.
- The quick start creates a real, lasting wiki, connects the reader's chosen
  agent, and helps them use their own notes. Keep disposable fixtures and
  demo-only setup in testing instructions.
- Use clear, concise, professional English. Prefer familiar words, short
  sentences and direct verbs. Explain a necessary technical term the first time
  it appears. Do not assume all readers are developers or native English speakers.
- Use the same everyday terms throughout the docs. Introduce a wiki as a folder
  of notes. Explain capturing as saving a copy that the agent can cite. Use
  internal names such as Source, Revision, canonical identity or projection only
  in a reference that needs their exact meaning.
- Keep storage internals, evaluation protocols, implementation history, review
  scores and release checklists in developer documentation. Do not make readers
  understand those details before they can use the tool.
- State a limitation where it changes the reader's decision or next step. Say
  what is unavailable, what happens and what they can do. Avoid abstract phrases
  such as "availability boundary", "qualification gate" or "evidence contract"
  in user instructions. Do not add defensive caveats to every paragraph.
- Avoid marketing language, em dashes, emojis, stock AI phrases and formulaic
  prose. Keep command names, code, quoted evidence and exact error text intact.
- Edit for natural flow, not just banned words. Replacing jargon with a synonym
  does not fix a paragraph organized around implementation concerns.

Before accepting user documentation, an independent reviewer must follow the
main task without developer docs and answer: Can I tell what this does for me?
Do I know where to start? Can I follow each step and recognize success? Can I
use my own notes with my agent afterward? Does this sound like instructions
from a helpful colleague? Missing steps or unclear language require revision.
A link checker or passing command does not establish that the writing is useful.

Keep working plans, agent notes, reviews, session checkpoints and logs local under gitignored `.work/<task>/`. Preserve existing evidence under ignored execution paths or `.artifacts/`; do not relocate it solely for consistency. Keep user documentation, reusable developer instructions, implementation contracts/decisions and curated validation summaries tracked. Promote stable findings into those maintained documents; never commit working plans, raw logs, per-attempt reports or temporary state. An ignore rule does not untrack existing files: extract enduring guidance before removing old worklogs from the tracked tree, preserve local evidence and Git history, and ensure tracked documentation works in a fresh checkout without ignored files.

## Delegate for independent judgment

Use subagents when a task benefits from independent research, a bounded implementation, adversarial testing or a fresh architectural perspective. Give each agent a concrete question, relevant files, exclusive write paths, constraints and a required evidence artifact. Prefer fresh context for critics and broad design reviews; implementation history can anchor their judgment. Respect available slots, serialize shared builds and keep integration with the orchestrator. Do not create parallel workers for tightly dependent edits or let a worker's self-review substitute for independent acceptance.

Batch related changes into a coherent end-user workflow before running integration checks. Account for build and test duration when choosing work boundaries: use cheap local checks while editing, then run focused tests on the integrated slice and broad suites at consequential milestones. Repeat a passed check only when changed code, a failure or an unresolved concern justifies it. Use long-running measurements for independent work, preserving their pinned executables, inputs and protocol; do not add competing builds or experiments that distort the measurement.

Before an expensive Rust build or test, name the user-visible batch now ready for
validation, the changed inputs and the checks that resolve an actual uncertainty.
If implementation for that batch remains, continue it before validating. If only
minor polish has accumulated, inspect the current priorities and combine it with
the next high-impact feature slice. Do not make arbitrary edits to justify a run.
After a passing checkpoint, advance the workflow rather than repeating unchanged
checks. A failed check, changed build environment, targeted correctness concern or
final acceptance gate is a valid explicit reason to run again. The optional
repository checkpoint hook reinforces this policy; it does not replace required
correctness or final acceptance checks.

For consequential quality work, establish a critic before implementing. Agree on representative user tasks, observable success criteria, correctness blockers and an acceptance threshold before seeing candidate results. Have the critic assess actual commands and returned content, document missing facts or failed tasks, and replay fixes. Keep task completion, citation correctness, retrieval relevance and answer completeness separate. If the user requires a high critic score, keep the goal active until the declared gate passes; do not lower the gate or inflate claims to finish.

Before consequential architecture decisions, ask an independent Astra reviewer to challenge the measured problem, expected user benefit, simpler alternatives, scaling costs and migration path. Revisit the decision when evidence or constraints change. Require a concrete experiment for uncertain tradeoffs and prioritize an integrated user workflow over additional disconnected components. Passing component tests and scoped critic scores do not establish that the architecture is useful or that integration is complete. Routine local edits do not need another architecture review.

Before adding a general repair subsystem, trace the failure through the actual public inputs and allocation rules. Ask whether a stronger input or identity invariant prevents the failure while preserving the intended workflow. Keep adversarial internal-plan tests, and verify that ordinary commands handle the case automatically; a safe refusal alone does not establish good usability.

## Research failures and reassess the architecture

When an unexpected failure appears, identify which pipeline stage loses the required information. Research how comparable tools address that failure using primary documentation, source code and original papers. Record the mechanism, fit, tradeoffs and testable hypothesis in the relevant report or decision log. A vendor's benchmark is evidence for investigating an approach, not proof it will work here or permission to add a conflicting dependency.

After two iterations without a clear improvement in whole-task success, or when fixes repeatedly regress other cases, stop local parameter tuning and request a fresh architecture review. Examine the complete data flow, information discarded between stages, budgets, defaults and the user workflow. Compare a coherent redesign with the current approach. Run controlled experiments that distinguish likely causes before adding more heuristics. Continue independent useful work while reviewing; this is not a reason to stop the authorized goal or request routine permission.

## Preserve trustworthy evaluation

Use the [critic workflow](docs/developer/testing-usability.md) and [context evaluation protocol](docs/developer/evaluating-context.md). Prefer suitable public datasets with provenance, licenses and immutable input hashes, supplemented by independently authored realistic and synthetic cases. Include paraphrases, distant and multiple-source facts, distractors, absent information and operational failures. Keep labels out of indexed content.

Separate development from unseen acceptance questions. Freeze the binary, protocol and limits before exposing a holdout; any question used to guide a fix becomes development data. Compare baseline and candidate on the same source revisions, caches and budgets. Acquire all authorized query embeddings before paired offline evaluation, since provider history can change the index. Record every error, regression and exclusion; do not cherry-pick modes, budgets or questions. Inspect the evidence actually returned, not just source-hit rates or passing unit tests.

When a solution adds an agent/model stage or changes the user workflow, declare a separate evaluation protocol before opening the holdout. Count its additional input, calls, latency and known cost; mark unavailable usage as unavailable. A selector must see only its task and candidate evidence, while the independent critic retains the expected answers. Do not present a model-assisted workflow's score as proof that the original deterministic command passed.

Measure elapsed time with a monotonic clock inside the process that owns the interval. Verify clock comparability before subtracting timestamps from separate processes; use recorded UTC observation times for external agent intervals when a shared monotonic epoch is unavailable, disclose wall-clock limitations, and distinguish observed turnaround from model inference time.

Before using timings to guide performance changes or claim capacity, verify the actual compiler optimization settings and intended user build profile. Use the release configuration for release-performance qualification; pin the executable, build mode and compiler settings together. Preserve unoptimized runs as explicitly labeled diagnostics, and do not infer shipping limits from them. Correctness tests and representative performance measurements serve different purposes.

Local mocks test mechanics. Live calls require existing user authorization, explicit finite limits and protected credentials; never print secrets or discard unknown accounting holds to make an experiment fit. Report experimental changes and residual limitations plainly. Update contracts, user documentation and execution state only to match the behavior actually implemented and verified.
