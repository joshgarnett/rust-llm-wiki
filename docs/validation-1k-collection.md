# 0.2.0 collection control

The native macOS ARM64 candidate completed a public 1,000-document lifecycle on
2026-10-04. Independent assessment: **8.5/10; frozen acceptance failed**. The
required cited-search workflow is missing and the initial preparation preview
was unrun. The earlier 9.5/10 small importer assessment remains separate.

## Execution and results

Release optimization level 3; Apple M1 Max, 32 GiB RAM, APFS, macOS 26.5.2.
Executable SHA-256:
`8d5a039e7249cbcb6a96302cfee5014c754d2f76e6833ba431e6ea86395a953c`.
The seeded varied corpus contained 1,000 UTF-8 files totaling 100,324,606 bytes,
with distractors and facts at different positions. Expected answers stayed outside
indexed content. All commands were offline, without production-vault edits,
provider calls or competing builds.

| Workflow | Observed result |
| --- | --- |
| Public schema-2 migration, activation and import | 1,000 unique Source/Revision mappings, 250 four-item groups, byte-exact originals/content. Five import windows totaled 400.965 seconds. |
| Real interruption | SIGKILL after observing four pending allocations at item 248; public resume preserved their identities/times without duplicate or lost mappings. Process restart does not establish power-loss safety. |
| Reads and updates | Twenty operational reads matched inputs. Twenty exact-request originals were also read, but after churn through explicit historical paths; the ordering limitation remains recorded. Ten refreshes preserved originals; one withdrawal removed Current eligibility; repeated refresh was a no-op. A separately counted 53-byte capture produced a valid citation. |
| Recovery and maintenance | Repeated completed resume/recover added no publication. Sync and full checks passed canonical/cache/revision-owner-history validation. |
| Complete cache loss | After complete backup, only `.wiki/cache` was removed. Normalized rebuild took 4.991 seconds. All 100 ordered context results matched after excluding only the predeclared generation header. SHA-256, size and mtime matched for 8,378 canonical/retained files; legitimate operation/writer authority changes were recorded separately. |
| Previews and refusal | Source add/refresh/withdraw and resume/context previews preserved whole-tree bytes/mtimes. A same-size/same-mtime selected-payload edit in a complete copy returned exit 4, `FRESHNESS_CONFLICT`, without evidence. The checker incorrectly expected exit 5, `SOURCE_INTEGRITY`; its failed assertion and actual response remain retained without replay. |
| Functional inputs | Separately counted non-ASCII, empty, unsupported binary and two licensed Cargo documents passed storage/read/index checks. Non-ASCII citations matched UTF-8 bytes. No semantic-quality credit. |

## Retrieval

The frozen schedule completed **746 queries**: 706 context requests, including
prefix observations, and 40 searches. Context used lexical Automatic selection,
80 candidates, five passages, 1,024-byte excerpts, 6,000 rendered bytes and 1,500
estimated tokens. All **2,898 context citations** passed independent current
revision, eligibility, exact UTF-8 span and quote/content hash validation.
Exact tasks, source filters, absent-token controls and withdrawal exclusions passed.

Initial completeness was 97/100 outcomes, or 87/90 positive tasks. Two distant-fact
requests omitted an early restore operator; one two-source request omitted restore
volume. After churn and rebuild: 98/100 outcomes, or 87/89 positives plus eleven
negatives. The distant-fact omissions persisted. The changed corpus resolved the
two-source omission; its initial failure remains recorded.

All 200 search hits lacked citations. This matches generation-scoped discovery
today, but fails the required cited-search workflow. Raw facts appeared in 19/20
initial searches and 18/19 positive searches after churn; raw text earns no citation
credit. Document context supplies source citations. Selected verification for
search is subsequent work.

## Resources and limits

Warm context p95: 0.355 seconds initially, 0.350 seconds after churn. All 700
full-size context calls stayed below half a second. Import main-process peak RSS:
38,518,784 bytes; largest main-process peak across all calls: 168,034,304 bytes.
Exact process-tree peaks are **unavailable**; periodic sampling cannot establish
them, and 41 sampling errors across six early invocations remain recorded.

All 839 CLI attempts and helpers were retained. Their separately owned monotonic
intervals totaled 662.690 seconds, excluding agent/preparation turnaround. Final
owned allocation, including controlled copies: 3,475,079,168 bytes; free space:
109,320,376,320 bytes. Observed bounds passed; unavailable exact tree peaks prevent
full resource qualification.

This partial control omits the Page/graph overlay and establishes neither
10k-to-25k scaling nor 25k capacity. The 53-byte capture does not qualify 100 KiB
maintenance. Synthetic results do not replace failing realistic tasks or unseen
HIGH acceptance. Remaining modes, default activation, strict Clippy and other
native platforms stay open. Next: explicitly verified search, remaining context
completeness and measured scale progression. See the [trial guide](testing-0.2.0.md),
[contracts](current-contracts.md) and unchanged [25k protocol](testing-large-vaults.md).
