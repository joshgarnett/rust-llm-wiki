# Evidence allocation development control

The distinct-evidence allocation experiment **failed its progression gate**.
It does not change public context behavior or qualify retrieval completeness.
The existing 0.2.0 candidate and the separate [verified-search acceptance](validation-verified-search.md)
remain available. The original automatic HIGH and 25k capacity gates remain open.

## Question and controlled comparison

Required evidence was present in retrieved sources but omitted from final context.
The experiment tested whether admitting distinct original paragraphs and complete
structures before adding surrounding text would fix that loss. It reused native
retrieval scores, exact source coordinates, citations and final admission rules.
No additional embedding acquisition, model selector, database or graph stage ran.

Six exposed development questions comprised five positive tasks and one absent
fact. They covered CLI errors/testing, how-to documentation, Rust panic behavior
and SQLite backup/corruption guidance. Expected facts remained outside indexed
content and automatic selection. These questions are development data, not an
unseen holdout or a representative large-vault sample.

Four arms used identical sources and discovered owners within each profile:

| Arm | Evidence selection |
| --- | --- |
| B | Existing automatic lexical selection |
| L | Distinct lexical evidence cores, then optional parent expansion |
| S | Existing semantic-unit selection using complete cached unit affinities |
| LS | Distinct cores using those same semantic-unit scores |

The HIGH profile used five owners, 6,000 rendered bytes / 1,500 estimated tokens
and a five-second proof deadline. Actual CLI defaults used ten owners,
12,000 / 3,000 and two seconds. Both used 80 discovery candidates and 1,024-byte
excerpts. Six cache preflights verified both profiles before any quality output;
all selected owners had valid cached unit coverage. There were 48 outcomes and
five separately labeled critic-selected diagnostic replays, with no retries.

## Actual results

| Profile | B | L | S | LS |
| --- | --- | --- | --- | --- |
| HIGH complete positive tasks | 0/5 | 0/5 | 2/5 | 2/5 |
| Default complete positive tasks | 0/5 | 0/5 | 2/5 | 2/5 |

S and LS recovered actionable error guidance and the panic behavior contrast.
Every arm still omitted injected-writer testing and failed-write companion-file
guidance. S/LS also lost the nonlinear-route fact that B retained. Thus no arm
passed the prospectively declared four-of-five threshold, gain over the prior
two-of-five full-pool result, and no required-proposition loss condition. The
absent measurement remained unsupported in every arm; neighboring passages are
not an explicit abstention.

The critic selected feasible evidence for all five positive tasks and the native
renderer retained it within the HIGH budget. This was a mixed-arm diagnostic:
one task required L because LS had excluded necessary cores at its retention cap.
It establishes available evidence and budget feasibility, not a five-of-five
automatic algorithm or LS candidate-retention pass. Do not tune the cap using
those critic choices or promote oracle coordinates into selection policy.

## Correctness and performance limits

The independent audit recomputed all **329/329 citation occurrences** across the
48 outcomes and five replays, checking canonical text, ranges, source/revision
identity and Blake3 hashes. Rendered byte/token, excerpt, owner and proof bounds
passed. Owned and original frozen vault/cache bytes were unchanged.

All 46 focused native correctness checks passed after correcting one fixture
that omitted its required vault marker. Only that failed check was rerun; the
other 45 passing checks were reused. Coverage includes incremental/native
admission agreement, exact budget boundaries, structural clipping, equal-content
revision identities, refresh/withdrawal refusal, semantic receipt completeness,
segmented renderer parity and existing corrupt-vector rejection.

The native macOS ARM64 executable used release optimization level 3. The maximum
arm coordinator interval was 77.30 ms. Semantic preflight plus arm intervals
were at most 102.03 ms HIGH and 97.33 ms default. Those sums are conservative,
noncontiguous development diagnostics with duplicate proof work; they are not
shipping semantic latency or a speedup claim. The fixture contained only 52
canonical files / 333,618 bytes and cannot establish 25k performance. Structural
and effective-token bytes are bounded separately; repeated original-prefix
parsing is reported and deadline-bounded, not covered by a total 4 MiB CPU-work
guarantee.

## Disposition and reproduction boundary

Do not integrate L/LS or continue local cap/weight tuning. Preserve the failed
control and return to an integrated question-to-cited-evidence workflow. A cached
semantic-unit adapter can improve normalized mode parity, but the observed
two-of-five result and regression do not establish completeness. Public commands,
ordinary source-reading continuation, update behavior and resource qualification
still need their own acceptance.

## Subsequent preimplementation support gate

A fresh architecture review tested a proposed lexical clause-coverage objective
before writing another allocator. The independent critic froze a contrastive veto
and inspected 312 correctly bound retained candidate rows on the first two exposed
development tasks. On the output-testing task, a test-harness display passage
covered a strict superset of the writer-injection method's lexical features and
cost 560 fewer standalone rendered bytes. It did not explain how to test CLI output
without spawning a subprocess. The proposed feature representation therefore failed
its frozen support-discrimination gate. No solver, product change, provider call or
native quality run followed; this is not a claim about the optimum complete union.

The retained traces preserve exact candidate text and coordinates, but already
follow per-owner retention and parent collapse. They cannot establish that all
original alternatives survived. This distinguishes two problems: preserving
evidence and recognizing whether it supplies the requested method or prerequisite.
The accepted [source-reading workflow](validation-source-reading.md) remains useful,
while automatic context, query parity and native HIGH remain open. Further lexical
weight/cap tuning is deferred; a new support mechanism needs a prospective
support-versus-distractor experiment before allocation or unseen acceptance.

The six correctness groups live in
`src/retrieval/indexed_documents_location_experiment.rs`. After building the native
`//:unit_tests` release target as described in [the build guide](builds.md), run:

```sh
bazel-bin/unit_tests \
  retrieval::indexed_documents::tests::location_experiment::tests:: \
  --test-threads=1
```

The ignored `frozen_location_action` accepts one explicit local control file;
it does not run by default or expose a production mode. Detailed executable,
input, protocol and output pins are local execution evidence. Those assets are
not distributed with a fresh checkout, so these unit groups reproduce mechanics,
not the historical six-question score. Follow the [context evaluation protocol](evaluating-context.md)
for a new prospectively frozen quality run.
