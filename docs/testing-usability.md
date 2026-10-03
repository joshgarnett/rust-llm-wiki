# End-user quality and independent critic workflow

Use this guide after consequential CLI, retrieval or documentation changes. The [2026-10 usability evaluation](execution/reports/UX-VALIDATION.md) is a worked example; [native qualification](qualification.md) covers additional provider/platform/recovery boundaries. Agent walkthroughs complement recruited-user testing; they do not measure how real beginners feel or establish population success rates.

## Set up an independent critic

Give a fresh critic the current binary, a disposable workspace, the README and a concrete task. Keep implementation reports out of its first walkthrough so explanations do not conceal discovery problems. A practical assignment is:

> Act as a critical new user of this CLI. Read the public getting-started instructions, execute their examples in a disposable wiki, then complete capture, search, cited context, page editing, source refresh, withdrawal and export tasks. Try missing inputs, empty results, small budgets, spaces in paths and conflicting edits. Record exact commands, exit codes, stdout/stderr, expected versus observed behavior, severity and recovery effort. Inspect implementation only after reproducing an issue. Do not use credentials or real user vaults. Do not modify the product while reviewing it. Return early actionable findings and independently replay fixes. Treat source retrieval, exact citation bytes and semantic answer support as separate outcomes.

The orchestrator grants exclusive file leases to implementation workers and owns shared interfaces, builds and execution state. Use a separate review pass for changes the critic implements. Keep the previous failing binary/output and a hash of each tested binary. A worker saying “passed” is insufficient without commands, artifacts and assertions.

## Task matrix

| Area | Tasks and success evidence |
| --- | --- |
| First use | Start with a clean directory and only documented prerequisites. Every input is created before use; every command produces the promised known result. Record manual ID-copy steps and unknown placeholders. |
| Discoverability | Top-level/subcommand help, defaults, examples and errors lead to the next task. Copy continuation commands from a working directory outside the wiki, including paths containing spaces and apostrophes. |
| Human output | Distinguish successful mutation, staged change and preview. Expose IDs, continuation cursors, omissions and guard hashes where needed. Summaries escape terminal controls; read/context stdout preserves exact content for pipelines. |
| Machine output | Check exit status, schema envelope, warnings, network/freshness flags and clean JSON/JSONL. Do not parse the human presentation as a stable API. |
| Failure/recovery | Missing files, unsupported targets, malformed input, stale guards, request caps, credentials, paid-response rejection and unknown outcomes give safe actionable guidance. User changes and unknown accounting holds remain intact. |
| Retrieval quality | Use realistic long and short documents, related distractors, exact identifiers, paraphrases, multiple-source questions and absent facts. Freeze query labels before seeing model results. |
| Evidence | Independently compare cited spans to original UTF-8 bytes and hashes. Refresh or withdraw a source and verify current versus historical eligibility. Exact bytes do not establish entailment. |
| Context usefulness | Read the actual returned excerpts. Rate whether they answer all, some or none of the requested question; document the missing proposition. Correct source IDs alone are inadequate. |
| Operational modes | Verify offline/dry-run network behavior and dry-run byte/mtime purity. Repeat sync/query to demonstrate cache reuse. Check explicit bounds and labels when work is incomplete. |
| Wiki maintenance | Create readable topic/overview pages, preserve authored edits, retain capture provenance, export, back up and restore, then check and retrieve the same evidence. |
| Regression | Run affected Rust targets, strict formatting/Clippy and maintained smoke/regression gates. Use the full native fault matrix for changed durability/accounting invariants. Record unavailable platforms instead of claiming native proof from compilation. |

## Research and scoring

[CLI Guidelines](https://clig.dev/) supplies concrete help, output, error and composability checks. [Diátaxis](https://diataxis.fr/quality/) separates functional correctness from whether documentation fits the reader's need; organize learning, task instructions, reference and explanation accordingly. [Write the Docs](https://www.writethedocs.org/guide/writing/docs-principles/) motivates useful, discoverable, maintainable examples. [NN/g's task-success guidance](https://www.nngroup.com/articles/success-rate-the-simplest-usability-metric/) supports testing concrete outcomes and fixing/retesting observed problems; [severity guidance](https://www.nngroup.com/articles/how-to-rate-the-severity-of-usability-problems/) motivates prioritizing impact, frequency and persistence.

Report blocked, completed-with-friction and completed tasks separately. For retrieval, report expected-source hit@1/@5, reciprocal rank and source recall alongside passage usefulness; exclude known-unanswerable questions from positive recall denominators and report them separately. Do not average ordinal usefulness labels into a fabricated satisfaction percentage. Keep equal result/candidate/context budgets across modes, disclose changes to excerpt size, and distinguish cold live-query latency from cached offline latency.

For public-source corpora, retain URL, author, retrieval timestamp, original download, extraction method, license notice, source/revision identity and content hash. Keep draft synthesis distinguishable from publisher text. When metadata is wrong, refresh it as a new immutable source revision rather than rewriting historical evidence.

## Live calls and completion

Live provider validation requires explicit authorization, trusted endpoint/vault bindings and finite request/data/deadline limits. Keep credentials in a private file or temporary process environment; never put them in logs, argv, wiki content or reports. Record reported usage separately from monetary cost and retained unknown reservations. Passing one embedding endpoint does not qualify generation or other gateways.

For each issue, preserve the failing example, implement the smallest justified fix, add a regression for consequential behavior, rerun the example and ask the critic to assess the result. Close with the exact tested tree/binary, actual gates, before/after outcomes, residual quality limits and a useful inspectable wiki. Preserve unsuccessful runs as evidence. Update execution state before stopping.
