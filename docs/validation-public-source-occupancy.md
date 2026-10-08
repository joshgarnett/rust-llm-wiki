# Public Source occupancy control

The unchanged default context failed this development quality gate: **24/30 facts
and 5/10 complete tasks in the empty arm; 25/30 and 6/10 in the occupied arm**.
Independent whole-workflow judgment was **6.5/10**, below the frozen 9/10 threshold.
All ordinary import, query, citation and revision-maintenance checks passed. This
control diagnoses retrieval losses; it introduces no product change or quality gain.

## Frozen inputs and workflow

The source-only overlay contains exact rows 0–63 of BRIGHT's Stack Overflow short
documents at revision `3066d29c9651a576c8aba4832d249807b181ecae`: 64 distinct documents,
134,412 UTF-8 bytes, with no empty inputs or duplicate IDs/content. Only `id` and
`content` were decoded. Publisher questions, reasoning and relevance labels were
never imported. The slice is predominantly R reference/tutorial fragments; it is
not a representative sample of Stack Overflow posts.

The retained Parquet SHA256 is
`d54559692f925666c3c6b1d33a696a64ef324cf5aaeff9d6f4d11fba5cd5ac8b`;
the source manifest SHA256 is
`d53a747ecf454a3925e5e836641ec306a35766a502c28090a8fb08e2c9a0a40e`.
An isolated PyArrow 21.0.0 runtime decoded the inputs without changing project
dependencies. Retained notices distinguish the publisher's CC BY 4.0 card from
underlying Stack Overflow contribution licenses. Per-post author/revision
attribution remains unverified; this local evaluation does not establish
redistribution rights. See the [dataset reference](eval-datasets-quick-reference.md#bright).

Arm A was an empty vault. Arm B was a qualified copy of the previously accepted
public 1K lifecycle backup: 1,000 retained Sources, 999 current Sources, 1,000 draft
Pages, history, graph and two Cargo documentation Sources. This treatment does
not isolate Source cardinality. Copy qualification and final preservation matched
all 13,672 original-seed entries, including bytes, membership, modes and mtimes.
The original seed received no CLI command or mutation.

Both arms explicitly rebuilt normalized indexes with the same native Darwin arm64
release executable, optimization level 3/debug information 0, from source commit
`a06e296b6043c2f1c04b8c125aee4eb3ee48d533`. Executable SHA256:
`398229c96b305cc166f84cd80fb379719fbfbbfd1036bb4acafddfb114404679`.
The overlay used ordinary `source import prepare/run/status`, eight-item groups,
individual immutable Source/Revision identities and authenticated import journals.

A fresh independent critic froze 12 development questions and 30 required
propositions before native outputs: ten positive tasks and two absent controls.
Public task-list SHA256:
`d35f7fdca0f2a149760fc00f10a1caed7346ac9fdc6f00566f81566cfc004b77`.
Private proposition/span labels remained with the critic. Acceptance required at
least nine complete positive tasks in each arm, no loss of supported baseline
facts, valid citations/currentness, passing absent controls and critic >=9/10.

The frozen runner completed all 64 offline commands: eight setup/import/status,
48 interleaved default-context/preparation calls and eight maintenance calls.
Default context retained indexed-document lexical retrieval, 12,000 rendered
bytes, 3,000 estimated tokens and ten owners. No scope narrowing, provider call,
answer actor, selector, parameter tuning, Rust build or query retry occurred.
One controlled local refresh changed a quoted observation from 21 to 18 minutes;
current context/read and explicit historical read were checked in both arms.
This is a synthetic revision, not a correction to the publisher's measurement.

## Returned evidence and first observable losses

| Observation | Empty A | Occupied B |
| --- | ---: | ---: |
| Required facts supported | 24/30 | 25/30 |
| Complete positive tasks | 5/10 | 6/10 |
| Absent controls passed | 2/2 | 2/2 |
| Default warm p95, seconds | 0.065430 | 0.277503 |
| Overlay import, seconds | 21.562349 | 26.454462 |

No supported final baseline fact was lost in B; one semantic-equivalent fact was
gained. The critic accepted authoritative equivalent definitions rather than
requiring verbatim expected phrases. Both two-Source tasks failed in both arms.
Passing absent controls is not product abstention or proof of global absence.

Three failed tasks retain required support in authenticated public preparation
cards but omit it from final context: the condition preserving already unique
initial names, the warning about pivoted decomposition of a matrix that is not
non-negative definite, and executable-location semantics alongside timeout rules.
Import/storage fixes alone cannot recover these final-selection omissions.

For the two-Source string-extraction/replacement task P07, A's public preparation
contains row 8's backward-indexing example in card `c0128`, bytes 0–196, but final
context discards it. Row 43's third-character assignment is absent from A's
public Source table. B's table contains neither row 8 nor row 43. Both imported
Sources passed exact byte, Revision-owner and current-head checks in both arms.
Occupancy therefore adds a concrete preparation-admission difference, while A's
existing final omission explains why there is no final-fact regression.

Public preparation is **not the complete automatic candidate pool**: another
task's B final context returns a supporting definition absent from both displayed
menus. Consequently, missing menu owners alone do not identify the exact internal
retrieval stage. Tokenization, candidate limits, corpus statistics, identity/tie
ordering and draft/history effects remain hypotheses requiring a code trace.
The evidence establishes losses before public preparation and during final
selection; improving only owner recall or only menu selection cannot be assumed
to complete every task.

A subsequent independent trace of the committed code narrows P07: each menu
contains ten distinct captured-Source owners, exhausting the unchanged ten-hit
admission limit. Assembly expands admitted hits; menu interleaving cannot add
owners. Row 43 is therefore outside the admitted top ten in both arms, and row 8
is outside B's top ten. This particular loss occurs before passage expansion,
although other menu omissions can still occur within admitted owners. The
`str_sub` identifier survives Markdown projection and symmetric FTS tokenization;
an identifier-preservation patch has no demonstrated benefit in this case.

The independent byte audit authenticated 222 SourceRef occurrences, including 11
actually returned old-1K references, 218 passage texts and 1,920 candidate-card
spans. All matched exact UTF-8 bytes, Source/Revision ownership and BLAKE3
commitments. Pre-refresh citations were checked against their acknowledged heads;
the later refresh did not retroactively invalidate saved initial observations.
Both maintenance workflows returned the changed current fact and preserved the
exact old revision with correct eligibility labels.

## Performance and resource limits

Every query completed within 15 seconds; packets stayed within the declared
byte/token budgets. Warm p95 uses nearest rank and excludes only each arm's first
task. Combined-arm default warm p95 was 0.234316 seconds; it differs from the
per-arm aggregates above. Timings are single observations, not a causal
cardinality experiment, throughput distribution or shipping latency guarantee.

Known charged local owning intervals total 137.347002 seconds, including corpus
preparation, runtime acquisition, copy, native sequence and final seed check once
each. Unmeasured preparation and critic/model turnaround remain unavailable.
Retained native streams total 2,594,430 bytes. Conservative serial-child maximum
plus controller maximum RSS was 201,719,808 bytes, not a per-command or general
descendant-tree peak measurement.

There were 253 resource observations: maximum observed charged allocation
1,296,723,968 bytes, minimum host free space 129,905,147,904 bytes and maximum
observation gap 0.988481 seconds. Five vanished owned-vault entries were counted
under the admitted non-atomic metadata census policy; canonical/input/citation
checks stayed strict. This passes the declared sampled limits, not continuous
peak qualification. The critic's final artifacts used 110,592 allocated bytes
plus a 16 KiB final-write reserve, within their separate fixed 32 MiB allowance.

## Decision and remaining gates

Seal this control without rerunning it or changing its questions. The next
architectural question is the exact owner-admission/candidate-pool boundary for
P07, while retaining the other tasks' complementary-selection failures. A fresh
review must choose a bounded discriminating experiment before implementation;
these results do not justify another lexical-weight or host-selector loop.
The separately frozen remote-embedding trial remains subject to its existing
specific outbound-payload approval. Coarse owner embeddings alone are not proof
of complete final evidence.

Historical native 9/28 facts–2/10 tasks and shipping-profile 13/28–3/10 remain
separate and unchanged. This small public development slice does not evaluate
the upstream BRIGHT query tracks, supply a maintained sufficient-span adapter,
qualify unseen native HIGH, demonstrate live-provider compatibility, establish
25K/~2.5 GB capacity or close the full 0.2.0 release.

Optional local evidence is under
`.artifacts/workflow-priority-resume-20261007/next-priorities-after-page-20261008-001/`:
the source manifest, public task freeze, pilot runner/GO/summary, seed-preservation
receipt and independent public `critic/ACCEPTANCE.{md,json}`. Frozen runner SHA256
is `4300350cca43344386c626dde2f8d47dcdddf09fce8044dec744de6eabf3d633`;
native summary SHA256 is
`75a9fdddab5018598c62fc60c42faa1d16eaf23c3f7c2ad6a13e782caf1f86bf`.
These ignored artifacts preserve the actual commands and failures; this tracked
summary remains readable without them.
