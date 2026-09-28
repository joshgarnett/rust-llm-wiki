# P03 preparatory invariant analysis

Date: 2026-09-28. Read-only source analysis by the assigned storage specialist; only this report was written. P01/P02 remain prerequisites, not accepted by this analysis. No implementation, Cargo commands, native crash tests, or live-provider work was performed.

Sources: repository AGENTS; execution README/STATE and P03 work package; storage ownership, capture, SQLite publication and recoverable-changeset contracts; CLI mutation/dry-run; handoff module/publication boundary; current domain primitives and change-record schema. P02 interface is the orchestrator's proposal, not inspected completed code.

## Required semantics

1. One root-bound writer permit spans origin lookup/preparation, each apply/recovery operation, and publication. Never reacquire this lock inside a publisher. Dry-run uses read-only planning and cannot acquire a lock by creating its lockfile or directories.
2. Prepared payloads, including exact binary before/proposed bytes, become durable before a Prepared event. An append-only operational journal authorizes state transitions; payload availability alone does not authorize apply. Every manifest/payload must be hash-verified when loaded, including recovery/rollback/origin lookup.
3. A started apply preflights **all** targets and the complete proposed graph before any canonical write, then rechecks each target immediately before replacement. Recheck graph/read dependencies before publication. Noncooperating edits remain possible after checks; do not claim filesystem compare-and-swap.
4. Recovery inspects all targets even if operation-complete flags exist. Third hashes or unexpected absence preserve bytes and prevent publication. Record a conflict with path, expected old/new states, observed state, and interrupted phase. Conflict is not automatically cleared by a subsequent recover.
5. Terminal committed/aborted changes are historical records, not work to replay whenever their original targets subsequently change. Only unresolved applications block fresh publication. This distinction is essential when later changes legitimately edit the same path.

## Small shared interface

Reuse `RecordId`, `Blake3Hash`, `VaultRelativePath`, `ReadSnapshot`, `WikiError`, and P02 `ExpectedState::{Absent, Hash}`; do not create parallel representations. Proposed field names below are a root-owned API recommendation, not existing APIs.

```rust
struct PayloadRef { path: VaultRelativePath, hash: Blake3Hash, byte_len: u64 }
struct ChangeOrigin { operation: GraphImport, packet_id: RecordId,
                      response_hash: Blake3Hash }
struct ChangeOp {
    target: VaultRelativePath,
    before: ExpectedState, after: ExpectedState,
    before_payload: Option<PayloadRef>, after_payload: Option<PayloadRef>,
    role: MutableRecordOrImmutableRevisionAsset,
    apply_after: Vec<usize>, // indices in the immutable, target-sorted manifest
}
struct ChangeManifest {
    version: u32, vault_id: RecordId, change_id: RecordId,
    origin: Option<ChangeOrigin>, inverse_of: Option<RecordId>,
    allocated_ids: BTreeMap<String, RecordId>,
    operations: Vec<ChangeOp>,
}
struct PreparedChange { change_id: RecordId, manifest_hash: Blake3Hash }
struct JournalFrame { version: u32, sequence: u64, manifest_hash: Blake3Hash,
                      event: ChangeEvent }
enum ChangeEvent {
    Prepared, Applying, Intent { op: usize }, Done { op: usize },
    FilesApplied, Indexed { snapshot: ReadSnapshot },
    Committed, Aborted, Conflict { /* phase + observations */ },
}
```

Use a bounded length prefix plus checksum over serialized frame bytes, with contiguous sequence validation and legal state transitions. Protect the fixed header/length with its own checksum: otherwise a corrupted interior length can masquerade as an incomplete trailing payload. The first frame binds change/vault identity through the verified manifest. The manifest hash covers a precisely specified immutable JSON serialization (or exact fenced bytes), not its mutable note envelope/outcome. Freeze that encoding in fixtures. Reject duplicate JSON keys, duplicate/overlapping target operations, cycles, payload paths outside this change, mismatching payload hashes/lengths, and unexpected manifest fields/version. Drop identical old/new no-ops during preparation so classification is unambiguous. Payload names must not derive unchecked filesystem paths from user input. Caller operations cannot target `.wiki`, retained changeset internals or the engine's own staging/journal files.

`apply_after` preserves target-sorted manifests while expressing immutable revision files before source-head update and rename destination before source deletion. A deterministic topological application order is sufficient. Rename lowers to guarded create/delete and retains both payloads; overwriting its destination requires a separately explicit expected version. Cross-operation portable case collisions need preflight validation. Conservative rejection of case-only renames is safer than undocumented intermediate overwrites.

