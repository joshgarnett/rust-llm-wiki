# Bounded agent task templates

Use the actual collaboration tool schema in the running session. Exact model IDs are `gpt-6-sol` and `gpt-6-astra`; the user chooses the root model before starting. This document does not install a new agent service or require API keys.

## Implementation packet

Root fills the fields, records the lease in STATE, then launches one Sol worker with fresh context. Provide only the selected package section and links; do not attach all designs or previous messages.

```text
Role: implementation worker, model gpt-6-sol.
Repository: <absolute workspace root>.
Task: <Pxx and bounded internal packet, objective in one sentence>.
Baseline: <HEAD or tree fingerprint>; interface revision: <hash/reference>.
Dependencies accepted: <IDs + evidence references>.
Read: AGENTS.md, the <Pxx> section of docs/execution/WORK-PACKAGES.md,
      <only the technical sections and source interfaces needed>.
Write lease: <exact files/directories, including tests and report>.
Shared files: propose deltas in your report; do not edit Cargo files,
              public schemas/types, lib.rs, CLI registry, or execution STATE.
Required behavior: <concrete inputs/outcomes and preserved invariants>.
Acceptance: <named tests/commands, expected errors and failure injections>.
Out of scope: <adjacent packages and explicitly deferred external checks>.

Implement this packet end-to-end, with meaningful tests and concise comments.
Use disposable vaults, synthetic data, fake credentials, and local mock HTTP.
Do not read real credentials, spend on APIs, install into host directories,
change Git state, or ask the user questions. Do not delegate further.
Preserve unowned edits. Request shared-interface changes from root by message.
If blocked, record evidence and an alternative; continue owned independent work.
Do not mark tests passed without running them. Document unavailable checks.

Write docs/execution/reports/<Pxx[-packet]>.md with changes, exact checks,
test source fingerprint, requested shared deltas, findings, and next action.
Final message <=400 words: completion status, changed files, checks/results,
remaining blockers/risks, report path. Return ownership to root after handoff.
```

The template is an assignment boundary, not a requirement to reread AGENTS before every edit. Worker tests may initially require root to register its module/dependencies. Ask root, continue useful owned work, and test after integration; do not create a duplicate build scaffold to evade ownership.

## Astra specialist packet

Use for a hard invariant or after an evidence-backed implementation failure. Usually read-only first; assign a narrow write lease separately if its proposed fix is accepted.

```text
Role: specialist, model gpt-6-astra, fresh context.
Question: <one concrete uncertainty, e.g. recovery between FilesApplied/Indexed>.
Contracts: <exact document sections/types>.
Implementation/evidence: <source files, reproduction, failure log excerpt>.
Scope: read-only; no user questions, commits, broad refactor, or subagents.

Find the smallest compatible resolution. Analyze counterexamples and failure
ordering. Preserve the stated invariants; do not remove capabilities to pass.
Return: recommended resolution, rejected alternative(s), exact affected files,
required regression/fault tests, and any irreducible blocker. <=700 words.
Write longer necessary evidence to the assigned specialist report.
```

## Independent review packet

Reviewers should not be the author of the component. They may read/run targeted tests under root's test lease; they do not edit the component while reviewing it.

```text
Role: independent reviewer, model <gpt-6-sol or gpt-6-astra>.
Review: <Pxx or combined gate>, code frozen at <HEAD/tree fingerprint>.
Read: <requirements, implementation diff, test/report paths>.
Scope: read-only; root grants test lease <yes/no>; no user questions/commits.

Check behavioral compliance and missing failure paths, not formatting trivia.
For storage inspect bytes, expected hashes, journals, publication and recovery.
For providers inspect endpoint trust, offline/dry-run, attempts, reservations,
unknown billing, crash replay, and source/space changes in flight.
For graphs inspect ambiguity, complete review, source invalidation, citations,
and interleaved apply behavior. Use only the relevant list for this assignment.

Record actionable findings with severity, path/location, concrete trigger,
expected versus actual behavior, and a minimal verification case. Distinguish
observed failure, code reasoning, and an unavailable external check.
Write docs/execution/reports/<Pxx>-review.md and return <=400 words.
If no blocking findings, say so without implying unrun checks passed.
```

Root classifies findings: invariant/data-loss/accounting bugs block acceptance; genuine external qualification is logged; optional polish is not allowed to prevent completing unrelated core work. Record the disposition of every actionable finding before accepting the package. Re-review the changed risk area only.

## Worker evidence format

Each report begins with package/packet ID, author/model, base commit/tree, owned paths, result (`ready for review`, `partial`, `blocked`), and exact source fingerprint. Follow with changed behavior, completed checks, findings/limitations, shared-file proposals, and next action.

Test rows contain `gate; command; working directory; toolchain/OS; result; tested fingerprint; log or relevant output`. Record failures and their resolution; do not overwrite history with only the final green run. Large logs may stay in an ignored local artifact directory, but preserve a compact textual result and reproduction in the report so a new session can act without that log.

Use readable messages with spaces and short excerpts. Do not send full source files or raw provider/credential output to the root chat. A task's success must be inferable from durable code and evidence, not the agent's memory.

## Reassigning after context loss

Root verifies the previous worker is inactive, reconciles its dirty paths, and sends a new worker the same objective plus the last report, accepted changes, remaining failing test, and lease. Fresh workers must not revert unfinished work merely because it is uncommitted. If the runtime supports goal continuation, resume that goal; these templates do not create a new root session automatically.
