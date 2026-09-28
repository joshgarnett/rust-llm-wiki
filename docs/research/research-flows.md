# Cost-controlled research and durable results

Research date: 2026-09-28. This combines primary-source findings with an original proposed workflow. It does not report measurements of our own software.

## What to learn from research agents

Anthropic's research architecture uses a lead agent and specialized workers. Its published observations put multi-agent token use at roughly 15 times chat usage in that system, and emphasize bounded effort, distinct assignments, checkpoints, and evaluation. That ratio is contextual, not a pricing multiplier for our product. The useful lesson is that parallelism buys breadth and latency at a resource cost; uncontrolled delegation is not a cost strategy. [Anthropic engineering report](https://www.anthropic.com/engineering/multi-agent-research-system)

Open Deep Research supports configurable model roles and search tools and publishes an evaluation workflow. Its repository was archived on August 21, 2026, so it should be treated as a reference implementation rather than an assumed maintained dependency. GPT Researcher documents web and local research, report generation, provider configuration, and MCP. Its supported installation includes Python 3.12+ and package dependencies; its server UI is optional to its Python package use. Neither is the proposed Rust core. [Open Deep Research](https://github.com/langchain-ai/open_deep_research), [GPT Researcher](https://github.com/assafelovic/gpt-researcher)

Provider cost optimizations are secondary to avoiding unnecessary work. OpenAI documents an asynchronous Batch API with a 50% discount and a 24-hour completion window; this fits independent bulk embedding/extraction tasks better than an interactive query loop. Prompt caching can reuse stable prefixes, but accounting varies by model and can include cache-write charges. Use provider-reported usage and model-specific rates rather than assuming every repeated prefix saves money. [Batch API](https://developers.openai.com/api/docs/guides/batch), [prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching)

## Proposed two-mode workflow

**Agent-driven mode comes first.** Codex, Claude Code, or Cursor uses its existing browsing and synthesis tools. The CLI supplies plans, existing evidence, source capture, deduplication, bounded entity/relationship extraction packets, run records, validation, and changesets. The host performs extraction; validated entities/assertions/evidence become linked Markdown records. This supports research without requiring us to recreate a full agent harness.

**Standalone mode follows.** The CLI runs a bounded state machine using configured search and generation providers. Embeddings remain a separate capability. A generic HTTP embeddings endpoint cannot search the web or author a report. Research should work from explicit URLs without a search subscription; broader discovery needs a search provider or an external agent.

For both modes, preserve these stages:

```text
question + constraints
  -> inspect existing knowledge and unresolved gaps
  -> plan a bounded search frontier
  -> discover and capture sources
  -> deduplicate and extract supported evidence
  -> identify contradictions and remaining gaps
  -> synthesize a report and proposed page changes
  -> validate citations and expected file versions
  -> apply when requested, then refresh local indexes
```

Research can stop successfully with unanswered questions. Exhausting the budget should save partial evidence, not produce unsupported completion claims. Initial jobs stop after configured search/fetch/round/time limits, sufficient question coverage, or a configurable number of rounds without new evidence. Those thresholds are product tuning choices, not research laws.

## Spend the expensive work selectively

| Stage | Preferred initial behavior | Escalation condition |
|---|---|---|
| Existing knowledge | Local lexical/graph retrieval and explicit context budget | Known paraphrase miss permits semantic search |
| URL filtering | URL/host rules, byte limits, normalized hash deduplication | Ambiguous relevance merits a small model call |
| Format extraction | Deterministic normalization and cited passages | Unsupported format or unclear structure requires explicit additional capability |
| Entity/relationship extraction | Explicit bounded agent/API pass over new or changed inputs, saved as Markdown | Ambiguity or contradictions may merit another budgeted pass |
| Evidence review | Check source diversity, dates, citations, and contradictions | Conflicting interpretations merit a stronger model |
| Synthesis | Reuse source cards and changed evidence only | Complex comparison needs broader context |
| Maintenance | Dependency-based invalidation | Substantive changed evidence merits resynthesis |

A low-cost model is not automatically suitable for evidence extraction: a missed qualifier can poison durable knowledge. Evaluate extraction accuracy and citations for the chosen model role. Deterministic URL deduplication also needs care: retain meaningful query parameters, canonical and observed URLs, redirects, and content hashes. Ten mirrored pages should not count as ten independent sources.

## Enforceable budget and resume semantics

Record the scope/checkpoint in `run.md`, durable events/receipts in event notes, and source revisions, stage outputs, and extracted relationships in linked Markdown under each run ID. Use deterministic task keys and preserved completed results so resuming reuses completed work. Operational locking/pending-I/O journals may remain non-Markdown outside the search cache. A Markdown-only export cannot reconstruct an unrecorded charge. A cached claim still identifies the source revision and prompt/model version that produced it.

In standalone mode, atomically reserve a conservative per-call allowance before dispatch, across all workers. Include outstanding calls, retry allowances, search fees, input/output limits, and cached-input/write pricing where relevant. Settle confirmed receipts afterward. A timeout can mean unknown billing, so it does not automatically refund a reservation. Use integer monetary units, rate-card versions, and explicit currency.

If rates or billable token bounds are unknown, report estimated/unknown cost and enforce request, byte, token, concurrency, and elapsed-time limits. Do not advertise an absolute dollar guarantee based on an approximate tokenizer or an uncontrolled external service.

**External agent budgets are advisory unless their work passes through the CLI's dispatcher.** The CLI cannot cap a Codex/Claude/Cursor subscription or observe every independent tool call. Import observed usage when available and mark missing spend unknown. The skill can reduce waste, but cannot turn an unenforced instruction into a hard spending limit.

Proposed controls include `research plan`, `research run`, `research resume`, `research status`, and `research report`, with `--max-requests`, `--max-fetches`, `--max-rounds`, `--max-duration`, and rate-backed `--max-cost`. Scheduling can initially use existing OS facilities or agent automation; a resident daemon is unnecessary.

## Write useful knowledge back

Keep fetched sources, a research report, and proposed wiki changes as distinct artifacts. Each factual finding identifies its evidence. Separate sourced observations, model inference, and unresolved disagreements. Default research output is a recoverable draft; an explicit apply mode can update the wiki within the caller's authorization without repeated prompts.

Validate that cited revisions exist and cited passages match. This proves referential integrity, not logical entailment; evaluate whether the passage supports the claim separately. On source revision/retraction, mark affected pages or claims for review before normal retrieval presents them as current support. Preserve previous conclusions as historical knowledge where appropriate.

An imported report from another AI is a useful lead, not independent corroboration of every linked claim. Store the original report and author/tool/date, resolve its URLs, capture underlying evidence when possible, and distinguish checked references from unavailable ones.

## What to measure

Track cost per accepted supported finding, cost per completed question, duplicate-fetch rate, cache reuse, evidence coverage, citation support, human correction effort, and total lifecycle cost. Measure initial compilation, incremental repair, and query costs independently. Evaluate the same question with a simple single-agent baseline before deciding that more workers are economical.

For imported web content, treat instructions as untrusted data. Keep source text out of executable config, isolate credentials from prompts, and bound fetched bytes/redirects. These are necessary consequences of an automated fetch-and-write workflow, not a request to build an enterprise security platform.
