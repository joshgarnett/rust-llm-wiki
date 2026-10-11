# Developer documentation

Build and test lwiki with disposable wikis, then use the contracts to understand
how a change affects files, citations and recovery. Start with the
[build guide](builds.md) and the [current contract map](current-contracts.md).

| Work | Reference |
| --- | --- |
| Build and run checks | [Builds](builds.md) and [build checkpoints](build-checkpoints.md) |
| Implement a command or change durable storage | [Contracts](contracts/README.md) |
| Understand design choices | [Design decisions](design-decisions.md) and [large-vault design](large-vault-design.md) |
| Choose validation and qualify a release | [Validation and limits](validation-status.md) and [external qualification](qualification.md) |
| Review a user's complete task independently | [Critic workflow](testing-usability.md) |
| Evaluate context and answer completeness | [Context evaluation](evaluating-context.md) and [quality targets](rag-quality-targets.md) |
| Acquire and adapt evaluation data | [Dataset reference](eval-datasets-quick-reference.md), [acquisition](eval-datasets.md), [ConditionalQA](evaluating-conditionalqa.md) and [ContractNLI](evaluating-contractnli.md) |
| Measure capacity and resources | [Large-vault testing](testing-large-vaults.md), [full-check benchmark](benchmark-full-check.md) and [query limits](query-limits.md) |
| Work on remote calls and accounting | [Embeddings](embeddings.md) and [providers/jobs](contracts/providers-jobs.md) |
| Work on reviewed facts and graph data | [Knowledge graph](knowledge-graph.md) and [record schemas](contracts/record-schemas.md) |
| Coordinate implementation agents | [Agent workflow](agent-workflow/README.md) |

## Maintain documentation

Write user guides around the reader's task. A reader should be able to install
lwiki, create a lasting wiki, connect an agent and use their own notes without
consulting developer references. Follow [the writing guidelines](../../AGENTS.md#writing-guidelines)
and have an independent reviewer follow the main task before accepting a change.

Keep reusable instructions, contracts, decisions and established limits tracked.
Put working plans, checkpoints, raw logs, reviews and unfinished checks under
ignored `.work/<task>/`. Promote useful conclusions into the relevant reference
instead of publishing a worklog.

The [documentation policy](documentation-policy.json) records each published
file's reader and purpose. Update it when adding or moving a document. Run:

```sh
python3 scripts/check_docs.py
python3 scripts/test_check_docs.py
```

These checks catch inventory errors, broken local links and anchors, diary
patterns and punctuation issues. They do not establish that the instructions
are complete, natural or useful; task-based review does that.
