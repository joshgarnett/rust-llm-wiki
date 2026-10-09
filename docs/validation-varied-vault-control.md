# Varied 1K import control

On 2026-10-09, the existing native macOS 0.2.0 preview imported 1,000 distinct
documents totaling exactly 100,000,000 current-content bytes into a new
normalized vault. All imported original and extracted bytes matched their
inputs. This is a measured preparation and import control for the
[large-vault protocol](testing-large-vaults.md), not 25K capacity or retrieval
quality acceptance.

## Inputs and executable

The two public documents are the complete Cargo configuration and features
references from commit `66221abdeca2002d318fde6efff516aab091df0e`; their README
and both upstream license files were retained. The other 998 documents contain
deterministic synthetic activities, distinct identifiers and routes, Unicode,
headings, tables, code blocks, and early and distant facts. Their base sizes are
199 × 80,000, 600 × 100,000 and 199 × 120,000 bytes, with small documented
adjustments so the whole public files remain intact and the total is exact.
The templates provide structured load, with limited semantic diversity. They
do not represent a natural corpus or establish semantic retrieval quality.

[large_vault_packet.py](../scripts/large_vault_packet.py) prepares these
source-only inputs without running the wiki CLI, acquiring a corpus, or adding
questions or expected answers to the indexed content. Its `plan` command writes
a proposed packet and symbolic command arguments. `generate` requires explicit
input-generation admission and refuses counts above 1,000; `verify` checks exact
membership, deterministic hashes, UTF-8, sizes and notices. Partial outputs are
preserved rather than overwritten. Planning a 10K or 25K tier does not admit it.

This historical experiment deliberately pins executable SHA256
`398229c96b305cc166f84cd80fb379719fbfbbfd1036bb4acafddfb114404679`, from production
commit `a06e296b6043c2f1c04b8c125aee4eb3ee48d533`. It used native arm64 release
settings, optimization level 3 and debug information level 0. The local preview
is not included in a fresh checkout; the script's pin is an experiment constraint,
not a guarantee that a newly built executable has identical bytes. No Rust
rebuild was needed for this control.

## Observed import

The public commands initialized the absent vault, activated the normalized
catalog, prepared an import manifest, and imported groups of four using a stable
key and a maximum of 64 groups per invocation. One initial run and three resumes
completed the 250 groups. The completion journal covered every input ordinal
once and returned 1,000 distinct Source and Revision identities.

| Observation | Result |
|---|---:|
| Input generation and verification, before final output emission | 3.388 seconds |
| Active import commands, including final status | 318.552 seconds |
| Whole import supervisor interval, including audits | 322.171 seconds |
| Largest native CLI peak RSS | 38,731,776 bytes |
| Native supervisor peak RSS | 29,065,216 bytes |
| Allocated runtime account after import | 1,016,643,584 bytes |
| Free space after import | 124,610,670,592 bytes |
| Original/extracted sources matching input bytes | 1,000 / 1,000 |

These are owning-process monotonic durations on this host, with warm filesystem
metadata. RSS includes native high-water measurements and sampled process-tree
observations. The sampled resource guards can overshoot, and their inventory
and process-monitor overhead is included in command measurements. The runtime
account includes inputs, canonical files, retained changes, indexes and logs;
its allocation is not a forecast for 10K or 25K. The resource envelope was
3.5 GiB runtime plus 512 MiB external reserve and at least 32 GiB free space.

A subsequent read-only binding check verified every Source current head and
Revision owner and matched all 1,000 current captured index rows to the import
mapping at publication epoch 251. This verifies the frozen imported membership;
it does not establish global filesystem freshness. An attempted `index status`
command returned `USAGE` because that command does not exist; the failed attempt
was retained, and publication identity was read from the quiescent catalog.

## Validation limits

Ten affected preparation controls pass across the initial checkpoint and the
replay of two repaired tests. The initial checkpoint retained one test-fixture
error; the unchanged passes were reused. Four import-supervisor tests cover
completion, journal coverage, final pin drift and deadline failures with a fake
Runner. These checks do not demonstrate live-provider compatibility or recovery.

Retrieval, individual updates, cited Page author preservation, interruption
recovery and larger tiers require separate actual-command acceptance. The
previous failed 1,000-change/600-second gate remains mandatory for full capacity.
Default retrieval quality, semantic acceptance and full release qualification
remain open.
