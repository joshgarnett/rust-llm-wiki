# Verified source-reading workflow

Ordinary normalized source reads now return reusable exact citations for their
returned ranges. The independently assessed import → verified search → deeper
read → refresh → withdrawal/history → offline reconstruction workflow passed
its frozen scoped gate at **10.0/10 with no blockers**. This is a small synthetic
workflow result; automatic HIGH, generated answers and 25k capacity remain open.

## Public behavior

JSON `data.source_citation.citation` contains the existing Source/Revision/span/
quote-hash citation object. Its explicit eligibility distinguishes Current,
historical and withdrawn evidence. The read authenticates selected dependencies
and binds the returned path, full hash and UTF-8 range to those exact bytes.
An older revision remains historical after refresh; withdrawal never promotes
its retained text to Current. External selected edits refuse without a citation.

Continuation uses the returned range endpoints. Human metadata appears on stderr
and stdout retains exact source text. The continuation command retains the vault
and `--no-sync` when requested. Cached, legacy, authored, empty and dry-run reads
remain uncited. See [the reading contract](indexed-context.md#continuing-a-captured-source-read).

The subsequent [occupied 10k diagnostic](validation-10k-workflow.md) retained
correct ordinary reads and citations but failed a dry-read deadline. CLI dry-run
now plans the request without resolving targets or returning body text; this
explicit compatibility change passed its [separate acceptance gate](validation-dry-read-preview.md)
at 10.0/10 with no blockers.
The small historical lifecycle score above applies to its frozen executable.

## Development comparison

The prospectively frozen recipe kept the same lexical query, five hits, 80
candidates and 1,024-byte excerpts. It then read unseen source intervals in stable
round-robin order, with one counted endpoint bootstrap per owner. Allowances were
32 reads of at most 8 KiB and 256 KiB read text per task, separately charging
search text, repeats and JSON transport. This deliberately differs from HIGH.

All twelve baseline/candidate arms returned in 166 CLI calls and 4.199 seconds.
Both evidence unions completed 5/5 positive tasks and all fifteen required
propositions, without baseline losses. The absent measurement stayed unsupported.
The candidate adds read citations, not improved text selection over the baseline.
Independent audit passed 60 search citations, 77 candidate Current read citations,
154 body ranges, accounting and original/copied-file preservation. Each task read
about 57.9–83.5 KB in 11–14 reads; bootstrap repetition was measured and charged.

## Independent lifecycle acceptance

A fresh critic authored four approximately 40 KB synthetic inputs and an update,
then froze five positive tasks and one absent task before candidate outcomes.
Expected facts stayed outside the indexed corpus and operator inputs. The fixed
public sequence completed 162 CLI calls in 7.615 seconds, including grouped import,
content refresh, exact historical reads, withdrawal, complete backup/cache loss,
offline normalized rebuild/full check, and cache/empty/authored/dry-run/human/
external-edit controls. Every positive task was evidence-complete; the absent
measurement remained unsupported. No answer-generation stage ran.

Independent audit passed 169 JSON citations (157 Current, six historical, six
withdrawn), 142 read ranges, 100 continuation warnings, human output, 324 raw
output hashes, all runtime pins and stage inventories. Fifteen immutable revision
files and the complete 74-file backup passed preservation checks. Full check
reported zero diagnostics and canonical/cache agreement. Returned bodies totaled
about 1.13 MB across lifecycle tasks and controls; broad reading nearly exhausted
discovered owners and does not establish retrieval precision or generality.

## Checks and limits

One combined release-opt3 native ARM64 build pinned 386 source/asset files. Two
new public read test groups and two cached/legacy regressions passed. Their first
launch failed to locate the compiled relative binary; fixing only the launch
working directory allowed the four failed groups to pass without a rebuild.
Seven separate experimental mechanics checks passed and were not repeated.

Timings are owner-monotonic small-fixture observations, including setup and
supervision, not shipping latency distributions or capacity qualification. Peak
RSS and ordinary-read proof counters were unavailable. No providers, vectors,
model answers, private HIGH evaluation, live vault or other-platform qualification
were involved. Detailed pinned inputs/outputs are local execution evidence;
a fresh checkout can run the public read tests but does not include historical
private questions or raw receipts. See [the candidate guide](testing-0.2.0.md).

## Actual 10k development comparison

The bounded match-to-read workflow now accepts `read --start N` without an end:
the selected authenticated body supplies EOF, and the byte cap and returned
continuation still limit each read. Explicit ranges remain strict. This fixes
a real copied-command failure; it does not change retrieval ranking.

On an actual 10,000-Source vault, five exposed development questions compared
historical context (H), ordinary context (D), ordinary verified search (S), and
the first two returned captured owners followed by bounded forward reads (S+R).
R retained the original filters, started at returned locations and followed only
actual continuations. All five questions and every requested fact were mandatory.

| Task | H facts | D facts | S facts | S+R facts |
| --- | --- | --- | --- | --- |
| Incremental compilation and config override (P08) | 3/3 | 3/3 | 0/3 | 3/3 |
| Incremental values and disabling default features (P16) | 3/3 | 3/3 | 0/3 | 1/3 |
| HTTP timeout controls and feature-syntax support (R13) | 1/3 | 1/3 | 1/3 | 3/3 |
| Network retry default and shared features (R14) | 1/2 | 1/2 | 0/2 | 1/2 |
| Scoped offline behavior and override (R16) | 3/3 | 3/3 | 1/3 | 3/3 |

H, D and S+R each completed **3/5 tasks and 11/14 facts**, with different tasks
passing; S completed none. Every arm failed the complete-answer gate. The larger
D packet added no required fact over H. Forward reading recovered R13 but missed
earlier P16 evidence already in D and left R14 incomplete. Later reads should
augment existing support; exact citations alone do not establish completeness.

The final collection used 35 commands (15 discovery and 20 reads), 142,313 read
body bytes and 239.219 seconds in its owning monotonic interval. Summed native
command intervals were 1.038 seconds; collection/proof overhead is included only
in the former. Independent audit authenticated all 81 SourceRef occurrences
(56 unique), actual read ranges, continuations and publication, with no citation
or operational blockers. Input/revision preservation passed. Ten unused read
slots were not reassigned. The original four-command missing-end failure remains
part of the record; the two attempts total 39 commands, not a paired baseline.

Native read checks passed after narrowly correcting launcher/test assumptions.
That broader checkpoint also reproduced an independent Page-move defect; its
failure was retained for a separate correctness milestone. This comparison used
a pinned native ARM64 release executable with opt-level 3 and debug information
disabled. It did not run an answer model, provider or unseen acceptance questions.
Historical default 6.5, HIGH, semantic and actual 25k capacity remain open.

A separately declared zero-new-call control retained ordinary context alongside
the same successful search/read envelopes. It completed **4/5 tasks and 13/14
facts** within 128 KiB of full raw stdout per task, without deduplication or
trimming. All 60 citation occurrences passed; R14's retry-default support still
remained missing. This supports preserving acquired evidence, while native D
remains 3/5. No answer model or unseen acceptance stage ran in that control.
