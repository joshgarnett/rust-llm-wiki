# Research and planning progress

Started: 2026-09-28. Status: autonomous M0–M4 implementation started; P00 accepted; P01/P02 next.

## Scope

Design a self-contained Rust CLI for a local, human-readable LLM wiki, with useful human and agent interfaces, lexical and graph retrieval, optional semantic retrieval, cost-controlled research, and a portable usage skill.

## Work tracking

- [x] Inspect repository and local instructions: empty repository; no existing implementation or applicable AGENTS.md found.
- [x] Read OpenAI documentation and skill-creation guidance for the integration plan.
- [x] Start primary-source research on the two named projects and the LLM Wiki concept.
- [x] Ask optional product questions about research execution and semantic-search dependencies.
- [x] Delegate focused research after explicit user authorization.
- [x] Comparable wiki/search systems — `wiki_landscape` agent; `docs/research/landscape.md`.
- [x] Retrieval, knowledge graphs, and evaluation — `retrieval_research` agent; `docs/research/retrieval.md`.
- [x] Rust, embedded storage, remote embeddings, and packaging — `rust_feasibility` agent; `docs/research/rust-stack.md`.
- [x] Research execution, costs, and agent integration — primary agent.
- [x] Synthesize architecture and command contracts.
- [x] Define phased implementation plan and acceptance gates.
- [x] Write independent research prompts for optional external review.
- [x] Review cross-document consistency, evidence, and local links.
- [x] Promote LightRAG-inspired entity/relationship extraction into the planned release scope and document the graph lifecycle.
- [x] Research official Obsidian behavior and the Agent Skills format; draft the Markdown wiki specification.
- [x] Define heading/metadata repair and document-first embedding/segmentation behavior.
- [x] Check the revised architecture, roadmap, research guidance, and skill plan for consistency.

## Technical design work

- [x] Commit the research and initial architecture: `4652248` (`docs: capture wiki research and initial architecture`).
- [x] Storage, Markdown schema, index generations, and recovery — `storage_design`.
- [x] Retrieval, extraction packets, entity resolution, and evidence — `retrieval_design`.
- [x] Provider adapters, dynamic credentials, jobs, and budgets — `wiki_landscape`, acting as provider designer.
- [x] CLI/skill contracts, module boundaries, and implementation handoff — primary agent.
- [x] Reconcile shared types, lifecycle states, error contracts, and milestone dependencies.
- [x] Review designs independently and validate document examples and links.

Seven technical design documents now define schemas, pseudocode, interfaces, state transitions, and acceptance matrices. Start with the [technical overview](docs/technical/README.md) and [implementation handoff](docs/technical/implementation-handoff.md). The design work and associated baseline clarifications remain uncommitted for review; `4652248` preserves the requested pre-design checkpoint. No Rust implementation was created in this step.

## Confirmed user decisions

- Semantic search uses a configurable `/v1/embeddings`-compatible HTTP API; local embedding models are out of scope.
- Embedding configuration must support a static key or a script that supplies dynamic credentials, a full endpoint URL, a model, and necessary request/runtime options.
- Include a LightRAG-inspired extracted entity/relationship graph as a planned product capability, alongside page links and provenance.
- Put as much durable state as practical in Markdown, with Obsidian-compatible links and an Agent Skills-like frontmatter/body contract. Recovery is best effort; not every index/runtime value must live in Markdown.
- Account for users changing headings or metadata; avoid unnecessary chunking and evaluate where segmentation is useful.
- Use sub-agents where useful and track progress. Specialists contributed the graph design, Obsidian research, and independent consistency review.
- Implementation orchestration uses `gpt-6-sol`, delegating to Sol and Astra as needed. Keep contexts focused and progress durable; choose safe defaults and record/defer missing external inputs instead of routine user questions.

## Autonomous execution plan

- [x] Write a short root `AGENTS.md` router and an [execution playbook](docs/execution/README.md).
- [x] Delegate the 22-package M0–M4 backlog to `gpt-6-sol`; define dependencies, owned paths, focused reads, tests, review models, and completion criteria.
- [x] Delegate independent autonomy/context/blocker review to `gpt-6-astra`; incorporate dirty-baseline preservation and core-gate versus external-qualification distinctions.
- [x] Define shared-file ownership, bounded task prompts, checkpoints/resume, model/runtime fallbacks, and local-only commit policy.
- [x] Define reversible defaults, material decision records, external deferrals, and conditions for stopping incomplete work.
- [x] Specify validation/evidence gates, including concrete production research output schemas and rejection tests.
- [x] Save a [ready-to-paste new-session goal](docs/execution/START.md).
- [x] Validate 29 Markdown documents, 176 local links, all 22 package IDs/required fields, and the dependency closure through P21; document syntax and whitespace checks pass.

