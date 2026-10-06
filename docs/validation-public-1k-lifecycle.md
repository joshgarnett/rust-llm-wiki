# Public 1,000-source lifecycle observation

Updated 2026-10-05. Independent review passes the declared 1K mechanics:
**10/10 checklist checks**, with 700 actual query workflows and 100/100 equivalent
post-update/rebuilt result sequences. The original recorder remains **FAILED**;
a separate read-only completion supplies the remaining rebuild evidence.
This is a scoped development observation. **25K capacity, native HIGH and the
full-capacity critic gate remain open; 10K admission is on hold.**

## Inputs and public workflow

The native macOS ARM64 0.2.0 candidate-008 uses release optimization level 3.
Its executable SHA-256 is
`4f42929b5e97c93e64772eaabc8384ba882f2957c51a05b03897238c0234115e`.
The pinned executable was reused without a new build. No provider call or live
vault access was used for this observation.

The frozen corpus contains 1,000 distinct Sources and **100,302,609 UTF-8 bytes**:
998 varied synthetic documents and two licensed Cargo 0.85 documentation files.
The fixture includes early, middle and distant facts, Unicode, distractors and
100 independently authored development questions. There are 20 questions each for
exact identity, multiple terms, distant facts, two-source facts and filters;
the filtered group includes ten absent controls. Labels remain outside indexed
content. The [dataset reference](eval-datasets-quick-reference.md) explains this
fixture's relationship to the public datasets and private acceptance questions.

Actual public commands cover:

- Two bootstrap captures, guarded storage migration and normalized activation,
  then 998 imports in 125 groups, with eight items per group and at most 16 groups
  per run/resume window.
- A reached SIGKILL cut during pending publication, ordinary `recover` and
  continuation. Pending identities, acknowledged allocations and capture timestamps
  survive. Immutable payload bytes are verified; churn/rebuild inventories also
  verify their mtimes.
- Publication of 1,000 generated Page drafts, plus a bounded graph fixture with
  four Entities and two Assertions. Draft publication does not demonstrate useful
  cited Page synthesis or guarded host editing.
- Ten Source refreshes, one withdrawal, historical reading, no-op controls and
  full checks. The final Current set contains 999 Sources.
- Three 100-question rounds before and after changes, a verified complete backup,
  complete cache removal, offline normalized rebuild, another full check and
  100 query replays. Canonical and retained files remain preserved.

The frozen question recipe uses native keyword queries: lexical search followed
by indexed-document context and bounded Source reads, at most eight CLI calls
per task. Context is limited to 6,000 UTF-8 bytes/1,500 estimated tokens and
65,536 inspected entries.
Evidence completeness is graded separately for native context and the combined
reading recipe. There is no answer-generation stage or natural-question claim.

## Independent results and preserved failure

The main owner executed 2,963 CLI calls and all 600 initial/post-update tasks.
It stopped on the second rebuilt task because its whole-packet comparison treated
a publication-bound continuation cursor's changed generation/identity as a
substantive difference. Independent decoding verified unchanged query, offset,
parser and version, with each cursor correctly bound to its own publication.
No product regression was found. The failed original result is preserved.

A separately frozen read-only tail executes exactly the 98 unrun tasks through
461 successful CLI calls and reuses the two saved rebuilt packets. It imports,
rebuilds and reruns nothing already completed. The independent comparison validates
cursor structure and binding before normalizing only publication-derived identity
fields; all other substantive fields remain compared under the predeclared
publication/observation metadata exclusions. **All 100 rebuilt sequences match.**

Across the complete observation, **5,801 structured Source-citation occurrences
and 2,604 rendered context citations** have zero byte, quote-hash, locator,
revision or eligibility errors. The critic also verifies both backups, immutable
Source allocations, 2,085 committed corpus files and final vault seals. Query-only
inventories change only the permitted SQLite SHM coordination file.

