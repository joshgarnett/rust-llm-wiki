# Try the 0.2.0 candidate

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

## Disposable collection walkthrough

Run this against a new fixture, then inspect the returned Source/Revision IDs:

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
"$LWIKI" --wiki "$DEMO/wiki" --offline search 'Atlas shipment' --verify-selected
"$LWIKI" --wiki "$DEMO/wiki" --offline context 'Atlas shipment' \
  --max-bytes 6000 --max-tokens 1500
# Copy a captured payload path from search's locator.path before reading it:
# --dry-run previews only the request; it returns no text.
# "$LWIKI" --wiki "$DEMO/wiki" --offline --dry-run --json read --path "$PAYLOAD_PATH" --max-bytes 16
# "$LWIKI" --wiki "$DEMO/wiki" --offline --json read --path "$PAYLOAD_PATH" --max-bytes 16
# Follow data.continuation with --start/--end; copy data.source_citation.citation.

"$LWIKI" --wiki "$DEMO/wiki" --offline check
```

The two originals retain separate immutable histories and share one committed
Change. Repeating `source import resume --key atlas` returns the existing
mapping. Use the [import guide](source-imports.md) for updates, interrupted runs
and backups, and [indexed context](indexed-context.md) for supported query modes
and verification boundaries. Normalized catalogs currently support plain
lexical discovery and document context; other modes can refuse explicitly.

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
