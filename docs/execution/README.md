# Autonomous implementation playbook

Detailed session reports, machine checks and `.artifacts` paths mentioned below are optional local evidence; fresh clones contain the curated summaries and tracked reproduction scripts. Recorded checks describe the identified historical source, not a new verification of the current checkout.

Status: the original M0–M4 implementation playbook is historical. Its reusable collaboration and checkpoint rules apply to new authorized work. Start with [the current contract map](../current-contracts.md) and [cleanup register](CLEANUP.md); [START.md](START.md) preserves the original implementation request. This document does not authorize a new run.

## Objective and completion boundary

Implement the technical design through **M4**: a working Rust CLI/library, recoverable Markdown records and graph, lexical/context retrieval, host-agent extraction and skill export, remote embeddings and generation, bounded resumable research, tests, and user documentation. M5 experiments are excluded. Follow [WORK-PACKAGES.md](WORK-PACKAGES.md) in dependency order; a functioning M1 prototype is an intermediate result.

Completion means every required package and local gate in [VALIDATION.md](VALIDATION.md) passes on the available development platform, the complete CLI workflow runs against disposable fixtures/mock HTTP providers, the maintained skill's examples execute, and the final evidence report distinguishes implementation from external qualification. Missing real credentials, installed host GUIs, remote CI access, or other operating systems go in the explicit external-validation register; they do not require user input or justify dropping their underlying implementation. No claim of production readiness or universal provider/platform support follows automatically.

Technical contracts take precedence over this schedule for behavior. User instructions remain authoritative. Resolve a contract gap conservatively, record the rationale and affected contracts, and continue; do not turn an engineering choice into a user question. A simplification that removes a required capability or weakens an invariant is not an allowable default.

## Fresh checkout and local session records

Reusable guidance and curated validation summaries are tracked. `STATE.md`, worker reports, reviews and raw machine checks are ignored local session records. Read `STATE.md` when present. If it is absent, inspect current Git status/HEAD, the current contract map, the authorized task and relevant tracked validation summaries, then create a small local checkpoint before implementation. Do not assume historical leases or acceptance results are current. Preserve existing local records.

The original bootstrap below records the M0–M4 baseline; do not recreate that run for subsequent tasks. Current model preferences and authorization come from `AGENTS.md` and the session. Promote durable findings into the relevant tracked contract or curated summary; detailed logs remain local.

## Read only what the current step needs

| Artifact | When to read / owner |
|---|---|
| `STATE.md` | Every startup/resume when present; create locally when needed as described above |
| [WORK-PACKAGES.md](WORK-PACKAGES.md) | Read dependency index, then selected package sections; worker scope and acceptance |
| [DECISIONS.md](DECISIONS.md) | Startup defaults and when a new ambiguity/blocker arises; root records decisions |
| [AGENT-PROMPTS.md](AGENT-PROMPTS.md) | When dispatching or reviewing a package |
| [VALIDATION.md](VALIDATION.md) | Relevant gate rows during work; full matrix at final qualification |
| `reports/Pxx.md` | Worker evidence and completion report; assigned worker owns it until handoff |
| `reports/Pxx-review.md` | Independent review findings, root records their resolution |
| [reports/FINAL.md](reports/FINAL.md) | Curated historical M0–M4 acceptance summary; new detailed runs remain local |
| [Technical overview](../technical/README.md) | Shared contract router, then the relevant detailed document |

Initial root reading should normally fit roughly 5,000 tokens; a worker's task plus selected contracts roughly 8,000–12,000. These are context targets, not model limits or reasons to omit a needed contract. Do not fork the full conversation into every worker. Use fresh context, explicit file references, a narrow objective, and a short completion report. Keep STATE under about 120 lines and worker summaries under 400 words; detailed evidence lives on disk. Do not repeatedly replay test logs or finished research.

## Bootstrap and preserve the baseline

1. Inspect applicable instructions, `git status`, current branch/commit, available toolchain, and execution state. Do not assume the previous session's thread IDs, tests, or leases are live.
2. The known checkpoint is `4652248`. Technical designs and this execution plan may still be uncommitted. Inspect and preserve them, including untracked files. Never use `reset --hard`, `clean`, or a broad checkout to obtain a clean tree.
3. Inventory pre-existing modifications in `reports/P00.md`. Snapshot the identified planning files in a local commit when allowed by the invoked goal; never sweep unrelated work into it. Branch from that baseline to `impl/autonomous-v1`, or an unused numbered suffix. Record actual branch/HEAD. If Git mutation is unavailable, retain files in place, record that limitation, and continue writable implementation without pretending a commit exists.
4. Recheck installed Rust/Cargo. Planning observed both at 1.98.0; this is an observation, not a future guarantee. Verify dependencies against their primary manifests, pin a usable toolchain and lockfile, and implement P00. Do not silently upgrade global tools or alter unrelated user configuration.
5. Freeze the smallest shared type/schema/module contracts needed for the next packages. Select ready work from the dependency graph and assign exclusive write ownership before workers start.

Read-only source/docs access and normal dependency downloads are part of implementation. Use the environment's permitted mechanisms; do not bypass a rejected network or filesystem operation. Mock-provider tests bind loopback explicitly and never need real API credentials.

## Models, slots, and dispatch

The user starts the **root as `gpt-6-sol`**. Keep coordination, interfaces, integration, state, routine decisions, and final acceptance there. Normal implementers use `gpt-6-sol`, typically medium reasoning, high for complex parsing/integration. Assign `gpt-6-astra` a specific hard question: lossless parsing, crash recovery, generation publication, source invalidation, graph corrections, or accounting under concurrency. Astra may implement a difficult component if a bounded Sol attempt exposes a deeper design problem; it should not redo routine work.

