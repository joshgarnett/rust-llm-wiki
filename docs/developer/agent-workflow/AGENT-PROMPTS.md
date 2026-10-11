# Bounded agent task templates

Give workers only the task, relevant contracts and current evidence. Do not send
the full historical implementation record to an independent critic.

## Implementation packet

- Concrete end-user outcome and observable required behavior.
- Relevant files/contracts and exact exclusive write paths.
- Shared API/schema boundaries owned by the orchestrator.
- Constraints: disposable fixtures, no production access, finite authorized
  provider limits, no nested delegation unless granted.
- Required local evidence report: changes, exact checks/results, failures,
  unresolved findings and the next integration step.
- Build coordination: inexpensive checks allowed; shared builds scheduled by root.

## Architecture or invariant review packet

State the measured problem and current public workflow. Ask the reviewer to challenge
expected benefit, simpler alternatives, identity/storage/accounting invariants,
scaling cost and migration. Supply relevant source/contracts and frozen failure
commands. Require a bounded distinguishing experiment for uncertain tradeoffs.
Reviewers do not implement or see hidden acceptance labels unless their role needs them.

## Independent acceptance packet

Freeze representative tasks, required propositions, citation alternatives, bounds,
blockers and the acceptance threshold before candidate results. Supply the pinned
executable/source/compiler settings and public docs. Require actual argv/cwd/exit,
returned bytes/citations, monotonic elapsed time, every exclusion/regression and
separate completion/integrity/relevance/completeness judgments. Replay preserved
failures after fixes; newly exposed cases become development data.

## Handoff and resume

Keep a small local report under `.work/<task>/reports/`; exact outputs may live in
adjacent ignored logs. Root integrates shared state and tested commits. On context
loss, resume from current local state and leases; do not restart accepted work or
repeat unchanged checks without a concrete correctness/final-gate reason.