No implementation goal was started, no runtime/model configuration changed, and no Rust code was created. All implementation gates remain pending. The execution plan is saved in the workspace alongside the uncommitted technical design.

## Working assumptions

These are proposals, pending discussion:

- Markdown and captured source files preserve durable knowledge; SQLite lexical and graph indexes are rebuildable. Missing vectors may require separately requested re-embedding.
- The default binary works without a model, daemon, database server, or API key.
- Existing agents can use the CLI; a standalone research runner is a separately scoped capability.
- The first agent-usable release includes extraction through the host agent and graph storage/querying; a configured generation API adds direct extraction in the CLI.
- Semantic search is an optional remote capability. Lexical lookup and traversal of an existing graph remain offline; semantic graph seeding and automatic extraction can require remote calls.
- Start with whole short-note embeddings and document-level text search. Split long model inputs or introduce smaller retrieval units when limits/quality justify it; no user-managed chunk files.

## Decisions still open

- Priority of agent-driven versus standalone automated research.
- Final binary name, initial target platforms, and representative evaluation corpus.

## Findings incorporated

- Separate durable sources, maintained wiki pages, research records, and disposable search projections.
- Include explicit links, provenance, and extracted entity/relationship graphs. Evaluate extraction quality and query costs as release gates; community summaries remain a later extension.
- Keep embedding HTTP transport separate from local vector storage; use exact vector search as the initial baseline.
- Do not equate wiki compilation, multi-agent research, or prompt caching with automatic savings.
- Enforce resource limits in the CLI-owned dispatcher; label budgets for external-agent work advisory.
- A filesystem changeset and SQLite transaction are not jointly atomic; recovery and external-edit conflicts need explicit contracts.
- Current libraries and projects have version-dependent capabilities and licensing; pin and recheck before implementation reuse.

## Verification boundary

Research uses current project documentation and papers. No project has been benchmarked locally, no provider calls have been purchased, and no cited performance claim should be read as a measurement of this proposed CLI.

## Review and validation completed

- Three sub-agents produced separate research briefs; two then reviewed the combined design independently.
- Incorporated review fixes for source withdrawal, snapshot tampering versus refresh, URL capture prerequisites, endpoint-bound credentials, external-editor races, embedding reuse across parser upgrades, and evidence freshness.
- Checked all 14 Markdown files for balanced code fences, opaque citation IDs, trailing whitespace, and valid local links (34 references outside example fences).
- Parsed both TOML configuration examples, the JSON credential example, and six YAML/frontmatter examples successfully using installed `tomli` and Ruby Psych.
- Reviewed the revised format/graph design independently. Added dependency/status verification, actual pending-change payload preservation, and precise UTF-8 span/hash rules. Obsidian behavior is documented from official sources; live compatibility tests remain an implementation gate.
- All command examples are labeled proposed. No runnable CLI or discoverable usage skill is claimed; the skill ships with tested commands in M2.
- Three technical designers completed their assigned areas and independently reviewed integration. Resolved direct source citations without graph assertions, intermediate apply steps, exact evidence revalidation, complete evidence review, durable pending packets, entity identity versus description eligibility, and repeated import idempotency.
- Selected a bounded Rust exact vector scan as the M3 baseline. Added retained active-space configuration checks so new-model queries cannot be compared with old-model vectors.
- Checked all 21 Markdown documents and their local links; parsed four JSON, four TOML, and six YAML/frontmatter examples. Executed the illustrative SQL schema in a temporary in-memory SQLite database. These checks validate documentation syntax only, not the future implementation.

## Next step

Start a new `gpt-6-sol` session with the [implementation goal](docs/execution/START.md). It begins at P00 in the [work packages](docs/execution/WORK-PACKAGES.md), preserves the existing planning baseline, and proceeds through M4 using [STATE.md](docs/execution/STATE.md) and the validation/decision records. The [format](docs/wiki-format.md), [graph design](docs/knowledge-graph.md), [milestones](docs/implementation-plan.md), and [embedding contract](docs/embeddings.md) remain the product requirements baseline; the technical documents resolve implementation details.

## Authorized implementation run

- Planning edits preserved in local commit `a97679c`; branch `impl/autonomous-v1`.
- Rust/Cargo 1.98.0 verified and pinned, Rust 2024 edition, lockfile generated.
- P00 shared types and 13 canonical record schemas implemented; thin CLI currently supports `capabilities` and `schema`.
- Disposable deterministic bootstrap vault generated with homonyms, Unicode, immutable revisions and exact evidence.
- Root bootstrap checks: ten all-targets tests, lint, format, build and fixture reproducibility passed; three independent Astra findings corrected and re-reviewed. P00 accepted.
- All later packages and V01–V17 integrated completion remain pending; see [checkpoint](docs/execution/STATE.md) and [coverage](docs/execution/reports/coverage.md).
- External provider/host/platform/release qualification E01–E05 remains unrun; no live spending, real-vault edits or publishing.
