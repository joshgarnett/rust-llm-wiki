# Documentation usability validation

Date: 2026-10-02. Baseline: `main` at `490b71a`. Worker lease: `README.md`, new `docs/getting-started.md`, this report, and `.artifacts/docs-usability/**`. No product, build, schema, execution state or Git mutations by this worker.

## Evaluation method and sources

This is a task-driven expert walkthrough and executable-example check, not a recruited-user study. The beginner persona has a macOS/Linux shell, a checked-out repository and a current binary, but no provider credentials or previous vault. Completion requires a known useful result without inventing input files, IDs, credentials or API responses.

- [Command Line Interface Guidelines](https://clig.dev/#help) recommends discoverable help, common examples and next actions; its [documentation guidance](https://clig.dev/#documentation) distinguishes immediate command help from fuller documentation.
- [Diátaxis tutorials](https://diataxis.fr/tutorials/) motivates a concrete learning journey with observable results and predictable expectations. [Tutorials versus how-to guides](https://diataxis.fr/tutorials-how-to/) distinguishes beginner teaching from a competent user's task instructions.
- [Write the Docs: How to write software documentation](https://www.writethedocs.org/guide/writing/beginners-guide-to-docs/) calls for a clear problem statement, a small common-use example, installation guidance and links for support.

Sources were searched and fetched through the web tool on 2026-10-02. These principles inform the rubric below; the pass/fail judgments are repository-specific observations, not standardized source scores.

## Rubric and findings

| Criterion / beginner task | Baseline observation | Resolution and evidence |
| --- | --- | --- |
| Understand a useful first outcome | README led with qualification history, then architecture and builds. | README now leads with capturing deployment notes, finding a command and supplying cited context. Historical evidence remains below the first workflow. |
| Know prerequisites and shell context | Build prerequisites linked, but runtime examples omitted explicit shell/root context and supported-write platform. | Tutorial specifies repository-root macOS/Linux shell, retained variables, absolute binary path, new candidate directory and Windows limitation. Build commands remain unexecuted in this pass. |
| Capture a supplied input | README used nonexistent `./notes.txt`. Actual command failed exit 1: `INTERNAL: open input: No such file or directory (os error 2)`. | Both docs create `deployment-notes.md` before capture. Original failure retained in `baseline-source.json`; error classification reported to root for a product fix. |
| Get a meaningful search hit | README searched unrelated placeholders `identifier` and `question`. | Exact fixture command `atlas_migrate` and lexical terms `Atlas deployment` produce current source hits. Expected excerpt documented and asserted. |
| Interpret bounded context | Example provided bounds without expected output or answer distinction. | Tutorial explains source/revision IDs, byte spans, quote hash, estimated tokens, omissions, and retrieval versus generation. Context output contains the expected note and Current citation. |
| Carry dynamic identity into follow-up tasks | No beginner method for preserving capture IDs. | Capture uses `--json` deliberately; IDs live under stable envelope `data.allocated_ids`. Explicit manual placeholders for read/refresh must be replaced from actual results. Harness substitutes those real IDs, never fixed example IDs. |
| Maintain a practical wiki | README routed directly to dense agent workflow prose. | New tutorial creates a readable runbook draft, reads it by path, searches only pages, refreshes source instructions, and shows retained historical text versus an unchanged authored summary. |
| Distinguish local and remote behavior | Existing distinction was accurate but surrounded by operational history. | Offline tutorial remains credential-free; links route semantic embeddings/API setup and research handoffs separately. It does not promise graph acceptance or model answers from capture. |
| Recover from ordinary beginner mistakes | No consolidated first-use troubleshooting. | Task-oriented table covers existing init directory, missing file, wiki selection, no hits, missing embeddings and author conflict. Advanced conflict/cleanup guidance stays linked. |
| Find help matching the binary | Capabilities/skill links existed but lacked a beginner sequence. | Tutorial exercises version/help/subcommand help, exports Codex skill to a fresh directory and explains that export does not install or launch the host. |
| Navigate maintained docs | No dedicated beginner tutorial. | README routes to new tutorial before contracts/plans/research; all 41 local links across both changed docs resolve. Historical release guides remain explicitly labeled. |

## Actual execution and retest

Qualified copied binary: `.artifacts/cleanup-qualification-final/lwiki`, SHA-256 `21797e63d6e8a8d9bb58382678769da6221f2cde78f6829e29388800be9e5c57`. `target/debug/lwiki` was not used. The binary reports `lwiki 0.1.2`; the hash identifies cleanup behavior beyond the historical 0.1.2 release.

`python3 .artifacts/docs-usability/run_examples.py json-capture` passed the final current-document examples: README one runtime shell block / five CLI commands; tutorial nine runtime shell blocks / eighteen CLI commands. Build blocks are deliberately skipped because this worker has no build lease. Runtime examples are extracted from the actual Markdown. Binary and temporary-parent assignments are replaced with the qualified copied binary and per-case directories under the leased artifacts path; manually copied IDs are bound from the preceding capture output. Input bodies, options and command order execute as written. Paths include spaces and the shell runs with `-e`. Every command except help/version carries `--offline`; no provider configuration or credentials are supplied.

Assertions cover all command exit codes, known literal/lexical hits, Current context citation and fixture text, zero-error checks, draft runbook persistence, old runbook text after source refresh, historical labels and refreshed text, and exported `SKILL.md`. Each command's arguments/stdout/stderr/exit code is retained in per-case `commands.jsonl`; shell blocks and outputs are retained separately. `examples-json-capture.json` summarizes the final run. `links.json` records local link checks.

The first harness attempt failed when its ID-binding selector included `source add --help` rather than only the actual capture. That was a harness JSON-decoding error, not a documentation or CLI defect. The selector was corrected; `examples-corrected.json` then passed all 23 commands before the subsequent stable-JSON capture refinement. Failed artifacts remain for transparency. The final `json-capture` run verifies the final leased prose against the original qualified copied binary.

## Independent product-fix retest

`DOCS_LWIKI="$PWD/bazel-bin/lwiki" python3 .artifacts/docs-usability/run_examples.py integrated-001` passed all 23 current-document runtime invocations on root's new binary, SHA-256 `c415f5a3dd83e08dc9d56e8be9ab68c883a17c831841bc269343f2b71beb2b44`. `examples-integrated-001.json` identifies that binary and the fresh disposable fixtures.

`python3 .artifacts/docs-usability/retest_root_ux.py 001` executed 20 additional black-box commands on that binary. Full arguments, exit codes, stdout and stderr are in `.artifacts/docs-usability/root-ux-001/*.json`. Assertions passed for concise init/capture/page summaries; copying the human search cursor verbatim to get a different next hit; nonexistent and directory inputs producing `USAGE` with corrective guidance; no-match context with empty stdout and explanation on stderr; bounded context with empty stdout, bound explanation and omission counts on stderr; exact page-body stdout plus hash on read stderr; stale page guards preserving the appended author edit; and zero-error final checks.

The retest also established two unresolved findings, reported to root immediately:

1. A staged source summary recommends `lwiki changes show CHANGE_ID` and `lwiki changes apply CHANGE_ID` without selecting the wiki. Copying that guidance from the repository working directory fails; include `--wiki` in continuation commands or clearly state the required vault working directory. Evidence: `capture-staged.json` and `stage-advice-without-wiki.json`.
2. Human search emits raw terminal control characters from supplied titles and source text. A title containing tab, newline, and ESC color control produces a forged-looking extra line and raw ESC in the output. JSON escapes these bytes correctly. Prefer escaped control characters for human search metadata/excerpts; preserve the deliberate raw-body contract of read/context. Evidence: `control-capture.json`, `control-search.json`, `control-search-json.json`. No credential or terminal exfiltration sequence was used.

Static renderer review additionally flags unconditional `CONTENT_CONFLICT` advice: `prepare a new change with --if-match HASH` applies to guarded page operations, but durable apply/recovery conflicts may require `recover` or `changes resolve`, and some structured graph inputs carry their own guards. A blanket claim that current files have been preserved can obscure an interrupted multi-file apply's earlier completed operations. Root should make the advice command-aware or describe inspecting retained work and reconciling author edits without prescribing an unsupported flag. The stale page-put case itself is correct and locally reproduced; this broader concern comes from the dispatch branch and supported command contracts, not a newly executed crash case.

## Changes and limits

- `README.md`: self-contained offline quickstart, realistic known-hit queries and expected outcomes, first-use navigation, explicit platform/version context, development/status evidence moved below first use.
- `docs/getting-started.md`: new beginner tutorial and practical page, source-refresh and skill-export tasks with recovery guidance.
- `docs/execution/reports/UX-DOCS.md`: this evidence and rubric.
- `.artifacts/docs-usability/**`: disposable sources/vaults, original failure, extracted-example runner, full per-command evidence and link results. These artifacts are ignored workspace evidence, not product files.

The exact build/install prerequisites were inspected against `docs/builds.md`, not installed or rebuilt here. No historical-release download, live provider, paid API, real vault, host GUI, Obsidian GUI, Windows writes or power-loss qualification was attempted. The tutorial's Obsidian/editor action is described but not executed. Runtime command completion is evidence of local executable accuracy, not evidence that unobserved beginners can complete the tutorial equally well. The guide's manual ID-copy steps remain a small cognitive cost; stable JSON makes them explicit and inspectable. Product-side concise mutation rendering and input-error improvements are owned by root.

## Final documentation review

The “unresolved” staged-guidance/control findings above describe the early `c415f5a3` retest checkpoint. Root subsequently implemented corrections and records their follow-up in [UX-VALIDATION.md](UX-VALIDATION.md); the original failing artifacts remain historical evidence. Retrieval implementation was a later separate lease, documented in [UX-EXCERPTS.md](UX-EXCERPTS.md), and does not change the scope of the original documentation walkthrough.

Final read-only QA checked all 65 relative Markdown links and heading anchors across README, getting-started, testing-usability, providers and the six UX reports; all resolve. The paired passage-usefulness table agrees with `comparison.json`: 539 old and 314 corrected passages, and 22 answerable questions per mode. Semantic expected-source hit@5 remains 22/22; improved usable passages do not imply improved source ranking or complete multifacet answers. Public guidance preserves offline/remote, citation/entailment, explicit budget and platform boundaries. Root's final optimized candidate and its 23-command tutorial replay are still pending at this checkpoint; this review makes no claim about that untested binary.

Root final replay: the fresh optimized macOS ARM64 candidate passed all23README/tutorial commands in disposable paths containing spaces. A final expanded file-link scan passed74local destinations; the earlier independent65-link/anchor review remains separately recorded. See UX-VALIDATION.md and UX-checks.json for binary provenance and complete evidence.
