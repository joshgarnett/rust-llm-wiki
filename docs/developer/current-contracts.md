# Current contracts

Use this map to find the contract and regression tests for a behavior. The
running binary's `capabilities`, `schema NAME` and command help give exact syntax.
See [validation and limits](validation-status.md) when choosing checks.

## Select, capture, edit and resume

| Surface | Contract and user guide | Representative tests |
| --- | --- | --- |
| Wiki selection | Explicit root, nearest ancestor, then identity-pinned default; no invalid-target fallback. [Selection](../user/everyday-use.md#choose-the-wiki), [CLI](contracts/cli-and-skills.md) | `src/config/local.rs`, `src/vault/discovery.rs`, offline CLI/config tests |
| Initialization | Intentional `--existing`, all reserved names absent, collision preflight before writes, authored landing preserved by sync. [Getting started](../user/getting-started.md) | `src/app/init_tests.rs`, `tests/offline_application.rs` |
| Preferences | One bounded private snapshot; TOML schema3 settings, legacy JSON read compatibility and guarded explicit migration with a backup. Preferences cannot grant provider authority. [CLI](contracts/cli-and-skills.md) | `src/config/local.rs`, `src/config/local_io.rs` |
| Literal inspection/edit | Physical whole-file byte coordinates and hash; guarded existing-note change with exact retained before/proposed bytes. Valid Page edits preserve IDs; immutable/control targets refuse. [Editing](../user/everyday-use.md#inspect-and-edit-exact-bytes) | `src/app/literal.rs`, changes lifecycle/recovery tests |
| Sources | Explicit capture retains original input; path refresh binds unique Source and one expected-hash input observation; old revisions stay immutable. [Storage](contracts/storage.md), [tutorial](../user/everyday-use.md#capture-and-refresh-notes) | `src/sources/path_refresh.rs`, `tests/sources_evidence.rs`, Source refresh/import tests |
| Pages | Named Page init allocates an ID once; existing destinations refuse. Guarded put/batch and moves preserve identity and provenance. [Reorganization](../user/everyday-use.md#create-edit-and-move-a-page) | Page citations/rename/lifecycle tests, `tests/offline_application.rs` |
| Health | Strict check remains nonzero for structured damage. Selected healthy evidence and physical inspection have separate scopes; contained observation must disclose exclusions/issues. [Recovery](../user/recovery.md) | check/catalog/CLI corruption and observation tests |
| Machine output | Output envelope v2 with selection origins; v1 remains a published older schema. Raw text stays clean on stdout and scope diagnostics go to stderr. [CLI output](contracts/cli-and-skills.md#structured-output) | `tests/machine_contract.rs`, `tests/offline_cli.rs` |

## Retrieval and authority

| Area | Implemented behavior | Contract and checks |
| --- | --- | --- |
| Literal/lexical retrieval | Bounded original-text discovery and cited context; excerpts may omit required facts. Ordinary legacy local lookup verifies files through a bounded in-memory view without a writer permit; explicit cached lookup still uses SQLite coordination files. Normalized discovery uses its published catalog. | [Indexed context](../user/retrieval.md#search-and-inspect-cited-context), [retrieval](contracts/retrieval.md); retrieval/context suites |
| Selected verification | Authenticate selected canonical records, revision ownership, heads, exact bytes/spans and final dependencies. A discovery generation does not establish global corpus currentness or completeness. | [Query limits](query-limits.md), selected identity/freshness tests |
| Historical evidence | Explicit history retains exact revision identity and Historical/Withdrawn labels. Old citation integrity does not make old facts Current. | Source evidence/currentness and context tests |
| Semantic/hybrid retrieval | Remote prepared compatible vectors, local caches and exact cached offline query vectors. Normalized queries still scan compatible vectors; full scale is unqualified. | [Embeddings](embeddings.md), embeddings/provider/retrieval tests |
| Host selection | Fingerprint-bound candidate selection and exact complete-original range replay are distinct workflows with additional host input/cost. | [Context selection](../user/retrieval.md#select-passages-with-a-host), [cited Page recipe](../../skills/llm-wiki/references/cited-page.md), selection/reading tests |
| Graph | Explicit extraction, identity resolution and evidence review; contradictions remain visible. A graph path does not prove a novel proposition. | [Knowledge graph](knowledge-graph.md), [record schemas](contracts/record-schemas.md), graph/review/decision tests |
| Source relations | Explicit experimental generation/preparation, immutable derived records and offline reconstruction; disabled unless requested. | [Experimental guide](../user/sources-and-providers.md#experimental-source-relations), Source relation tests |

## Durable work and recovery

Canonical knowledge stays in Markdown. Indexes are rebuildable projections; vectors,
private credentials and uncertain billing cannot be recreated from lost authority.
Schema2 relocates operational history under `.wiki/retained`; Source revisions and
ordinary record schemas keep their existing identities.

Changes bind expected author bytes, read preconditions, normalized operation authority
and catalog publication. Preparation alone never authorizes replay. Journals retain
exact before/proposed payloads and conflict/recovery state; apply uses recoverable
sequential file writes rather than a multi-file filesystem transaction. Preserve
unknown edits, incomplete retained evidence and unknown accounting holds.

Collections use explicit manifests and bounded resumable group publication with
individual Source/Revision identity. Batch refresh retains per-Source author/head/input
guards. See [imports](../user/sources-and-providers.md#import-a-collection), [batch refresh](../user/sources-and-providers.md#refresh-a-guarded-batch)
and [storage](contracts/storage.md); importer, normalized lifecycle and native fault
suites cover their separate mechanics.

Dispatcher-managed providers enforce finite admission and retain uncertainty across
retries. Host research instead owns external tools and unobserved host usage; lwiki
retains local packets and validates submitted citations. See [providers/jobs](contracts/providers-jobs.md)
and [research handoffs](contracts/research-handoffs.md). Mocks prove these mechanics,
not live compatibility or billing. [External qualification](qualification.md) is separate.

## Architecture and acceptance

[Design decisions](design-decisions.md), [large-vault design](large-vault-design.md)
and [capacity protocol](testing-large-vaults.md) retain architecture rationale and
prospective resource gates. [Context evaluation](evaluating-context.md) and the
[critic workflow](testing-usability.md) separate task completion, citation integrity,
relevance and completeness. Passing component tests or a scoped host workflow does
not establish default retrieval quality, native crash safety or a released product.
