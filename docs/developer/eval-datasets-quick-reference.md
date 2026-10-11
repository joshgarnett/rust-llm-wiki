# Evaluation dataset reference

Choose a dataset for the failure you want to test, then preserve provenance,
terms and immutable input hashes. Supplement public examples with independently
authored realistic and synthetic cases. Preparation and passing mechanics do
not establish end-user quality.

## Dataset families

| Family | Useful coverage | Preparation and limits |
| --- | --- | --- |
| QASPER v0.3 | Long scientific documents, paragraph/sentence evidence, missing information | [Supported acquisition/adapter](evaluating-context.md#acquire-and-prepare-from-a-fresh-checkout); preserve full-paper notices and CC BY 4.0 dataset attribution. Evidence location alone does not establish semantic completeness. |
| ConditionalQA v1_0 | Scenarios, applicability conditions, exceptions, absence | [Supported mapped-Markdown adapter](evaluating-conditionalqa.md); retain scenario plus question and complete answer/condition sets. BSD software notice does not replace the CC BY-SA data grant. |
| ContractNLI | Entailment, contradiction, NotMentioned and distant exceptions | [Train-only adapter](evaluating-contractnli.md); noncontiguous spans and three-way classification are separate from retrieval coverage. Repeated hypotheses cannot dominate task-family scores. |
| WixQA ExpertWritten | Real support questions spanning several articles | [Acquisition/adaptation](eval-datasets.md); expert article IDs lack byte-span/proposition gold, which must be independently authored before evaluation. Preserve publisher declarations and snapshot identity. |
| MuSiQue-Full v1.0 | Two-to-four-hop chains and missing-bridge controls | Verify the official archive and preserve paired instance scope; a missing bridge must not be restored by another instance. |
| Cargo release documentation | CLI/configuration, cross-file instructions and revision controls | Use two immutable release snapshots and independently authored questions/labels. Preserve source revision/notice/hash. |
| T²-RAGBench / FinQA dev | Text/tables, numerical operands, entity/year ambiguity | Keep source-only input separate from labels; complete evidence and arithmetic reasoning require independent judgment. Table adaptation is not rich-document qualification. |
| BRIGHT | Reasoning-intensive developer/domain retrieval | Preserve short/long source settings and map supporting spans independently; source relevance labels alone do not establish complete context. |
| DevDex | Paraphrased documentation, issue/PR and repository tasks | Pin the linked source snapshots and author sufficient-span labels independently. |
| Public Rust/SQLite/CLI documentation | Practical setup, conflict, refresh, history and cited-Page workflows | Pin official documentation snapshots and revision hashes; author task families before the evaluation. |
| Repository fixtures | Identity, UTF-8, ambiguity, graph direction, freshness, budgets and interruption | Local lifecycle fixtures exercise mechanics. Synthetic vectors do not establish semantic ranking gains. |
| Independent unseen tasks | Frozen whole-task completeness and scoped absence | Keep labels quarantined outside indexed content. A case exposed to implementation becomes development data. |
| Wikimedia article proposal | Representative whole-article capacity control | Verify eligible article payloads and total content bytes; shard metadata alone does not establish the requested scale. |

The detailed [acquisition guide](eval-datasets.md) retains original source URLs,
licenses, hashes and adapter constraints. [QASPER](evaluating-context.md),
[ConditionalQA](evaluating-conditionalqa.md) and [ContractNLI](evaluating-contractnli.md)
have tracked reproducible preparation scripts. Other datasets need a verified
adaptation to the evaluator input format.

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

## Plan an evaluation

Use [context evaluation](evaluating-context.md), [critic workflow](testing-usability.md),
[quality targets](rag-quality-targets.md) and the separate
[capacity protocol](testing-large-vaults.md). Preserve missing facts, failed tasks
and regressions; do not select only successful families or modes. Report added
model or agent input, calls, latency and known cost. Unavailable usage remains
unavailable. Scores apply only to the declared workflow, executable, corpus and
budgets.
