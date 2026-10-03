# End-user usability and live embedding validation

Detailed session reports, machine checks and `.artifacts` paths mentioned below are optional local evidence; fresh clones contain the curated summaries and tracked reproduction scripts. Recorded checks describe the identified historical source, not a new verification of the current checkout.

Run started 2026-10-02 local time (artifacts use UTC). Baseline clean `main` at `490b71a`. User authorized independent 6.1 Sol/Astra agents, research/downloads, a new local wiki, defect fixes/retests and live OpenAI embeddings. No generation API, user-vault mutation, host installation, publication or credential disclosure is part of this run.

**Completed locally.** The CLI and documentation defects below are fixed and retested. Retrieval remains useful for finding sources but is not yet dependable for automatically supplying complete answers: only 1 of 6 unseen answerable questions received fully sufficient semantic context. This report separates mechanical correctness from relevance quality.

## Useful artifacts and repeatable critic setup

- Local wiki overview (optional local `../../../.artifacts/ux-corpus/wiki/pages/overview.md`): five readable draft topic/overview pages covering CLI design/testing, documentation, Rust reliability and SQLite durability/backup.
- Corpus report (optional local `UX-CORPUS.md`), source manifest (optional local `../../../.artifacts/ux-corpus/manifest.json`), frozen 24 questions (optional local `../../../.artifacts/ux-corpus/queries.json`): 25 captured primary sources, originally 357,839 UTF-8 bytes, acquisition URLs/timestamps/notices and immutable revisions. One focused source was imported through an actual offline research handoff; repeat import reused it.
- [Critic workflow](../../testing-usability.md): reusable fresh-agent assignment, task matrix, scoring rules, authorization boundaries and fix/retest procedure.
- [Beginner tutorial](../../getting-started.md) and [complete embedding setup](../../providers.md): executable onboarding and configuration/recovery guidance.
- Detailed independent reports: Astra critic (optional local `UX-CRITIC.md`), docs walkthrough (optional local `UX-DOCS.md`), excerpt implementation (optional local `UX-EXCERPTS.md`), actual passage usefulness (optional local `UX-RETRIEVAL-CRITIC.md`).

## Evaluation design

The recent cleanup changed 148 files relative to `d8a82a5`, including storage, graph, retrieval, provider jobs, host research, CLI/docs and their tests. This run inspected the recent history/diff and current contract routes, then combined black-box user tasks, targeted implementation review and affected regression gates. It does not claim every line was independently audited or repeat every previously accepted native crash cut.

