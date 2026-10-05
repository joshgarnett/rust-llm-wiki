# Public cited-investigation development control

Both fixed reading recipes **failed their completeness gates**: the original
question arm R completed **3/5** positive tasks, and the query-only clause arm P
completed **2/5**. Exact citations and selected reads passed. The experiment
qualifies neither native HIGH context nor generated cited answers.

## Workflow and actual results

Six exposed development questions comprised five answerable tasks and one absent
measurement. R used verified lexical search with the original question and four
hits. A fresh query-only planner froze ten clauses before seeing results; P used
one hit per clause, without replacement for duplicate paths. Each selected source
received a 1,024-byte prefix read, exposing its public endpoint, and a 1,024-byte
read beginning at the returned search citation anchor. Expected facts remained
with the independent critic, outside indexed content and planner inputs.

| Task | R | P |
| --- | --- | --- |
| Actionable CLI error guidance | Complete | Complete |
| Injected-writer testing without spawning | Complete | Incomplete |
| How-to routes, overlap and assumed competence | Incomplete | Incomplete |
| Requested lwiki / Intel Mac latency measurement | Unsupported | Unsupported |
| Panic unwind versus abort and release selection | Complete | Complete |
| Failed-write restart and journal/WAL recovery | Incomplete | Incomplete |

Neither reached the prospectively frozen four-of-five threshold. Both also lost
supported facts returned by the earlier automatic baseline. An unsupported absent
measurement is not a generated abstaining answer. The failure remains preserved;
do not retune clauses or replay selected failures to recast it as acceptance.

All **32 anchor reads duplicated their already returned primary search excerpts**.
Only prefix reads added evidence. Thus this tests the fixed prefix-plus-anchor
recipe, not genuine deeper continuation. Query decomposition also changed hit
allocation, so its regression does not isolate decomposition quality.

## Integrity, accounting and limits

All twelve arms completed mechanically in **80 public CLI calls**: sixteen
searches and 64 reads. Independent inspection verified all 34 search citation
occurrences and 64 read ranges against exact canonical UTF-8 bytes, hashes,
Source/Revision identities and Current eligibility. Original files and protected
owned files retained bytes, sizes, mtimes and membership. Only the permitted owned
derived SQLite SHM file changed. There were no retries or operational failures.

The native macOS ARM64 release executable was the same 0.2.0 candidate used for
the [identity replay](validation-selected-identity.md), SHA256
`19350962d3342bb6372397ffe986b6c772635523956e0154566006a1f6c6b699`.
The fixture contained 52 canonical files / 333,618 bytes. No provider call or
build occurred during this control.

Returned text totaled **100,342 bytes**, including search excerpts and repeated
reads; full stdout JSON totaled 277,599 bytes. Read-body allowance was at most
8,192 bytes per arm, with search and transport charged separately. The owner
interval was 2.433 seconds; summed command intervals were 1.715 seconds and the
maximum command was 0.033 seconds. These small-fixture measurements are neither
large-vault performance nor model inference time.

Planning is separate: 856 question-content bytes, 1,153 delivered query-JSON
bytes and 2,441 plan-JSON bytes. Total harness/model input, tokens, cost and planner
turnaround are unavailable. No final answer synthesis was tested. Larger search
excerpts than the original draft and the separately budgeted historical baseline
prevent attributing gains solely to reading.

## Next coherent workflow

The fresh architecture assessment chose a bounded comparison of genuine public
continuation and evidence-conditioned investigation, including final cited
answers. Freeze prompts, controller, resource accounting and independent quality
criteria before execution. Advance beyond already returned intervals; charge
search, secondary evidence and repeats. Keep discovery, evidence sufficiency,
answer completeness and citation entailment separate, and prefer the simpler
route on a tie.

This host workflow remains separate from the native 6 KB HIGH gate. Promotion
requires independent unseen lifecycle tasks spanning import, investigation,
refresh, withdrawal/history and cache-loss recovery. Normalized mode parity and
actual 25k qualification remain open. Detailed protocols, private rubric and raw
outputs are retained locally; follow the [evaluation protocol](evaluating-context.md)
for a new control rather than assuming a fresh clone includes historical inputs.
