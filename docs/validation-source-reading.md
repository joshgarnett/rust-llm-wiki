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
