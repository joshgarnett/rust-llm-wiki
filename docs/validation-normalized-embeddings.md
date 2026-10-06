# Normalized embedding workflow checkpoint

Current source connects bounded embedding preparation, cached semantic/hybrid
document search and cited document context on an explicitly activated normalized
catalog. The affected native correctness checks pass. Independent public-task
acceptance, realistic evidence completeness and 25K capacity remain open.
This checkpoint is newer than the packaged 0.2.0 candidate010; that archive does
not contain these changes.

## Implemented workflow

Preparation uses exact authenticated Current Page, unmanaged-note and captured
document inputs. Identical formatted inputs share vectors while retaining every
owner's read guards. Own operational publications and unrelated updates do not
change an owner's embedding identity. Source refresh, withdrawal or changed
rendered input invalidates the affected membership. Selected inputs are checked
before dispatch, on received-output recovery and before membership commit.

`embeddings sync` uses the configured remote provider to acquire missing inputs.
An offline sync uses only the retained active space and matching preparation
settings, without loading private provider configuration or resuming accounting
jobs. It reports actual missing coverage; it cannot recreate lost vectors. Empty
or dimensionless preparation does not replace an established active space.

Current document search supports semantic and hybrid modes. It uses compatible
retained vectors, exact rendered input hashes and filters before dense selection.
Plain search is cached discovery. `--verify-selected` authenticates the displayed
owners, attaches exact captured-source citations and rechecks selected dependencies
before returning. Automatic `indexed-documents` context shares the existing
bounded passage allocation and selected proof. Query vectors must be compatible
with the retained active space; offline misses refuse explicitly unless the user
selected the existing lexical fallback.

For a prepared vault with cached query vectors:

```sh
lwiki --wiki /path/to/wiki --offline search 'release checklist' --mode semantic --verify-selected --no-sync
lwiki --wiki /path/to/wiki --offline context 'release checklist' --mode hybrid --scope indexed-documents
```

Retained job publication uses the normalized version-3 Run/RunEvent path without
constructing a legacy whole-vault projection. Never-applied candidates can rebase
only with exact original target and input agreement; acknowledged candidates
retain their original identity. Received-output recovery preserves paid bytes,
no-resend behavior and unknown accounting holds. Missing named proof authority
refuses bound continuation instead of searching unrelated history. Status can
still report accounting with an explicit authority warning.

## Validation and its limits

The native macOS ARM64 release checkpoint uses Rust edition 2024 and compiler
optimization level 3. Across retained evidence, **98 distinct affected checks
pass**: 62 selected unit/workflow tests, 28 semantic integration tests and eight
normalized document CLI tests. The nine embedding workflow tests cover reuse,
offline/cache misses, exact cited semantic and hybrid results, filtering,
refresh/addition/withdrawal, received stale-input recovery, commit rollback,
cursor binding and vector-read limits. Providers in these checks are local mocks.

Original compiler and harness failures remain preserved. Grouped fixes addressed
exact JobBatch fact admission, proof-deadline checks and invalid test fixtures.
A recorder filter error omitted eight workflows from one replay; those eight were
run separately. Five final adversarial fixture corrections were replayed together.
Unchanged passing checks were reused. This is an affected-check checkpoint,
not a full repository suite or proof of live-provider compatibility.

Fresh independent Astra reviews covered the job admission/rebase invariants,
semantic coordination and the five runtime fixture failures. These source and
component reviews do not substitute for the declared public-command assessment.
Its real-vector tasks and recovery/resource observations remain unrun; unavailable
public fault cuts and total preparation-work counters keep the full gate open.
The earlier [dense development comparison](validation-native-dense-context.md)
still failed its quality gate. No new ranking or allocation quality improvement is
claimed here, and the unseen native HIGH questions remain isolated.

## Capacity boundary and next milestone

Preparation currently retains at most 4,096 owners. Exact semantic discovery
renders the eligible corpus twice under a shared 64 MiB/65,536-unit allowance.
Those limits alone prevent qualification of 25,000 documents / roughly 2.5 GB;
they are source constraints, not measured throughput. Whole-preparation work
must not be described as proportional only to changed owners.

After observing the frozen small public workflow, the next connected milestone is
changed-document preparation, cited query and single-document refresh/reprepare.
A bounded release comparison will test winner-only second-pass rendering against
a compact rebuildable unit inventory with paged changed-owner preparation. It must
retain exact coverage, evidence, freshness and accounting behavior. ANN, wider
limits and another repair subsystem are not justified by current evidence.
Representative capacity, default activation, full command parity and native HIGH
remain separate acceptance gates.
