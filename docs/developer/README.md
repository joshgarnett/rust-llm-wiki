# Developer documentation

Use the [current contract map](current-contracts.md) to find an implementation
contract and relevant tests. It describes current behavior; local execution state
and raw experiment logs are intentionally outside the published documentation.

| Area | Entry point |
| --- | --- |
| Build and validate a native candidate | [Builds](builds.md) and [build checkpoints](build-checkpoints.md) |
| Public interfaces and durable invariants | [Contracts](contracts/README.md) |
| Architectural rationale and measured limits | [Design decisions](design-decisions.md) and [large-vault design](large-vault-design.md) |
| Current qualification boundary | [Validation status](validation-status.md) and [external qualification](qualification.md) |
| Independent usability/quality acceptance | [Critic workflow](testing-usability.md) |
| Context sufficiency and honest holdouts | [Context evaluation](evaluating-context.md) and [quality targets](rag-quality-targets.md) |
| Dataset provenance/adapters | [Dataset reference](eval-datasets-quick-reference.md), [acquisition](eval-datasets.md), [ConditionalQA](evaluating-conditionalqa.md), [ContractNLI](evaluating-contractnli.md) |
| Resource and capacity protocols | [Large-vault testing](testing-large-vaults.md), [full-check benchmark](benchmark-full-check.md), [query limits](query-limits.md) |
| Remote contracts | [Embeddings](embeddings.md) and [providers/jobs](contracts/providers-jobs.md) |
| Graph authority | [Knowledge graph](knowledge-graph.md) and [record schemas](contracts/record-schemas.md) |
| Agent implementation collaboration | [Agent workflow](agent-workflow/README.md) |

Historical run diaries remain in Git history. Ongoing plans, checkpoints, raw logs
and per-attempt reviews live locally under ignored `.work/<task>/` or existing
ignored evidence locations. Promote stable implemented findings here; do not
publish a diary archive or require ignored artifacts to understand the contracts.

The [documentation policy](documentation-policy.json) records the reviewed purpose
and classification of each published document. Review both content and purpose
when changing that inventory. Run `python3 scripts/check_docs.py` to check visible
paths, fresh-clone links/anchors, structural diary signals and prose punctuation.
`python3 scripts/test_check_docs.py` verifies those checks on disposable fixtures.
Writing guidance is maintained in [AGENTS.md](../../AGENTS.md#writing-guidelines).
Code and quoted evidence remain exact; grammar and clarity still need human review.
