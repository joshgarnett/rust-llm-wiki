# Ordinary HTML evidence workflow

Local `.html` and `.htm` captures now support the ordinary import → search →
cited read → draft Page → refresh workflow. Previously, valid UTF-8 HTML was
captured as original-only, while the lexical projection omitted ordinary HTML
text. Scalar capture and manifest preparation now share filename admission, and
catalog text and excerpt mapping consume one parser-offset text stream.

Original and captured text bytes remain identical. Lexical discovery omits tags,
attributes, comments, script and style, including quoted `>` attributes and
suppressed regions split across parser events. Removed markup introduces
separators. Citations authenticate raw byte slices, which can include intervening
markup. HTML headings contribute body text, without new heading metadata.
HTML-event runs retain raw entity spellings; Markdown Text/Code events retain
their existing decoding. This is a bounded lexical projection, without browser
rendering, CSS visibility, encoding conversion or general malformed HTML repair.

## Compatibility and checkpoint

The parser fingerprint changes to
`markdown-lexical-events-v2-ordinary-html-span-stream-v1`. Complete retained,
unapplied indexed Changes using their preparing binary before upgrading and
explicitly rebuilding. A new binary refuses the obsolete preparation proof;
it does not discard or regenerate the operation authority. Completed receipts
remain readable. Frozen unsupported import policies remain unsupported on
resume. Refresh creates a complete successor; old original-only revisions stay
original-only.

The native ARM64 release checkpoint passed **321 distinct affected checks**:
89 projection/import/catalog/application checks, 207 retrieval/CLI checks and
25 offline public checks. Eleven ignored diagnostics remain unrun. Two obsolete
test expectations failed initially: valid HTML was expected to be unsupported,
and already-supported normalized query modes were expected to be unavailable.
The grouped fixture corrections retained those failures and replayed only failed
checks. No production code changed during that correction phase.

The three planned checkpoint intervals total **807.362 seconds**, within the
original 1,200-second aggregate. The CLI was byte-identical across them; the last
build compiled only the corrected unit-test target. Actual compiler parameters
confirm optimization level 3, debug information 0, native
`aarch64-apple-darwin` and minimum macOS 26.5. The CLI SHA-256 is
`4313f3835db1c0b8f1789ad31f23dc6d21d3c833a8ac5563c972a73ce7d57fc9`.

## Actual public workflow

Ten synthetic inputs, totaling 134,761 bytes, were frozen before implementation.
Expectations remained outside indexed input. The prospective protocol required
eight mandatory groups, a critic score of at least 9/10 and zero correctness
blockers. It declared 124 commands, five-second query and thirty-second other
command limits, 600 summed owning seconds, 128 MiB combined known reads and
128 MiB owned allocation, 16 MiB streams, a 32 GiB free-space floor and no provider
calls. These small fixtures do not qualify a maximum source size or vault scale.

All **124 public commands completed**, with zero unrun commands, in both legacy
and migrated retained storage layouts. Expected refusals were limited to obsolete
parser authority and a stale Page author hash. The workflow exercises:

| Mandatory group | Observed workflow |
| --- | --- |
| Capture and qualified evidence | Exact HTML original/content bytes, lexical results and cited export procedure including its required conditions. |
| Collection import | Case-varied `.HTM`, prepare/run/resume/status, retained identities and frozen old unsupported policy. |
| Span mapping | Unicode, inline markup and quoted attributes; returned evidence maps to original bytes. |
| Negative evidence | Attribute/comment/script/style-only and absent tokens excluded; raw literal discovery and Markdown controls retained. |
| Bounded projection | Fragmented and truncated markup, 2,048 repeated tags and small context output limits. |
| Immutable history | Original-only revision unchanged, complete refresh successor and correct current/historical/withdrawn eligibility. |
| Draft Page | Actual returned citations, exact guarded edits, both author notes, stale-hash refusal and reads after withdrawal. |
| Upgrade and recovery | New-binary refusal, old-binary completion of the same staged Change, explicit upgrade and complete cache-loss check/search/context/Page parity. |

The first operator attempt stopped after 52 successful commands because its
post-withdrawal assertion expected historical eligibility. The actual read
correctly returned withdrawn eligibility with the same retained bytes and
citation. That attempt remains **failed**, rather than being reclassified.
An independent prospective review admitted one continuation of only the 72
unrun commands; none of the completed prefix was replayed. Legacy immutable
revision commitments were reconstructed from public Change identities and
authenticated committed manifests, including revision metadata, before taking
the continuation preservation baseline.