Minimal public operations: read-only `plan`/`inspect`; `prepare_or_reuse(permit, origin, policy, build_once)`; `apply(permit, prepared, validator, coordinator)`; `recover(permit, validator, coordinator)`; `abort(permit, id)`; `prepare_inverse(permit, id, validator)`. `build_once` allocates IDs only after locked canonical origin search finds no matching change. Exact packet/response matches reuse the existing change; a different response for the same packet conflicts unless policy explicitly requests a separate extraction. Duplicate matching origins or corrupt matching manifests fail closed. Returned existing outcomes include prepared/conflict/aborted/committed status; do not silently resurrect an aborted origin or allocate replacement IDs.

The validator receives the full current canonical scan plus proposed overlay and returns validated read dependencies/fingerprint, not a caller-supplied boolean. It must cover records **outside** the changed paths (e.g. evidence membership). A coordinator-owned, privately constructible `PublicationPermit<'lock>` binds vault/held lock, fully-applied change+manifest (or clean sync), verified scan hash and parser fingerprint. Catalog receives this permit and verified scan; it does not decide journal safety. `publish(...) -> ReadSnapshot` can rebuild when already published. No public no-op validator/publisher may activate graph changes; mocks are test-only. Until P05 wiring, unavailable validation/publication must fail closed. Avoid starting canonical mutations when required publisher capability is known unavailable.

## Replay classification and intent loss

| Operation | Old/pending | New/applied | Conflict |
|---|---|---|---|
| Create | absent | new hash | any other present hash |
| Replace | old hash | new hash | absent or third hash |
| Delete | old hash | absent | another present hash |

Read errors, directories, symlinks and failed containment are errors, never absence. All classifiable states are necessary but not sufficient to resume. A verified Applying event is the change-wide authorization; an Intent event records the per-operation boundary. If Applying survives but Intent does not, append a new Intent before a pending replacement. If a target already matches new bytes, sync relevant file/directory and record observed completion after verification. Never trust a missing completion flag as proof replacement did not occur.

With operational journal loss, retained payloads/manifest recover identity and planned operations. All-old remains staged (or may be aborted explicitly); mixed/all-new without trustworthy apply intent is ambiguous and blocks automatic completion. Exact proposed bytes could have been written independently. A durable verified outcome can reconstruct terminal status, but a mere editable `wiki_status: committed` string cannot by itself prove its manifest/outcome binding. The simplest implementation needs no second intent journal: report ambiguity and require explicit `changes apply` to establish fresh intent after complete validation. Unknown/third-hash states still require explicit repair or a separately prepared change, never force overwrite. If fully automatic resume after complete journal loss is desired later, persist a separately hash-bound intent receipt under retained `changes/<id>/` before canonical mutation; it is additional format, not required to infer intent from hashes.

An incomplete trailing prefix/payload/checksum may be ignored only as an incomplete final frame. A complete bad checksum, invalid sequence/state, oversized length or malformed frame is corruption, including at EOF. Never scan ahead to salvage later frames. Under the lock, truncate an accepted incomplete tail to the last valid offset and sync before append; otherwise later recovery encounters a corrupt interior. Preserve diagnosable corruption; do not silently recreate its journal.

## Durability order and publication

Prepare directories/payloads → sync files → sync newly created directories and their parents → persist/sync manifest note → durable Prepared journal (including parent directory when newly created). Apply: durable Applying → stage/write/file-sync → durable Intent → final expected-hash check → replace/remove → containing-directory sync → durable Done. Stage creation needs directory sync if the intent expects that temp name to survive; alternatively recovery can always restage from retained payloads and must not require surviving temps.

Recovery observing new bytes after a crash between rename and directory sync must sync the relevant directory before Done; observed presence is not proof of durability. Every sync/append failure stops progression. A replacement can have happened even when its API returns a subsequent directory-sync error: recovery classifies bytes. Avoid an error path that deletes a staged file already moved to the target. New ancestor directory entries need their own parent syncs.

After all targets are verified at proposed state and whole graph/dependencies revalidated: durable FilesApplied → build/recheck scan under held lock → SQLite transaction publishes ordinary generation+both FTS sets+published pointer → durable Indexed(snapshot) → expected-hash finalization and sync of outcome note → durable Committed. A crash after SQLite commit but before Indexed causes idempotent rebuild/publication, not inverse file writes. Generation numbers need not remain identical; projected graph equivalence must. Any other unresolved apply blocks the permit, even if this change is fully applied. Abort cannot cancel an Applying change.

