# Evaluation dataset reference

Use public datasets with provenance, terms and immutable hashes, plus independently
authored realistic and synthetic cases. Acquired/prepared data, passing mechanics
and high development scores are different from accepted end-user quality.
[Native default completeness and representative capacity remain unqualified](validation-status.md).
This catalog records available families and adapters, not a new acquisition or run.

## Dataset families

| Family | Useful coverage | Preparation and limits |
| --- | --- | --- |
| QASPER v0.3 | Long scientific documents, paragraph/sentence evidence, missing information | [Supported acquisition/adapter](evaluating-context.md#acquire-and-prepare-from-a-fresh-checkout); preserve full-paper notices and CC BY 4.0 dataset attribution. Evidence location alone does not establish semantic completeness. |
| ConditionalQA v1_0 | Scenarios, applicability conditions, exceptions, absence | [Supported mapped-Markdown adapter](evaluating-conditionalqa.md); retain scenario plus question and complete answer/condition sets. BSD software notice does not replace the CC BY-SA data grant. |
| ContractNLI | Entailment, contradiction, NotMentioned and distant exceptions | [Train-only adapter](evaluating-contractnli.md); noncontiguous spans and three-way classification are separate from retrieval coverage. Repeated hypotheses cannot dominate task-family scores. |
| WixQA ExpertWritten | Real support questions spanning several articles | [Acquisition/adaptation](eval-datasets.md); expert article IDs lack byte-span/proposition gold, which must be independently authored before evaluation. Preserve publisher declarations and snapshot identity. |
| MuSiQue-Full v1.0 | Two-to-four-hop chains and missing-bridge controls | Proposed scoped pairs; archive/hash verification, acquisition and supported adaptation remain pending. Metadata is not acquired corpus. |
| Cargo release documentation | CLI/configuration, cross-file instructions and revision controls | Two immutable release snapshots were acquired; new questions/labels remain independent work. Preserve source revision/notice/hash. |
| T²-RAGBench / FinQA dev | Text/tables, numerical operands, entity/year ambiguity | Prepared source-only development input; complete evidence and arithmetic reasoning require independent judgment. Table adaptation is not rich-document qualification. |
| BRIGHT | Reasoning-intensive developer/domain retrieval | Acquired domain and ordinary-import development controls; general span mapping/upstream query evaluation remain separate. Source hits did not establish complete native context. |
| DevDex | Paraphrased documentation, issue/PR and repository tasks | Tasks/notices acquired; linked source snapshots and sufficient-span labels remain pending. |
| Public Rust/SQLite/CLI documentation | Practical setup, conflict, refresh, history and cited-Page workflows | Known development inputs; official documentation snapshots and revision hashes must be pinned. Small scoped workflows do not pass native HIGH. |
| Repository fixtures | Identity, UTF-8, ambiguity, graph direction, freshness, budgets and interruption | Bootstrap/P08/P21 and local lifecycle fixtures exercise mechanics. Synthetic vectors do not establish semantic ranking gains. |
| Independent unseen tasks | Frozen whole-task completeness and scoped absence | Keep labels quarantined outside indexed content. A case exposed to implementation becomes development data. |
| Wikimedia article proposal | Future representative whole-article capacity control | Researched shard metadata is not acquired/imported payload or proof of 25K eligible documents/2.5GB content. |

The detailed [acquisition guide](eval-datasets.md) retains original source URLs,
licenses, hashes and adapter constraints. [QASPER](evaluating-context.md),
[ConditionalQA](evaluating-conditionalqa.md) and [ContractNLI](evaluating-contractnli.md)
have tracked reproducible preparation scripts. Other locally acquired research
manifests need an explicit supported adaptation before being used as an evaluator input.

## Split and integrity discipline

Upstream `test` does not mean unseen to this implementation: every inspected/tuned
public example is development data. Freeze corpus/task hashes, source families,
propositions, acceptable citation alternatives, answerability and protocol before
candidate output. Preserve original downloads/notices, extraction/transformation
maps and exact normalized UTF-8 labels. Keep questions/answers outside the source
corpus. Record excluded tasks and ambiguous offsets; mapping failure must not
silently improve the denominator.

Compare baseline and candidate on identical immutable Source revisions, caches,
query vectors and bounds. Acquire every authorized query embedding before paired
offline replay; remote preparation history can otherwise change the index. Grade
actual final returned text, not owner hits or gold paragraph overlap. Separate
completion, citation identity/byte/freshness integrity, relevance, proposition
coverage, absent-information handling and operational errors.

## Existing evidence and next runs

Historical development exposed missing conditions after relevant owners were
retrieved, incomplete host/Page reconciliation and rejected regressions. Preserve
those failures in Git history/local evidence and do not cherry-pick successful
families or modes. See [validation status](validation-status.md#retrieval-quality).

For a new run use [context evaluation](evaluating-context.md),
[critic workflow](testing-usability.md), [quality targets](rag-quality-targets.md)
and the separate [capacity protocol](testing-large-vaults.md). Report all added
model/host input, calls, latency and known cost; unavailable usage remains unavailable.
Scores qualify only the declared workflow, executable, corpus and budgets.
