# Graph update feasibility: development result

The proposed normalized Graph workflow is **not feasible under its frozen
256 MiB processing allowance with genuine identity-remap history**. A shared
Review receipt removes repeated payload copies, and precise receipt membership
removes false invalidation from ordinary mention bindings. Neither change closes
the complete update workflow. Graph CLI, semantic-seal, origin and replay expansion
has stopped; the prototype remains separate from production.

This is a negative development result. It does not improve default retrieval,
establish shipping performance, qualify 25K capacity or accept a 0.2.0 release.
The accepted [Page synchronization](validation-external-page-sync.md) and existing
preview remain separate.

## Actual workflow and fixed comparison

A disposable Source contains 16 distinct Assertion groups, each supported by
16 exact Evidence ranges: 256 total. Actual packet/import/resolution constructors
bind two existing Entities. The first Review accepts all Assertions, assesses
all Evidence as supporting, and produces 32 writes with one 445,340-byte receipt
carrier and small references. Complete normalized reconstruction verifies it.

The Source is genuinely refreshed into another immutable Revision and imported
again. The actual next binding constructor produces 275 writes: 16 Assertions,
256 Evidence, two Decisions and one Extraction. The paired arms use identical
canonical inputs and the same frozen proposal bytes and allocated identities.
Each arm reconstructs separate complete publications under its own evaluator
mode; readers, certificates and operation meters do not cross modes.

Three populations were declared before candidate results:

- A: no entity-remap receipt.
- B: A with four separate ordinary imports/resolutions, two bindings each.
- C: B with a genuine active Merge of an Entity with resolved Assertion and
  Mention references into the retained existing Entity, before the first Review.

A separate genuine Split checks exhaustive reference remapping, retention of both
allocated Entities and the actual resolved endpoint. It is a semantic control,
not a replacement for the full-sized update.

## Dependency loss and measured lower bounds

The existing Remap collector observes canonical binding Decisions before checking
whether they contain an entity-remap receipt. The published candidate-membership
key is equally broad. Consequently, a new ordinary binding can invalidate Remap
and its composed Review policy, requiring the old full Review carrier as a guard.

The test-only candidate uses one consistent raw-fence discriminator for complete
reconstruction, indexed membership, traced evaluation and prospective projection.
It retains malformed receipt witnesses and allocated-family checks. Introduced
classification work is charged. This is not a production parser activation or a
compatible reinterpretation of an existing publication.

The executor checks unchanged read dependencies three times per write. The
diagnostic therefore computes a mandatory lower bound:

`already charged processing + 3 × write count × unchanged dependency bytes`.

| Population | Current rules, bytes | Precise rules, bytes | Result |
| --- | ---: | ---: | --- |
| A, no remap | 701,383,414 | 137,664,840 | Candidate lower bound fits; inconclusive |
| B, ordinary history | 729,555,171 | 137,664,840 | Candidate lower bound fits; inconclusive |
| C, genuine active Merge | 1,087,691,933 | 1,211,710,247 | Both exceed 268,435,456-byte allowance |

In A and B the precise rule removes false policy invalidation and reduces unchanged
guards to 157,278 bytes. Complete final Review remains verified. In C, both modes
retain the same 1,176,238 bytes of guards and legitimately recompute Remap and
Review. Added classification costs increase the candidate's charged work; no
cache or parameter tuning followed this result. The old carrier alone requires
367,405,500 bytes across the 275-write update, already above the allowance.

Complete states, aliases, compatible families and supersession edges agree between
the paired modes. Every prospective policy output matches independently rebuilt
final-note policy output. Original/content assets, both Source revisions and WIKI
guards remain authenticated; owned canonical and asset bytes remain unchanged.
No second proposal is applied through ordinary command publication.

## Correctness evidence and limits

Native Darwin arm64 library tests used optimization level 0 and debug information
level 2. Byte lower bounds do not depend on compiler optimization; elapsed test
intervals cannot establish release speed or capacity.

The first integrated core checkpoint compiled and passed 55 checks but failed four
fixture controls. One grouped fixture correction replayed only those four, all
passing. The discriminator checkpoint compiled and passed seven new controls,
including all paired workflows and Split, while four controls encountered another
shared invalid binding fixture. Its grouped correction replayed only those four,
all passing. This is composite evidence for 70 distinct checks, not one complete
green suite or one binary running every check. Original failures remain recorded.

Receipt controls cover malformed JSON, multiple fences, resolution-only exclusion,
missing required allocated fences, duplicate identities, wrong kind/action,
noncanonical and unallocated witnesses, and synchronous mode restoration. Preserved
non-UTF8 parser failure is an explicitly adversarial internal parsed-note test.
Rejection of previously ignored wrong-kind or unallocated copies is a declared
strengthening; existing exact-write authority still governs paths.
Actual incremental fence-removal publication, moved-path authority and selected
third-hash refusal were not all demonstrated in this experiment. These remaining
controls prevent general safety or compatibility acceptance.

Only the original normalized layout ran. Complete projected FTS/old-plus-new rows,
constructor-copy accounting, sealed Graph admission, full executor usage, ordinary
CLI/replay and recovery remain unavailable. A fitting lower bound cannot pass those
gates. One required-layout counterexample suffices for the negative decision;
unchanged failed workflows were not rerun for layout symmetry.

## Decision and reproduction

Independent Astra reviews admitted the bounded experiment and validated the initial
negative guard proof. A final independent review checked the six actual paired
outcomes, matching canonical/proposal hashes, complete semantics, Source guards
and the separate four-control replay. Genuine-Merge guard checks alone require
970,396,350 bytes, independently proving the negative result. Preserve
selected-input, shared-carrier and row-preflight work,
but do not activate the Graph proposal or expand its CLI/intent/replay machinery.
Removing false dependency membership is useful evidence; it is insufficient for
production promotion without provenance migration and a complete accepted workflow.

To repeat the diagnostic, use disposable canonical inputs, actual constructors,
the full 16/256 first Review and refreshed 275-write binding proposal. Freeze the
proposal once because constructors allocate fresh identities. Rebuild complete
policy independently in each mode, compare semantic outputs before interpreting
resource differences, and retain every Source asset and policy-read guard. Keep
the 4,096-query, 64 MiB/4,096-file capture and 256 MiB processing bounds unchanged.
Do not filter guards, widen caps, duplicate Entities or divide hidden Changes.

Optional local evidence is under
`.artifacts/workflow-priority-resume-20261007/normalized-knowledge-protocol-20261008-001/implementation/`:
checkpoint-002's initial proof, checkpoint-003's fixture replay, checkpoint-004's
paired results and checkpoint-005's four-control replay. Source manifests,
executable hashes, exact commands, process-owned monotonic intervals and failed
streams are retained there. These ignored artifacts are not required to understand
the failed architecture decision.
