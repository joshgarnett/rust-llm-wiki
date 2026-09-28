# Start the autonomous implementation session

Open this repository in a new session with **`gpt-6-sol` selected as the root model**. The original “gpg-6-sol” spelling is treated as a typo. A prompt cannot switch an already-running root model. The goal below explicitly requests Sol workers and Astra specialists, subject to available tools/slots.

Paste the following as the new session's goal/request. If its interface has a persistent goal feature, use this objective there; otherwise paste it as an ordinary task. Do not invent a `/goal` shell command. No new API account or orchestration framework is required.

```text
Implement the Rust LLM wiki CLI in this repository through M0–M4, following
AGENTS.md and docs/execution/README.md. Start by reading
docs/execution/STATE.md, the defaults in docs/execution/DECISIONS.md, and the
dependency index plus P00 in docs/execution/WORK-PACKAGES.md. Read remaining
technical contracts only as each package requires them.

Use gpt-6-sol as the main orchestrator. Delegate bounded implementation to
gpt-6-sol subagents and difficult invariant analysis/independent reviews to
gpt-6-astra. Use fresh, focused contexts, disjoint write ownership, compact
reports, and the actual available concurrency. Follow the recorded fallback
policy if a worker model or collaboration tool is unavailable.

I authorize autonomous repository edits, normal dependency resolution,
builds/tests using disposable vaults and mock providers, and local baseline
and implementation checkpoint commits. Preserve existing uncommitted work.
Do not push, publish, rewrite history, install into real host profiles, modify
real vaults, obtain secrets, or spend on live APIs. Prepare production adapters
and opt-in live checks; use fake credentials and local mocks for this run.

Do not ask routine questions or stop for milestone approval. Choose reversible
defaults, record material decisions and external deferrals, and continue all
safe unblocked work. Never defer a core correctness failure or missing feature
as an external check. Respect platform permissions and user/system limits.
Stop incomplete only when no safe meaningful in-scope progress remains.

Maintain STATE.md, package/review reports, validation coverage, decision logs,
and PROGRESS.md after each integration and before compaction/interruption.
Resume from those files rather than restarting. If persistent goal tools are
available, create/continue this objective without inventing a token budget.

Finish all required packages and local gates in docs/execution/VALIDATION.md;
do not stop at M1 or M2. Deliver a working CLI/library, graph and retrieval,
skill export, remote-provider adapters, resumable bounded research, tests,
and docs. Record real evidence in docs/execution/reports/FINAL.md. Distinguish
local implementation completion from still-unverified real-provider,
host-app, other-platform, and release checks. M5 and publication are out of scope.
```

The run persists while its session/goal runtime remains active. Context compaction can be handled through the checkpoint; this plan cannot keep executing after the host stops the session. To resume an interrupted run, use the same goal and STATE rather than a new competing implementation.

No implementation or goal was launched by creating this plan.