Use at most the runtime's available slots. With four total slots, default to root + two Sol implementers + one independent reviewer/specialist. One worker is enough on the critical path. Never manufacture parallelism between dependent changes, or run multiple writers on one file. Prefer one Astra review at a consequential gate over continual whole-repository reviews. Match review depth to risk; re-review changed invariants, not the entire accepted history.

Use the harness's actual collaboration tools. In this environment, explicit model overrides require a fresh/limited fork; use `fork_turns: "none"`, an exact model name, and the prompt template. Other sessions may expose different tool schemas; inspect them instead of pasting unavailable CLI/API flags. These are implementation agents, distinct from the eventual wiki's configurable generation/embedding providers.

If slots are exhausted, reuse an idle appropriate worker with a bounded fresh packet where supported, wait for useful active work, or do ready work in root. If the runtime cannot release a thread or reset context, record it and continue sequentially. If Astra is unavailable, obtain an independent Sol review or perform a separate evidence-driven root review after implementation; retain the same gates and record the model substitution. If Sol subagents are unavailable, root proceeds. A requested model mismatch is disclosed; a worker model cannot change the already-running root.

## One integration loop

1. Select the next package whose dependencies are accepted. Decompose an oversized package into bounded internal packets without changing its completion criteria.
2. Record package state, agent/model, allowed paths, frozen interface version, and dependencies in STATE. Send a self-contained task from [AGENT-PROMPTS.md](AGENT-PROMPTS.md).
3. Workers implement and run targeted checks after root integrates any requested dependency/interface changes. They report changed paths, test commands/results, remaining defects, and requested root-owned deltas. They neither commit nor rewrite other workers' changes.
4. Root inspects the actual diff and evidence. Run independent review when the package calls for it, resolve actionable findings, and run the integration gate on a quiescent tree. A worker's assertion that tests passed is insufficient without recorded commands and matching artifacts.
5. Register only implemented commands/capabilities. Integrate shared files sequentially. Commit a coherent accepted slice locally when permitted; root is the only Git writer. Update STATE, gate evidence, PROGRESS, and decision records.
6. Continue to the next ready package without asking for milestone approval. A failed gate blocks dependent work; independent ready work may continue. Two failed attempts at the same issue trigger a smaller reproduction or specialist review, not more blind retries or a skipped gate.

Default workspace mode is a shared checkout with disjoint path leases. Root owns `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `src/lib.rs`, application wiring/CLI registry, published schemas, CI configuration, and execution state. Worker proposals for these files are text/diffs in their report until root integrates them. Workers may own leaf schema fixtures, not the authoritative schema registry. The exact lease overrides broad module suggestions in the backlog.

Only root runs repository-wide format/lint/test/commit commands while others are writing. Workers format their leased Rust files and use the agreed targeted test commands; package dependency and lockfile changes happen first through root. Serialize expensive shared build jobs unless isolated targets demonstrably help. Use `--locked` once a lockfile exists; offline mode for tests should be the normal path after dependency resolution. Independent worktrees are an optional conflict-avoidance tool after a committed baseline, not a prerequisite or a way to hide untracked files.

## Decisions, blockers, and user impact

Follow [DECISIONS.md](DECISIONS.md): choose reversible defaults, log decisions that affect behavior, defer unavailable external choices, and continue all unblocked work. Do not ask routine questions, invent credentials, purchase provider work, publish, modify real vaults, or choose the owner's legal license. The initial session may use public documentation and package registries but all product integration tests use local mocks/synthetic data.

If a permission/resource failure occurs, retain the exact error, try a permitted safe alternative, and work on independent packages. Platform-required approvals cannot be disabled by this plan. An unresolved core correctness failure is never an external-validation deferral. When every remaining path is unsafe or blocked, leave a restartable checkpoint with attempted alternatives and the precise external action required. Do not repeatedly retry identical failures. If the runtime provides persistent goal tools, follow their actual blocked/completion thresholds; never mark an unfinished objective complete or pause it without the user's request.

## Context rollover and stopping

Checkpoint at each accepted package, before risky integration, before anticipated compaction, and on any interruption. STATE must identify current branch/HEAD, dirty owned paths, active leases/agents, last passing gates, outstanding findings, decisions, and the exact next command/package. Reports carry full details. Do not include secrets or transient credential output in checkpoints.

After compaction/resume: reread local STATE when present (otherwise reconstruct it as above), inspect Git and the filesystem, list actual live agents if supported, reconcile unfinished work, and revalidate only what changed or whose evidence is uncertain. Reclaim a lease only after stopping the former worker or proving it is inactive. Completed code and test evidence survive the chat; do not restart from scratch because conversation history is gone.

When all M0–M4 implementation gates pass, write FINAL with actual commands, package commits/tree fingerprints, residual external qualification and owner decisions, and how to run the CLI. Mark completion only against [VALIDATION.md](VALIDATION.md). If the harness or user budget ends earlier, leave an accurate partial checkpoint; never shrink the objective to the work completed so far.

The choice to isolate worker context and coordinate overlapping edits follows [OpenAI's multi-agent guidance](https://developers.openai.com/api/docs/guides/agents-api/multi-agent). The short router and selective reading follow [OpenAI's guidance on skills and repository prompts](https://developers.openai.com/blog/rethinking-skills-and-prompts-for-gpt-6-astra). Model assignments and limits here are this project's execution policy, not a performance or price guarantee.
