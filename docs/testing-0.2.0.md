# Try the 0.2.0 collection workflow

Use a supplied 0.2.0 trial executable with a new disposable vault. Set `LWIKI` to
its absolute path and confirm that `capabilities` includes `page_source_refs`
and `schema page-source-refs` is available before the cited-Page steps. Historical
0.2.0 artifacts below may predate that capability. This walkthrough uses offline local files and explicit
normalized activation; no provider setup is needed. It connects import, cited
evidence, a draft Page, Source refresh and guarded Page reconciliation. Capacity,
default answer completeness and full release qualification remain open; historical
candidate artifacts and validation results are retained below.

## Import two files and inspect completion

Keep inputs and draft work files outside the vault. `init` creates a vault that
uses the legacy layout until `index rebuild --normalized` explicitly activates
the normalized catalog:

```sh
"$LWIKI" --version
"$LWIKI" --offline --json capabilities
DEMO=$(mktemp -d)
printf 'Atlas shipment contains 17 amber crates.\n' > "$DEMO/shipment.txt"
printf 'Atlas inspection begins at 06:40.\n' > "$DEMO/inspection.txt"
printf '%s\n' '{"path":"shipment.txt"}' '{"path":"inspection.txt"}' > "$DEMO/inputs.jsonl"
"$LWIKI" --offline init "$DEMO/wiki" --title '0.2.0 walkthrough'
"$LWIKI" --wiki "$DEMO/wiki" --offline index rebuild --normalized
"$LWIKI" --offline --json source import prepare \
  --input-list "$DEMO/inputs.jsonl" --output "$DEMO/import.jsonl"
"$LWIKI" --wiki "$DEMO/wiki" --offline --json source import run \
  --manifest "$DEMO/import.jsonl" --key atlas --group-size 4
"$LWIKI" --wiki "$DEMO/wiki" --offline --json source import status --key atlas
```

