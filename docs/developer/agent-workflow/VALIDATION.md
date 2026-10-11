# Validation gates

Choose checks for the changed integrated workflow, then broaden at consequential
or final acceptance gates. Passing component tests is evidence for that component;
it is not a complete product or release qualification.

| Area | Required observations |
| --- | --- |
| Selection/default | Explicit/nearest/default precedence, invalid chosen target refusal, path replacement/expected-ID rejection, fresh-process resume. |
| Config | Bounded strict JSON, absolute XDG, bypass/explicit precedence, protected regular-file writes, concurrent guards, exact v1 backup and unknown-version preservation. |
| Existing init | Preview purity, preflight every reserved collision before state, author/editor bytes unchanged, landing preserved by sync. |
| Author read/edit | Exact physical bytes/hash/coordinates, plain/unknown/BOM/CRLF/Unicode inputs, stale refusal, retained diff/backup, immutable/traversal/symlink refusal. |
| Source lifecycle | One guarded input observation, unique path identity including malformed claims, new current facts, old exact citations retained, no implicit recapture. |
| Pages | Named new draft, destination refusal, stable ID through editor/title/heading/move, accurate eligibility, author/provenance preservation. |
| Health/containment | Strict damage errors, healthy selected evidence, explicit issues/exclusions, failed observation cannot claim corpus absence. |
| Recovery | Reached native fault cuts, concurrent editor conflict, exact retained before/proposed bytes, replay/inverse authority, backup restore. |
| Retrieval | Actual required facts and absence controls in final text, every citation authentic, equal budgets, all failures/regressions counted. |
| Providers/accounting | Offline/dry-run no helper/network, bounded dispatch, uncertainty/unknown holds, stale responses and retry limits, live behavior qualified separately. |
| Documentation/skills | Fresh-clone links/anchors, public recipes, generated reference/schema consistency, export and actual host discovery/negative controls. |
| Release | Pin final executable/source/compiler/archive identity, native supported-target workflow/recovery, normal OS trust, exact staged and public download replay. |

Use [critic workflow](../testing-usability.md), [context protocol](../evaluating-context.md),
[capacity protocol](../testing-large-vaults.md) and [external qualification](../qualification.md)
for their separate thresholds and evidence rules. User-required all-task/high-score
gates cannot be waived by a high average, mock, cross-build or checkpoint commit.

Documentation checks use the reviewed purpose inventory in
[documentation policy](../documentation-policy.json). Run
`python3 scripts/check_docs.py` and `python3 scripts/test_check_docs.py` from the
repository root before integrating documentation changes. These checks supplement
actual recipe execution and independent content review.

## User documentation review

Apply the [writing guidelines](../../../AGENTS.md#writing-guidelines) to user
guides, CLI help and setup messages. Review the complete journey from installation
to using the reader's own notes with their chosen agent. It must produce a lasting
setup, with clear steps, expected results and a useful next action.

An independent reviewer follows the instructions without consulting implementation
contracts. They flag unexplained terms, missing steps, unnecessary caveats and
paragraphs written around internal mechanisms instead of the user's task. Read
prose aloud to catch unnatural wording. Move research and test details into the
developer reference. Resolve these findings before accepting the docs; passing
syntax, link and command checks alone is insufficient.
