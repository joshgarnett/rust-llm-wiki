# Agent implementation workflow

This is reusable collaboration guidance. It does not authorize an implementation
run, spending, publishing or host installation. Start from the user's current task,
[AGENTS.md](../../../AGENTS.md), the [contract map](../current-contracts.md) and only
the relevant detailed contracts.

## Startup and scoped work

Inspect Git status/HEAD and preserve user changes. Read the authorized local task
and checkpoint if present; otherwise create a small local checkpoint under ignored
`.work/<task>/`. Do not reload all historical research, diaries or conversations.
Record task scope, source baseline, invariants, unresolved findings and next step.

The orchestrator owns shared interfaces, Cargo files, CLI registry, schemas, commits
and integration state. Workers own exclusively leased paths plus a local evidence
report. Follow current session model preferences; for bounded work prefer
`gpt-6.1-sol`, and independent difficult architecture/storage/accounting review
`gpt-6-astra` where available. Markdown does not switch the running model.
No nested delegation without an explicit orchestrator grant.

## Critic and architecture challenge

For consequential quality work, freeze representative tasks, observable success,
correctness blockers and acceptance thresholds with an independent critic before
implementation results. The critic examines actual public commands and returned
content, preserving missing facts, failures and regressions. Keep completion,
citation integrity, relevance and completeness separate. A required high-score gate
remains active until every mandatory condition passes.

Before consequential architecture changes, obtain independent review of the
measured failure, expected benefit, simpler choices, scaling cost and migration.
Trace actual public inputs and allocation invariants before introducing repair.
After two interventions without clear whole-task improvement or repeated regressions,
stop local parameter tuning and request a fresh complete-data-flow review. Continue
independent authorized work while reviewing.

## Integration and validation

Batch related work into a coherent end-user workflow. Before an expensive build,
name the batch, changed inputs and actual uncertainties its checks resolve. Use
cheap local checks while editing, focused checks on the integrated slice and broad
native suites at consequential or final gates. Repeat passing checks only for changed
code/environment, a failure, an unresolved concern or final acceptance.

Serialize shared builds. Long performance measurements retain pinned executable,
inputs, compiler/profile and protocol with no competing work. Disposable fixture
vaults and mocks need no production access. Live work requires existing user
authorization, finite limits and protected credentials. Mocks, cross-builds and
process kills do not establish live interoperability, native recovery or power loss.

Commit coherent tested milestones only through the orchestrator. Update local state
after integration, before compaction and before stopping. Preserve raw logs/reviews
locally, including failed attempts; promote stable findings into maintained
contracts/decisions and [validation status](../validation-status.md). A checkpoint
preserves unfinished progress and does not assert acceptance.

See [decision procedure](DECISIONS.md), [worker packets](AGENT-PROMPTS.md) and
[validation gates](VALIDATION.md). Historical M0–M4 schedules are Git history, not
fresh execution authority.
