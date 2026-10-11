# Practical quality targets for retrieved context

Research reviewed 2026-10-03. This is a recommendation for a finite acceptance decision, not a record of a passing run or a replacement for the [frozen evaluation protocol](evaluating-context.md). No private acceptance questions or public test questions were inspected for this research.

**Use 90% complete tasks as the initial quality target, with 100% checked citation integrity and no correctness blockers. Keep the existing 18/20 unseen-task gate as a small pilot. Treat 95% as a stretch target, not a presumed ceiling, and stop optimizing when the declared gate passes.** Published research does not establish a universal 90–95% practical limit for RAG. Difficulty, corpus, context budget, task definition and judging method change the result substantially. These targets are product judgments for a local wiki, not scientific constants or industry certification.

## What the evidence supports

The relevant outcome for lwiki is whether its **final returned cited context contains every required proposition**, including qualifications, version constraints and supporting bridges. lwiki currently supplies context rather than generated answers. Good ranking, a correct source hit, or matching a reference paragraph is useful diagnostic evidence; none alone establishes whole-task success.

Google's *Sufficient Context* research separates retrieval insufficiency from generation failure and demonstrates that relevant context can still lack essential information. Its selective generation experiments report a 2–10% improvement in correctness among answered queries through confidence and sufficiency signals. That conditional improvement must be accompanied by the answer coverage/abstention rate. The original preprint's definition permits a plausible answer from the supplied context; lwiki's acceptance should additionally require the right entity/version and the critic's required propositions. [Original preprint, subsequently accepted at ICLR 2025](https://arxiv.org/html/2411.06037v1), [author research summary](https://research.google/blog/deeper-insights-into-retrieval-augmented-generation-the-role-of-sufficient-context/).

Ragas distinguishes reference-claim coverage from ranking precision and response faithfulness. Its context recall asks what fraction of reference claims the retrieved text supports; its context precision rewards relevant chunks near the top. Faithfulness asks whether emitted answer claims follow from context, so a faithful but incomplete answer can score well. These are different denominators, not interchangeable percentages. [Context recall](https://docs.ragas.io/en/latest/concepts/metrics/available_metrics/context_recall/), [context precision](https://docs.ragas.io/en/latest/concepts/metrics/available_metrics/context_precision/), [faithfulness](https://docs.ragas.io/en/latest/concepts/metrics/available_metrics/faithfulness/).

TRACe similarly separates relevance, utilization, adherence and completeness. Its completeness measures whether the generator uses relevant information **already in retrieved context**, so it does not detect every fact missing from retrieval. It is not the same as lwiki's complete-task metric. RAGBench evaluates evaluators as well as RAG outputs; its judge comparisons are not a production reliability target. [RAGBench original paper, sections 3.2 and 5](https://arxiv.org/html/2407.11005v1).

Useful benchmark results demonstrate why a universal ceiling is unsupported:

| Primary evidence | Reported result | Why it cannot set lwiki's threshold |
| --- | --- | --- |
| [Anthropic contextual retrieval study, 2024](https://www.anthropic.com/engineering/contextual-retrieval) | Its 1−recall@20 metric falls from 5.7% to 1.9% with contextual embeddings/BM25 and reranking. | Relevant-document recall exceeds 95%; this is not a whole-query success rate or all-proposition completion in a 1,500 estimated-token final context. It is a vendor's experiment, not independent qualification here. |
| [CRAG original benchmark, 2024](https://arxiv.org/html/2406.04744v1) | Straightforward RAG reaches 44% accuracy; studied industry systems reach about 63% perfect answers. | Dynamic web/KG questions and generated answers differ from a bounded local Markdown wiki. Historical results do not establish today's model ceiling. |
| [ALCE original benchmark, 2023](https://arxiv.org/pdf/2305.14627) | Best studied ELI5 systems lack complete citation support about half the time. | Long-form answer generation and semantic attribution differ from exact source-byte citations. A citation can be byte-valid while failing to support an answer. |
| [QASPER original paper, section 4.1](https://aclanthology.org/2021.naacl-main.365.pdf) | Estimated human lower bounds: 60.9 Answer-F1 and 71.6 Evidence-F1. | Reference disagreement and metric choices affect these scores; neither is a binary complete-task rate. The authors explicitly call them lower bounds, not human capability ceilings. |

NIST's voluntary AI RMF calls for criteria tied to deployment conditions, documented test sets, uncertainty and independent assessment. It does not prescribe a universal RAG percentage. Microsoft likewise separates retrieval, groundedness and response completeness; its configurable default 3/5 evaluator pass threshold is a framework setting, not evidence that 60% task completion is acceptable. [NIST AI RMF Core, Measure](https://airc.nist.gov/airmf-resources/airmf/5-sec-core/), [Microsoft RAG evaluator definitions](https://learn.microsoft.com/en-us/azure/foundry/concepts/evaluation-evaluators/rag-evaluators?view=foundry).

## Recommended current acceptance decision

Keep these gates preregistered before the private holdout is exposed. They are a defensible **HIGH pilot** standard for the declared corpus, workflow and budgets. Research supports the measurement choices; it cannot guarantee that the implementation can achieve the target.

| Dimension | Gate | Interpretation |
| --- | --- | --- |
| Complete answerable tasks | At least 18/20 | Every required proposition must be supported by final returned evidence. Partial credit remains diagnostic. Errors, invalid selections and timeouts receive zero completion credit. |
| Category floors | Exact ≥7/9; paraphrase ≥5/6; multisource ≥4/5; new documents ≥3/4 | Preserve the existing approximately 75% floors, with integer rounding. New-document cases may overlap the other categories; do not double-count them in the overall denominator. |
| Known absent information | All four remain unsupported | Check that no returned evidence is falsely credited with the missing fact. Neighboring passages are permitted. This does **not** establish product abstention or justify interpreting empty context as proof that the corpus has no answer. |
| Citation and operational correctness | Every emitted citation passes applicable revision/hash/span checks; no freshness, budget, isolation or accounting blockers | Zero tolerated observed failures in these checks. This is not a claim of zero future failure risk or of world-truth for every quoted source. |
| Independent critic | At least 9/10 under the existing published rubric, plus the gates above | Keep this expert rubric as a separate judgment. A 9/10 score is not 90% statistical reliability and cannot compensate for a failed completion/category/correctness gate. |
| Workflow identity | Separate deterministic and host-assisted results | A host selector's pass qualifies only that explicit workflow, including its additional input, invocation and latency. It cannot qualify the original one-command retrieval. |

Integrity can be an absolute acceptance condition because the checked properties are program invariants with direct verification. Semantic completeness remains empirical. Attribution frameworks such as AIS verify a claim against a provided source; source quality and factual truth need additional assessment. Correct citation bytes do not guarantee correct OCR, trustworthy authorship or a supported downstream inference. [AIS original paper](https://aclanthology.org/2023.cl-4.2/).

CRAG explicitly penalizes wrong answers more than missing answers, rather than rewarding a system that answers everything. For any future answer-generating workflow, preregister completeness, unsupported-answer rate and abstention coverage separately. Do not improve apparent accuracy by omitting hard queries from the denominator. Its human score assigns perfect/acceptable/missing/incorrect answers 1/0.5/0/−1; lwiki should retain its own stricter task-completion rule rather than importing that aggregate score. [CRAG metric definition](https://arxiv.org/html/2406.04744v1#S4).

## What 20 tasks can establish

An 18/20 result is 90% **observed** completion. Under independent Bernoulli sampling, its two-sided 95% Wilson interval is approximately **69.9–97.2%**. Thus the correct claim is “passed the preregistered HIGH pilot gate, completing 18 of 20 unseen answerable tasks”; “at least 90% reliable on future wiki questions” is unsupported. Even 20/20 gives approximately 83.9–100.0%. Wilson intervals are appropriate for small proportions; a normal/Wald interval behaves poorly near the boundary. [NIST proportion interval guidance](https://www.itl.nist.gov/div898/handbook/prc/section2/prc241.htm).

The following calculations are illustrative, using that same independent-trial assumption:

| Observed complete tasks | Observed rate | Two-sided 95% Wilson interval | Relative positive-run volume |
| --- | --- | --- | --- |
| 18/20 | 90% | 69.9–97.2% | 1× |
| 45/50 | 90% | 78.6–95.7% | 2.5× |
| 90/100 | 90% | 82.6–94.5% | 5× |
| 180/200 | 90% | 85.1–93.4% | 10× |
| 190/200 | 95% | 91.0–97.3% | 10× |

A stronger optional qualification could require a preregistered one-sided 95% exact lower bound above 90%; 190/200 meets that requirement at about 91.7%, while 95/100 narrowly does not at about 89.8%. Do not impose this much larger qualification retroactively on the current pilot. Four negative controls with zero failures have an exact one-sided 95% upper failure bound of roughly 52.7%; 20 have 13.9%, and 59 have 5.0%. Four controls are useful bug detectors, not proof of a low unsupported-answer rate.

Hand-authored stress questions are not a random deployment sample. Multiple questions over one document share failure modes; duplicate/paraphrase families also correlate. These binomial intervals therefore illustrate limited precision, rather than certify population reliability. For broader qualification, sample independent source/task families, report per-category denominators, and use source-family cluster resampling when estimating uncertainty. More questions on the same easy source cannot substitute for representative sources.

For a later bounded qualification, **100 answerable questions over at least 40 source families plus 20 scoped absent-information controls** is a reasonable cost/coverage compromise: ≥90/100 complete, ≥75% in each adequately represented major category, all negative controls correctly assessed and all integrity gates passing. This is still a scoped qualification, not a rare-error guarantee. At a true rate near 90%, approximately 139 independent tasks are needed for a nominal ±5-point normal planning margin, or 385 for ±3 points. Annotation and source-family coverage often matter more than nominal sample size.

## Trustworthy judging and version gates

Before running, reviewers freeze required propositions, acceptable evidence alternatives and answerability scope. Grade the actual final cited text without implementation explanations or retrieval-hit scores. For the small 24-case pilot, a second independent assessment of all cases is affordable and preferable; if bounded further, include every absent/ambiguous case and a stratified subset of positive cases. Retain both raw judgments, agreement counts, disagreements and evidence-based adjudication. Use agent reviews honestly as expert-agent assessments; they are not recruited-human usability results.

LLM judges have measurable error and bias. ALCE's automatic/human citation agreement is imperfect (κ≈0.698 for recall and 0.525 for precision), while MT-Bench's reported >80% preference agreement concerns open-ended assistant preferences, not wiki evidence completeness. These observations favor calibrated, proposition-level review, not treating a judge's single numeric score as ground truth. [ALCE human evaluation, section 6](https://arxiv.org/pdf/2305.14627), [MT-Bench original paper](https://arxiv.org/abs/2306.05685). ARES uses human annotations and prediction-powered inference to correct judge errors and estimate confidence intervals, showing why many automatic labels alone are not sufficient assurance. [ARES original paper](https://aclanthology.org/2024.naacl-long.20/).

LangSmith recommends starting with 10–20 carefully curated examples, separating development/testing splits, comparing versions and monitoring real usage afterward. That advice supports a small initial pilot, not a confidence claim from 20 cases. Its experiment comparisons expose individual regressions and latency rather than just averages. [Evaluation concepts](https://docs.langchain.com/langsmith/evaluation-concepts), [experiment comparisons](https://docs.langchain.com/langsmith/compare-experiment-results).

Freeze the binary, model/settings, corpus/revisions/caches, task transformation, budgets, final context limit, failure policy and judging rubric before acceptance. A case exposed to tuning becomes development data. Repeating a holdout until a stochastic selector happens to pass is also selection bias. Any reliability reruns must be declared before results are seen, with failures retained and question-level dependence recognized.

Report cached local retrieval, remote-query acquisition, host selection and end-to-end latency separately. For the host workflow, include preparation + selector + validation + orchestration, every failed call, median/tail latency, input bytes, available token usage and known cost. Unknown actual tokens or spend remain unavailable, not zero. Keep the existing finite authorization and hard bounds; set any additional user-facing latency requirement before acceptance rather than choosing it after results. Extra stages cannot win by hiding their cost or expanding final context.

## Finite stopping policy

1. Finish independent grading of the declared development run, localize failures and freeze a candidate only when the development evidence justifies acceptance. Development scores diagnose readiness; they cannot pass the unseen gate.
2. Run the preregistered private pilot once. If every gate passes, finish the quality milestone, preserve residual failures and scope the claim to the qualified workflow. Reaching 95% or 100% is optional follow-up work, not a reason to keep the completed milestone active.
3. If the pilot fails, retain the failed result and exact missing facts. Those questions become development data. Continue only with a concrete failure hypothesis and a bounded intervention; a fresh holdout is needed for another acceptance claim.
4. After two development interventions without clear whole-task improvement, or repeated regressions, follow the repository's fresh architecture-review rule. Stop parameter tuning; assess retrieval, evidence loss and packing together. A time box triggers a candid disposition, not automatic acceptance or waiver of an already authorized quality goal.
5. A 16–17/20 result may justify an explicitly limited exploratory pilot if that scope is separately agreed, but it does not meet the current HIGH gate. Broader benchmark work, rich-document support and tighter statistical confidence can be separate milestones. Do not silently lower the gate, narrow categories after failure or add enough attempts to manufacture a pass.

The adjacent rich-document handoff proposes 95% required-fact extraction and 90% end-to-end evidence completion. Those remain unagreed starting criteria. A five-fact task needs all five facts, so 95% atomic-fact accuracy does not imply 95% complete tasks; under an illustrative independence assumption it gives only 0.95⁵≈77.4% all-fact success. Extraction, citation integrity and retrieval completeness must retain separate denominators when that work begins.