Two root seal-preparation failures also remain recorded. A final explicit
critic amendment increased only cumulative preparation allowance from 1 MiB to
2 MiB; the original aggregate limits and command schedule remained unchanged.
Final preparation read upper bound was 1,876,992 bytes. Executable identity reused
the original full hashes and root's no-writer assertion with current file
metadata. Initial inode metadata was not recorded; this is not a fresh executable
hash verification.

Cumulative owning time, including a conservative thirty-second preparation debit
and prior attempt, was **74.594 seconds**. Combined exposed known reads were
**77,907,707 bytes** and streams **296,493 bytes**. Maximum observed query interval
was **0.0762 seconds**, and maximum command interval **1.496 seconds**, including
launch, capture and receipts. The supervisor uses its own monotonic clock and
explicit prior-duration debits. Unknown internal nonquery reads, physical I/O,
RSS and instantaneous allocation peaks remain unavailable. These are small-fixture
observations, not a throughput comparison or capacity claim.

## Acceptance and remaining work

Independent actual-content review passes at **9.3/10**, all eight mandatory
groups and zero observed correctness blockers. The continuing implementation
critic independently authenticated **70 SourceRef appearances, 16 distinct
references**, actual returned quotations, owner/revision membership and
phase-sensitive eligibility. It checked all 124 receipts, 233 sealed artifact
pins, 20 imported original payloads/policies, 199 protected legacy files,
41 legacy immutable commitments and 41 retained-layout revision files.
Retained cache directories exist but were not independently fully rehashed.
The retained-layout full noncache comparison is an executed operator assertion;
its persisted revision inventories support the independent per-file audit.

The critic's own observed review interval was 214.642 seconds, with 5,260,994
known bytes read. Root dispatch to final report was **251.343 seconds**, missing
the requested 240-second handoff by 11.343 seconds. This separate procedural
gate remains failed. This is scoped product correctness acceptance, rather than
an unqualified pass of every review condition. The reviewer is independent from
implementation but continuing, not a fresh or blind evaluator.

The constrained repeated-tag query returned 165 bytes and 42 estimated tokens,
with explicit truncation/omissions and **no evidence**. It earns bounded-output
credit only. Omitting markup from lexical discovery does not sanitize a raw
citation span that crosses intervening original markup or script text.

This milestone makes previously original-only HTML usable in the cited workflow.
It does **not** improve the measured default retrieval baseline: 9/28 facts and
2/10 complete positive tasks remain the last accepted development observation.
Unseen HIGH, semantic readiness/default activation, 25K qualification and full
release gates remain open. The separately accepted
[4,096-current Markdown lifecycle](validation-capacity-workflow.md#separate-native-4096-current-markdown-lifecycle)
uses the preceding binary and earns no HTML-scale credit.

The next selected workflow is a bounded, authenticated original import mapping
through existing individual refresh and cited Page reconciliation. First compare
the existing public route with a small mapping experiment. Add only the missing
user-facing step established by that experiment; no collection mirror, automatic
withdrawal, general repair subsystem or renewed retrieval parameter loop is
authorized by this milestone.

## Local 0.2.0 trial artifact

Candidate `015-normalized-html-001` packages the same tested CLI without another
Rust build. Production source is commit
`99a8da8b089cb749d400a92a34a45e488c2e726d`. All 596 frozen files were compared
with the accepted index: 593 matched, with only three maintained documentation
differences. The new validation document is also documentation-only. All frozen
production source, build inputs and embedded skill assets matched; unfinished
main-working-tree experiments were preserved and excluded from the build.

The local archive is `lwiki-0.2.0-aarch64-apple-darwin.tar.gz`, 11,152,318 bytes,
SHA-256 `8a7bafe4409d426b7e3186f36b7b8df29180b0069282ac8955d034ed52582d1e`.
It requires macOS 26.5 or newer on Apple Silicon. The copied executable's hash,
version/capabilities, seven-file Codex skill export, five embedded guides and all
four regular archive members passed their artifact checks. These checks used
three offline commands; no workflow or unchanged Rust test was replayed.

Packaging took 2.737 owning monotonic seconds including its receipt, with
105,068,114 known content/decoded bytes, 43,012,096 allocated output bytes and
35,178,192,896 free bytes at the terminal observation. This is a local trial;
there is no tag, publication, six-platform qualification or new completeness
or capacity credit.