Require `completed: true` and inspect the returned mapping. These two inputs have
separate Source IDs and immutable Revision histories, with one shared committed
Change. If incomplete, inspect the reported state and use `source import resume
--key atlas` for this same import; another key creates fresh identities. Keep the
manifest and returned `results_path` journal. The [import guide](source-imports.md#run-inspect-and-resume)
explains interruption hints and how to recover mappings across multiple groups.

## Obtain cited support, then save a draft Page

Ask for both facts and inspect the actual returned evidence:

```sh
"$LWIKI" --wiki "$DEMO/wiki" --offline --json context \
  'Atlas shipment crates and inspection time' > "$DEMO/context.json"
"$LWIKI" --wiki "$DEMO/wiki" --offline --json search \
  'Atlas shipment' --verify-selected > "$DEMO/search.json"
```

These ordinary commands use lexical document retrieval. Normalized context
resolves to `indexed-documents`, with `--limit 10`, a 12,000-byte context
ceiling and a 3,000 estimated-token ceiling; plain search defaults to 10 hits
with 240-byte excerpts.
Plain search supplies uncited cached discovery; `--verify-selected` authenticates
displayed dependencies and supplies exact citations for nonempty captured text.
Context verifies selected evidence against the published discovery generation.
Neither proves global freshness or that every requested fact was returned. Check
both requested facts, omissions and qualifications before drafting.

If a fact is missing, follow the [bounded gap-reading recipe](../skills/llm-wiki/references/cited-page.md#read-remaining-gaps).
Use the captured `locator.path` actually returned by discovery, rather than
inventing a payload path or reading a Revision metadata record as source text:

```sh
# Set PAYLOAD_PATH from an actual returned captured-text locator.
"$LWIKI" --wiki "$DEMO/wiki" --offline --json read \
  --path "$PAYLOAD_PATH" --max-bytes 4096
```

Inspect the returned text, `data.source_citation.citation` and eligibility. Follow
`data.continuation` with its returned `--start`/`--end` if truncated, within your
chosen evidence allowance. Each returned range has its own citation. Dry-run
previews return no text or evidence, and cached `read --no-sync` is uncited. See
[exact read and continuation](indexed-context.md#continuing-a-captured-source-read).

Follow [answer and save](../skills/llm-wiki/references/cited-page.md#answer-then-save-only-when-authorized)
to write `$DEMO/answer.md` and `$DEMO/refs.json` outside the vault. The body should
answer only supported conditions, with explicit gaps. The refs request uses
`schema_version: "1"` and `citations` containing the unchanged returned
`kind: source` CitationRefs, including their actual Source/Revision/span/hash;
inspect `schema page-source-refs` for the schema. Then save:

```sh
"$LWIKI" --wiki "$DEMO/wiki" --offline --json page init \
  --file "$DEMO/answer.md" --title 'Atlas shipment draft' \
  --source-refs "$DEMO/refs.json"
```

Inspect the allocated Page ID/path and read the saved Page. The command generates
links for its actual Page path; do not calculate citation links by hand. A draft
may be absent from ordinary current search; use `search 'Atlas shipment draft'
--kind page --status draft --verify-selected` to discover it explicitly. Valid
citations establish the referenced bytes, not the truth or completeness of prose.

## Refresh one Source and reconcile the Page

Use the shipment Source ID from the actual import mapping; read that Source and
its returned current Revision first. Follow the [maintenance recipe](source-import-maintenance.md#recover-the-original-mapping)
when identifying items from a larger completed import. Prepare a replacement,
then stage, inspect and apply:

```sh
printf 'Atlas shipment contains 19 amber crates.\n' > "$DEMO/shipment.txt"
# Set SOURCE_ID from the shipment item in the actual import mapping.
"$LWIKI" --wiki "$DEMO/wiki" --offline --json read --id "$SOURCE_ID"
"$LWIKI" --wiki "$DEMO/wiki" --offline --json --stage source refresh \
  "$SOURCE_ID" --file "$DEMO/shipment.txt"
# Set CHANGE_ID from this staged result.
"$LWIKI" --wiki "$DEMO/wiki" --offline changes show "$CHANGE_ID"
"$LWIKI" --wiki "$DEMO/wiki" --offline --json changes apply "$CHANGE_ID"
```

Before applying, inspect the complete proposed original payload and expected
Source before-state, as described in [stage, review, then apply](source-import-maintenance.md#stage-review-then-apply).
Staging accepts a snapshot; later edits to the external input do not change it.
Managed import, Source refresh and Page writes appear through catalog publication.
They do not require a global sync after each write. Source refresh does not
rewrite the Page's prose or citations.

Retrieve the new evidence, reassess both facts, and keep the newly returned refs.
Read the whole, nontruncated Page using its actual allocated ID/path. Retain its
record metadata, all author text and notes, and its returned `data.hash`. Follow
[guard and reconcile an edit](../skills/llm-wiki/references/cited-page.md#guard-and-reconcile-an-edit)
to form `$DEMO/revised-page.md` as a full Markdown proposal and
`$DEMO/revised-refs.json` from current returned citations:

```sh
"$LWIKI" --wiki "$DEMO/wiki" --offline --json read --id "$PAGE_ID"
# Set PAGE_PATH and AUTHOR_HASH from that whole Page read.
"$LWIKI" --wiki "$DEMO/wiki" --offline --json page put \
  --file "$DEMO/revised-page.md" --path "$PAGE_PATH" --if-match "$AUTHOR_HASH" \
  --source-refs "$DEMO/revised-refs.json"
"$LWIKI" --wiki "$DEMO/wiki" --offline --json read --id "$PAGE_ID"
```

Verify stable Page identity, draft status, revised facts, citations and preserved
notes. A stale author hash refuses; reread and reconcile the actual author file.
For intended external wiki-file edits, run `index sync` before verified rereading
or discovery; [external Page synchronization](external-page-sync.md) explains
that boundary. Updating the input file outside the vault requires the Source
refresh above to capture it; sync alone does not import it.

The old SourceRef remains bound to its old immutable Revision and bytes, and
becomes Historical after refresh. An ordinary read of its retained captured path
and range can inspect that history with the returned eligibility; do not relabel
it Current. [Page reconciliation](source-import-maintenance.md#reconcile-the-cited-page)
keeps useful history explicit while updating current claims.

## Keep history and recover safely

Quiesce writes and back up the entire vault, including `.wiki/state` and
`.wiki/retained`, as described in [interrupted runs and backups](source-imports.md#interrupted-runs-and-backups).
Import progress, retained snapshots and naming commitments are operational state,
not rebuildable cache. Preserve unresolved operations and inspect their reported
Change IDs; use the documented same-key import continuation or ordinary recovery
rather than deleting state. Run `check` when you need a separate complete audit:

```sh
"$LWIKI" --wiki "$DEMO/wiki" --offline check
```

For query capabilities and proof boundaries, see [indexed context](indexed-context.md#mixed-document-context-on-normalized-indexes).
Normalized document retrieval supports lexical and literal queries; semantic and
hybrid routes require compatible prepared vectors, including cached query vectors
for offline use. Strict current/historical context and general graph expansion
remain unavailable on normalized catalogs. This local walkthrough adds no new
performance, capacity or answer-quality qualification.

## Historical candidate artifacts and validation

The following records describe their identified historical builds and fixtures.
They are retained as evidence; an existing packaged executable does not acquire
later Markdown changes from this checkout.

This build adds [selected search citations](validation-verified-search.md), normalized Page authoring, individual source capture/refresh/
withdrawal, bounded catalog reconstruction, context-selection replay and a
[resumable local collection importer](source-imports.md). The first capacity
target is now 25,000 documents / roughly 2.5 GB of text. Capacity and unseen
retrieval completeness remain unqualified; the version number does not certify
those gates.

Candidate010 includes the maintained [question-to-draft skill recipe](../skills/llm-wiki/references/cited-page.md)
and its 15-command normalized author-conflict fixture. A [fresh three-Source task](validation-skill-transfer.md)
supplied all six requested facts with five verified SourceRefs and preserved the
author note. The independent score is 9/10, but mandatory layout and prescribed-edit
gates failed because their instructions were omitted from the operator handoff.
Those failures remain explicit; this is a development checkpoint.

The local unsigned macOS ARM64 archive is
`lwiki-0.2.0-macos-arm64-candidate-010.tar.gz`, SHA256
`a54ec99a47f0c9409c992fdacc8c5cc46e474dd2fe9d819af100b9bff11d1d73`.
It packages the optimized executable from source commit
`942fd4486cc4d3904eec15045bf7000517e9490c` and the exact seven-file tested export.
Thirteen bundled files passed byte/permission checks, and all 282 recorded build
inputs matched that committed tree. Packaging reused the pinned binary without
another build or native replay; no public release, signing or notarization occurred.

Candidate009 adds [bounded ordinary-publication WAL reclamation](validation-wal-lifecycle.md).
Independent scoped acceptance passes all six mandatory conditions: 26 affected
native checks, 700 public query workflows, citations, recovery and 100 rebuilt
result sequences. The largest observed selected WAL is 8 KiB allocated;
post-update whole-task p95 is 0.514 seconds versus the baseline's 1.292 seconds.
Evidence completeness is unchanged. The existing disk guard still refuses 10K;
representative 25K and native HIGH remain open.

The latest candidate also includes the [selected identity repair](validation-selected-identity.md):
an empty authenticated Current Entity hit no longer aborts verified discovery
when its description is unsupported. Sixteen native tests and a 54-observation
public update/withdrawal/navigation/cache-loss replay passed, with independent
scoped acceptance at 9.5/10 and no blockers. Unsupported body evidence still
refuses; broader missing-fact and capacity failures remain open.

The new candidate also includes [per-import migration validation reuse](validation-import-throughput.md#per-import-validation-reuse-and-paired-control).
It preserves every fresh plan read/hash and marker check while skipping repeated
semantic decoding during one import. Thirty-seven native checks and a twenty-call
paired control passed, with independent scoped acceptance at 9.4/10 and no
blockers. On the occupied fixture the candidate imported 2.208 items/s, 2.200 times
the prior build; the small-fixture ratio was 0.940. These single observations meet
the frozen thresholds and do not qualify 25k capacity or complete answers.

The latest candidate adds [exact source-read citations](validation-source-reading.md).
Verified search can be followed by deeper reads and directly reusable citations
for the newly returned bytes, with Current/historical/withdrawn state. Its small
synthetic import/update/withdrawal/cache-loss workflow passed independent scoped
acceptance at 10.0/10. Broad reading completed the exposed development questions
on both old and new builds; the new build supplies citation metadata. Automatic
context completeness and representative capacity remain separate open gates.

Candidate006 also includes [request-only dry-read previews](validation-dry-read-preview.md),
with independent scoped acceptance at 10.0/10. This explicitly changes CLI
`--dry-run read`: it now reports the requested selector/range/limit/mode and
returns null body/citation, leaving target, content endpoints and freshness
unverified. Run without `--dry-run` to obtain text. Forty previews and sixteen
ordinary controls passed on small legacy/normalized vaults and an owned 10k copy;
the slowest preview took 0.067 seconds. The original
[occupied 10k diagnostic](validation-10k-workflow.md) remains failed at its old
dry-read deadline. The [representative 1k observations](validation-representative-1k-workflow.md) now
complete import and lifecycle controls separately; 25k qualification remains open.

Candidate007 adds [normalized authored Page reorganization](page-reorganization.md),
with scoped independent acceptance at 10/10 and no blockers. It moves a stable
Page ID, updates known authored/plain/unadopted incoming links and immediately
supports guarded edits, search/read/context and recovery. All 86 public commands
completed; exact catalog and returned results matched after offline cache-loss
reconstruction. Reached native filesystem/SQL cuts and repeated ordinary apply
passed. Other target kinds remain unsupported on normalized catalogs, and external
edits need explicit sync before incoming discovery. The guide explains author
hashes, space-containing paths, immutable refusals and request-only previews.

The candidate now has an actual [1,000-document collection control](validation-1k-collection.md):
import, interrupted resume, refresh/withdrawal, recovery and complete-cache-loss
rebuild succeeded. The broader frozen assessment is 8.5/10 and remains failed:
cached search supplies no source citations, and the initial preparation preview
was unrun. Context citations passed; two distant-fact tasks still missed required
information after churn. The new explicit `search --verify-selected` workflow separately passed at 9.5/10
with no observed correctness blocker; it preserves search contents and does not
fix those missing facts. This is a local trial with those limits visible.

The current candidate is a native macOS ARM64 release build, compiled with Rust
optimization level 3 on macOS 26.5.2, with minimum macOS deployment target 26.5.
Other native platforms require their own qualification. Windows vault writes
remain unsupported. Set `LWIKI` to the supplied executable's absolute path.

## Validation boundary

The grouped importer checks cover 168 distinct native unit tests, supplemented
by three adapter/presentation checks, thirteen focused recovery/replay checks
and the existing offline CLI and source-evidence integration targets. Passing
unchanged checks were reused rather than rerun as a full suite. An independent
walkthrough passed 109 public CLI calls and 289 assertions on disposable vaults
in both storage layouts. Its interrupted-state extension reached 173 calls and
524 assertions with one public recovery failure after a successful resumed
import. The narrow historical replay correction passed its native checks;
independent replay of that exact preserved failure passed eighteen further CLI
calls and seventy valid assertions. The scoped assessment is **9.5/10 with no
observed correctness blocker**; resource qualification remains open. The earlier
plain-text diagnostic omission was corrected and publicly replayed.

A controlled four-input comparison preserves exact manifests, inputs, original
baselines and cited readiness. Grouping four inputs uses one publication rather
than four, and intercepted file/directory sync calls fall from 1,124 to 708.
This is one operation-count comparison, not a latency distribution or capacity
claim. SQLite and direct sync paths are excluded from those counters. Peak RSS
and launch free-space observations were unavailable; this comparison does not
qualify resource bounds.

The [scale protocol](testing-large-vaults.md) and retrieval evaluation retain
their own acceptance gates. Strict Clippy remains failing. Native returned-error
reopen tests do not establish power-loss safety, and local fixtures do not
qualify a live provider.