The rubric draws on [CLI Guidelines](https://clig.dev/), [Diátaxis quality](https://diataxis.fr/quality/), [Write the Docs principles](https://www.writethedocs.org/guide/writing/docs-principles/), and NN/g's [task success](https://www.nngroup.com/articles/success-rate-the-simplest-usability-metric/) and [severity](https://www.nngroup.com/articles/how-to-rate-the-severity-of-usability-problems/) guidance. Concrete successful tasks, recoverable failures, useful excerpts and runnable documentation are measured separately. These are agent expert walkthroughs, not recruited-human usability studies or standardized satisfaction scores.

The initial 24 queries were frozen before model search (SHA256 `b277c9840a01b03bfd1f11ece0205080a0a1be71692610f6287b9b5652638138`): 22 answerable and two absent facts. Search uses five results, 80 candidates and 512-byte excerpts; context uses five results, 80 candidates, 6,000 total bytes and 1,500 estimated tokens. Baseline context default was 240 excerpt bytes; the corrected default is 1024. All source citations are checked against original bytes and independently recomputed BLAKE3 quote hashes. Actual passage usefulness is separately rated usable/partial/not-useful against the requested facts.

Implementation was informed by this initial set, so it is an **in-sample regression comparison**. Separately frozen unseen questions assess new queries after code freeze on the same corpus; they do not establish new-corpus or human generalization.

An independent review caught a confound: live provider run/receipt records changed global FTS statistics while the original search run proceeded. Therefore original lexical ranking differences cannot be attributed to excerpt code. The authoritative old/new comparison uses both copied binaries offline on the same final wiki snapshot and cached embeddings. Original runs remain in `evaluation`; paired baseline in `evaluation-paired-baseline`; corrected results in `evaluation-after`, all under `.artifacts/ux-live`.

## Fixed issues

| Issue | Observed user impact | Change and retest |
| --- | --- | --- |
| Missing first-run fixture | README source add failed because notes.txt did not exist; searches used unrelated placeholders. | Self-contained deployment-note tutorial;23 runtime example commands replayed against old and updated binaries. |
| Missing/unreadable input reported INTERNAL | User error appeared to be an internal program defect with no recovery guidance. | USAGE names the input and suggests readable-file/stdin recovery; regression checks missing and directory inputs. |
| Hidden/nonexistent continuation cursor | Search said to use a cursor without showing one, including candidate truncation with no cursor. | Prints copyable cursor when present; otherwise explains candidate bound. Next page executed in regression. |
| Empty context lacked an explanation | No matches and no text fitting the budget were both blank stdout. | Distinct stderr guidance and omission reason/counts; exact budgeted stdout preserved. |
| Mutation output buried results | Initialization and source/page commands dumped internal plans as JSON. | Concise preview/prepared/committed summaries, IDs, paths, extraction state and retained changes; JSON envelope preserved. |
| Staged commands lost vault selection | Copying show/apply from outside the wiki failed. | Preserve explicit --wiki and shell-quote spaces/apostrophes; copied command executes successfully. |
| Terminal control injection in search summaries | Captured title/body ESC sequences could color or forge terminal output. | Escape controls in human summaries and newlines/tabs in titles; JSON remains escaped and read/context stays exact pipeable authority. |
| Guarded-edit guidance missing/overbroad | Human read hid author hash; generic conflict advice could suggest the wrong recovery workflow. | Hash on stderr, command-aware continuation, qualified reconciliation versus durable-conflict guidance. |
| Provider setup and cap errors opaque | 0644 token file and tiny response ceiling both became generic bounded-operation errors. | Closed safe local reasons explain private-file requirements and 8 MiB embedding reservation; no secret/provider text copied. |
| Failed probe remained Running | A returned failure could not be amended through jobs amend. | Pause the owned failed run after settlement handling; preserve all unknown holds; explain a new explicit probe creates a new run. |
| Preferences warning implied provider rejection | Valid explicit provider configuration still warned profile was not permitted. | Distinguish preference listing from separately checked provider authorization. |
| Reserved index.md rejection unexplained | Authoring a natural index page produced generic canonical-path error. | Explain generated index reservation and suggest pages/overview.md. |
| Verification ceiling gave no recovery advice | A 986-file accumulated-history wiki exceeded 16,384 logical directory/component checks. | Preserve the bounded refusal and add resource-specific hints; explicit 65,536-entry retry succeeds. Repeated path checks make this different from a unique-file count. |
| Correct source, wrong passage | Common query words exhausted the first64 match cap; lexical excerpts began at early words and semantic excerpts at unit starts, often returning headers. | Bounded query-aware window selection favors distinct/rare matching terms; semantic focus stays inside winning unit; context default1024 while explicit bounds win. |

Independent lifecycle checks also passed extraction/import/apply, explicit entity resolution, reviewed current graph evidence, idempotent imports, source refresh/withdrawal, historical labeling and preservation of conflicting human edits. These remain task evidence on disposable data, not real-host discovery proof.

## Live scope and accounting

The actual `https://api.openai.com/v1/embeddings` endpoint accepted `text-embedding-3-small`,1536 dimensions. A synthetic probe succeeded; initial public-source sync generated all 174 eligible inputs with 3,000-byte quality target and 8,000-byte hard input ceiling. The 24 initial query embeddings succeeded; context/hybrid and repeated queries then used retained vectors offline.

The user-supplied token file was 0644 and the CLI correctly refused its private-file contract. Its permissions/content were left unchanged. The runner read the token only into an ephemeral child-process environment and used a private configuration outside the wiki. Neither token contents nor authenticated headers appear in reports/argv. Network-blocked and pre-dispatch failures remain retained evidence; successful live calls used platform-approved network access.

Request/byte/deadline bounds were explicit. Corpus sync allowed at most 20 attempts and 1 MiB outgoing bytes; query operations allowed one attempt and 16 KiB outgoing bytes each. No complete trusted rate card was configured: **monetary cost remains unknown**, and conservative unknown reservations remain even when reported token usage exists. A successful response is not a zero-cost statement or billing reconciliation. The 40 validated live embedding receipts report 89,124 input tokens, including one probe, initial corpus sync, 32 query embeddings and the final two-input refresh. All 40 receipts remain unknown-reserved for billing. Exact evidence is in UX-checks.json (optional local `UX-checks.json`).

## Quality and practical limits

The controlled comparison used snapshot 87: all 72 top-five result lists (24 questions ×3 modes) were identical between old/new binaries. Source hit@5 was 22/22 for each mode. Actual answer-bearing context changed as follows; each cell is usable / partial / not useful across 22 answerable questions:

| Mode | Old context | Corrected context |
| --- | ---: | ---: |
| Lexical |1 /6 /15|11 /9 /2|
| Semantic |0 /9 /13|11 /10 /1|
| Hybrid |0 /9 /13|11 /10 /1|

All 853 independently reviewed paired passage spans match original bytes. More complete excerpts consume more budget: mean context source recall fell from 1.000 to .9091 lexical and .98485 to .93939 semantic. This is an in-sample improvement in passage usefulness, with no ranking gain.

**Unseen-query result is substantially weaker.** Eight questions were frozen after retrieval implementation freeze (SHA256 `130c010742ae6702e6dc8e92339b0348def0b0132c125b71f82aba917dd0e7a5`); six are answerable. Semantic and hybrid source hit@1 and source recall@5 were 100%, while lexical hit@1 was 5/6. Independent semantic-context ratings were **1 usable / 3 partial / 2 not useful**. All 40 heldout context citations were independently byte/hash checked. The two absent questions returned neighbors that do not support the requested facts; the CLI does not generate an answer or implement an answer-level abstention classifier.

The original sixth heldout context failed the default proof-entry budget after provider history accumulated. That original result is preserved; final heldout runs explicitly used 65,536 verification entries, with all output limits unchanged and no ranking/excerpt tuning. Two missing query vectors were completed separately; a subset-summary runner divided by zero because both were absent questions, after both calls/context outputs had succeeded. The corrected full 8-question offline replay passed. This is a changed verification protocol, not an all-default success.

For the two unhelpful questions, one selected embedding units that did not contain the requested panic/abort explanation; another contained the requested ignored-test commands but the overlap heuristic selected the wrong 512-byte subwindow. No citation/containment defect was found. Full cited-source reads recover both answers, but automatic passage completeness is an open quality weakness. Source packing, unit selection, per-source window allocation and wider-corpus/human evaluation merit a separate improvement cycle; no universal answer-quality claim is made.

Remaining limitations include multi-facet questions whose needed text spans several windows/documents, source diversity under small total budgets, and model rankings that select adjacent but incomplete explanations. Increase context bounds, narrow a query or read the cited source when important facts are omitted. Neither nearest-neighbor matches nor current citation labels establish that an absent fact is known. The CLI retrieves evidence; it does not generate an answer to these questions.

The ordinary human `check`/graph output still contains structured JSON, and raw read/context text deliberately preserves source bytes. Cold remote query latency includes provider, local durable accounting and index work; it is not a pure model latency benchmark. Broad-corpus retrieval quality, human usability, other models/gateways, generation compatibility, GUI/host discovery, other native recovery platforms and power-loss behavior remain separately unqualified.


## Final local verification and retained deliverables

- Aggregate 24 Bazel targets pass, covering 284 Rust parent cases plus format and strict Clippy. The initial run passed 22 targets and failed two newly maintained fixtures: generated skill manifest and default-excerpt page fixture. Continuations corrected the manifest and made the page a valid reviewed canonical page, then passed. Prior compile-import/style and missing-directory fixture failures remain in the logs. No failing whole invocation is relabeled successful. Unaffected earlier integration results are reused; the final hint-only change was checked by context freshness/CLI tests and format/Clippy rather than repeating the whole matrix.
- Four separately executed unit cases pass: three dense-unit span containment cases and one provider diagnostic redaction case. All 41 Python build-tool tests pass. Existing native recovery cases in the routine regression gate pass; the entire historical native fault matrix was not rerun.
- Fresh optimized macOS ARM64 candidate: lwiki (optional local `../../../.artifacts/ux-final-candidate/lwiki`), SHA256 `3c03dbd9cbb03d3dd0655be014044b0cbedaf587956198683f60569c912251e6`. Built from the reviewed dirty working tree at baseline HEAD 490b71a; 326 frozen product/test/skill/tool files remained unchanged through final candidate verification. This is a local candidate, not a published release or clean-checkout provenance claim.
- The copied optimized binary passes 23 README/tutorial commands in paths containing spaces and explicit provider-budget/credential-failure recovery probes. An initial final-probe harness reused an existing fixture directory and was rerun with a new directory; the CLI correctly refused reinitialization. The exported command reference exactly matches the maintained reference and regenerated manifest; 65 relative documentation links/anchors passed review.
- The corpus license correction uses `source refresh`: the old revision is byte-preserved, a new MIT-corrected revision is current, and one authored page update uses an observed-hash guard. Full-byte/mtime dry-run checks, ordinary full-vault backup/restore including `.wiki`, restored index rebuild and current/historical retrieval pass. This is quiescent copy/recovery evidence, not power-loss qualification.
- Refresh reduced embedding coverage 174→172; one bounded live request regenerated 2 inputs, reused 172, and restored 174/174. The next sync generated 0, used no network, and current semantic citations excluded the old revision. Final canonical check reports 0 errors. Original frozen manifest/labels remain unchanged; current manifest (optional local `../../../.artifacts/ux-live/corpus-lifecycle/manifest-current.json`) records the correction.
- Complete wiki archive (optional local `../../../.artifacts/ux-corpus-wiki.tar.gz`) retains canonical and hidden operational/cache state. The token scan found no exact credential bytes in validation artifacts; original token file unchanged. Machine-readable evidence (optional local `UX-checks.json`) records logs, counts, hashes and limits.

To explore the retained wiki from the repository root:

```sh
.artifacts/ux-final-candidate/lwiki --wiki .artifacts/ux-corpus/wiki --offline search 'SQLite backup'
.artifacts/ux-final-candidate/lwiki --wiki .artifacts/ux-corpus/wiki --offline context 'SQLite backup' --verification-max-entries 65536
```

All 32 benchmark/heldout query vectors are cached; new semantic questions require explicit provider authorization. Read the actual passages and full captured sources for omitted facts. No commit, push, installation or publication was performed.