| Evidence sufficient for the question | Native context | Combined reading recipe |
| --- | ---: | ---: |
| Initial, each 100-question round | 78/100 | 89/100 |
| After changes, each 100-question round | 79/100 | 90/100 |
| Rebuilt, 100 questions | 79/100 | 90/100 |
| All 700 executions | 550/700 (78.57%) | 627/700 (89.57%) |

Missing distant facts, Cargo option details and identification of unavailable
support remain unresolved. Rebuild parity preserves these deficits. Expected
owner hits and exact citations do not establish answer completeness.

## Timing, storage and next decision

Active public import took **359.372 seconds**. Initial/post-update whole-task
p95 was **1.278/1.292 seconds**; native query CLI maximum was 0.391 seconds.
Observed query RSS reached 52.8 MB and the longest full check took 10.892 seconds.
The two owners' separate monotonic intervals total 1,084.198 seconds; separately
observed UTC turnaround is charged against the experiment budget. Observer work
is disclosed. Faster rebuilt-tail timings are not a paired speedup result.

The complete observation records 3,424 CLI calls and 155,116,645 retained output
bytes. Maximum observed experiment allocation is **2,920,808,448 bytes** and
minimum recorded free space is 63,703,605,248 bytes. Exact active allocation/tree
peaks, physical SQLite I/O and the complete logical import-work ledger remain
unavailable. Recorded finite phase limits pass; these observations do not establish
the full large-vault resource contract.

The worst 128-item import window adds 171,667,456 allocated bytes. The existing
conservative growth/backup guard blocks a 10K run. A fresh architectural review
finds the selected WAL at **470,635,872 logical bytes**, versus 4,152 after rebuild;
it explains 98.77% of logical cache reduction. Main database size changes only
260,407,296→259,260,416 bytes. A bounded read-only header observation confirms a
consistent current WAL/SHM header state; recovered SHM metadata cannot establish historical
checkpoint work. Deleting the WAL would be unsafe.

The independently reviewed next architectural milestone is bounded WAL reclamation through ordinary publication,
preserving external readers, FULL durability and recovery. Its preimplementation
gate requires logical and allocated WAL ≤64 MiB at existing unpinned checkpoints,
active import ≤449.215 seconds and unchanged correctness/resource checks. This
does not authorize a schema migration, replay-format redesign or periodic rebuild.
Larger-tier admission must be reconsidered from actual candidate measurements.

The subsequent [WAL lifecycle comparison](validation-wal-lifecycle.md) passes
its independent scoped gate. Ordinary publication keeps the selected WAL within
64 MiB at every applicable observation and completes the same 700 tasks with
unchanged evidence grades. The separate growth review still retains 10K HOLD;
this improvement does not establish the full capacity or native HIGH gates.

## Evidence bindings and scope

Optional local receipts are under
`.artifacts/representative-25k-public-lifecycle-001/`; this tracked summary remains
usable without those ignored files. Corpus source-manifest SHA-256:
`780237515d37b3d14b2b47e711355d63ca37c4db9bf6e121e56ba171bd7d8d08`.
Public task SHA-256:
`e80952ec9b3efdbcf096b26bdf954d4c9a18581a005a816011c44bb3b3098d20`.

| Retained evidence | SHA-256 |
| --- | --- |
| Original failed owner result | `1fd7ab08eb4b453ad3f96a4b7fac6c020d034ec434e4c20245867efceb73c601` |
| Main independent audit JSON | `4300022a9016374ef4b4c90f061ca3a852b3ce9ee5476c800b7784b0daf85a78` |
| Separate tail result | `9ef3a2987018a492b47afe167531a27338d090f4b858c836711a9954b3cc4f28` |
| Final independent tail audit JSON | `82f3be6e843383694e998cd5e40bcbe1ebb96bdd5175a2c3785ced8de165d8a6` |

The mechanics checklist is not the mandatory ≥90/100 capacity critic score.
Strict/historical and semantic/hybrid mode qualification, the 1,000-change and
add/delete gates, useful cited Page workflows, native HIGH, 25K and other-platform
or power-loss qualification remain open. The earlier
[mixed-binary representative diagnostic](validation-representative-1k-workflow.md)
and all its failed owners remain separate historical evidence.