Keep `changes/**`, all retained payload copies, operational journals, temps, and outcome notes out of semantic/control-manifest inputs. Otherwise outcome finalization invalidates the snapshot it describes. Keep payloads and immutable manifest separately hash-bound so finalizing status never changes proposal identity. A third edit to the outcome note itself must not be blindly overwritten; stop with an explicit bookkeeping conflict. Old SQL read transactions retain their old view; P03 mock publication cannot prove P05 transaction isolation.

## Immutable assets and rollback

Enforce immutable revision paths even if the source record is absent/deleted. Caller-selected operation role alone cannot grant replacement/deletion authority. Recognize `sources/<source>/revisions/<revision>/...`, verify owning manifests, and reject replace/delete or adding content to a previously sealed revision. A new revision's complete asset set may be created across this change; interrupted retries use its recorded create expectations and hashes. Existing equal assets are reused explicitly, not overwritten. Enforce this at apply/recovery/rollback, not only in source capture's caller.

Rollback prepares a **new** inverse changeset, validated and journaled normally, guarded by each original proposed state. A deleted mutable record's inverse is create-with-absence; a created mutable record's inverse is delete-with-new-hash. Any unfamiliar current state blocks the inverse. Retain immutable captured originals/normalized bytes/revision notes and retained changeset assets even when rolling back their source-head advance; omit those asset-deletion inverses and report retained paths. The graph validator decides whether the remaining logical inverse is valid. Never translate rollback into unjournaled cleanup or abort-after-mutation.

## P02 interface requests now

- Expose bounded root-bound injectable append/write-at-offset or journal append, truncate+file-sync, explicit file-sync and directory-sync, and safe operational-file creation. P03 must not bypass the actual production DurableIo calls for journal fault tests.
- Ensure recovery can sync an already-proposed destination (or absent deleted destination's parent) without performing another replacement. Return enough path/stage information to do so safely.
- `replace/delete` must check the actual expected state immediately before mutation; later sync errors must not imply that mutation did not happen. Permit and staged-file objects must be bound to the same vault; consuming staged files should not enable reuse at another target without fresh validation.
- Recursive managed-directory creation must make each new parent entry durable. Provide intentional policy for unsupported directory fsync rather than silently treating every error as success.

## Local fault/subprocess boundary matrix

Run one deterministic failpoint before and after each production durability call, restart with a fresh process/lock, and inspect target bytes plus manifest/journal state. Injectable tests model errors/torn writes; subprocess termination demonstrates process-crash behavior only, not power-loss durability.

| Boundary family | Required cases/assertions |
|---|---|
| Preparation | mkdir/create/write/file sync/directory sync for text and binary payloads, manifest, initial journal; incomplete preparations cannot apply; missing/tampered payload fails closed |
| Journal | partial prefix/body/checksum, complete corrupt last/interior frame, bad sequence/transition, oversized frame, append/sync failure, truncate/sync failure; only incomplete final frame tolerated |
| Application | Applying; stage create/write/sync; Intent append/sync; final check; replace/remove; directory sync; Done append/sync; all-old/all-new/mixed and third hashes at every target, including first/last operation and rename pair |
| Recovery | Inject third hash or unexpected absence after each boundary; completed flag plus third hash; already-new target after failed directory sync; journal deleted with all-old/mixed/all-new; missing before/proposed asset; terminal old changes must not replay after newer changes |
| Publication | FilesApplied append/sync; unpublished SQL construction; transaction before/after commit; Indexed append/sync; outcome replace/sync; Committed append/sync; other unresolved apply blocks; scan changes retry once then conflict |
| Policy | Repeated prepare/import before apply reuses IDs; differing response conflicts unless explicit separate extraction; abort after mutation refused; inverse conflict preserves bytes; absent source cannot bypass revision immutability; rollback retains captures; dry-run creates nothing |
| Concurrency | Two subprocess CLI writers serialize; killed lock holder releases advisory lock; editor after check exposes documented observation limitation; reader keeps prior SQL view (P05 integration gate) |

P03 can establish mock coordinator ordering and failure behavior now. Actual SQLite atomic publication/snapshot assertions belong to P03/P05 integration. Windows/Linux/native power-loss claims remain separate qualification, never inferred from macOS mocks or cross-builds.
