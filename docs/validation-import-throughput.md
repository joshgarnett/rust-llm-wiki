# Import throughput development control

Import mechanics passed independent scoped assessment at **9.3/10**, with no
observed correctness blocker. **Throughput advancement failed.** The eight-arm
control does not qualify 25k capacity or change importer defaults.

The native macOS ARM64 0.2.0 candidate used release optimization level 3;
executable SHA256
`19350962d3342bb6372397ffe986b6c772635523956e0154566006a1f6c6b699`.
No build, provider call or production vault was involved during measurement.

## Controlled comparison

The existing public importer prepared and ran 64 deterministic inputs per arm.
Four- and eight-item groups were compared for 1 KiB and 100 KiB documents on
isolated copies of small and occupied normalized seeds. The occupied seed came
from the [1k Sources / 1k Pages checkpoint](validation-capacity-workflow.md).
Paired cells used identical absolute input lists and manifests; execution order
was fixed and counterbalanced. There were 512 fresh captures and 44 successful
public commands across eight arms.

Rates include preparation and import run time:

| Seed | Input size | Four-item group, items/s | Eight-item group, items/s |
| --- | ---: | ---: | ---: |
| Small | 1 KiB | 3.104 | 3.438 |
| Small | 100 KiB | 3.059 | 3.303 |
| Occupied | 1 KiB | 0.901 | 1.128 |
| Occupied | 100 KiB | 0.907 | 1.108 |

The prospective advancement conditions required the occupied 100 KiB eight-item
arm to achieve at least 2.0 items/s and at least 1.25 times the four-item rate.
Its actual rate was **1.108 items/s**, with a **1.222 times** ratio. Both failed.
The 1.736 items/s arithmetic needed for 25k imports within four hours is not a
completed capacity observation or a replacement threshold.

## Correctness and limits

All 512 distinct Source/Revision pairs and 96 group Changes passed ownership
checks. All 1,024 original/extracted payloads matched their inputs. Immediate
search, context and read succeeded; the critic independently verified eight
search citations, twelve context citations and eight selected reads. Protected
existing arm, original seed, owned seed and backup bytes and mtimes were retained.

The owner completed in 419.607 seconds, including copies, hashing and supervision,
within a 24-minute aggregate limit. Each command had a 180-second ceiling;
allocation stayed within a 100 GiB account and 32 GiB free-space floor. Native
import main-process peak RSS was approximately 20–51 MiB. Process-tree sampling
does not establish an exact peak. Filesystem barrier counts, SQLite VM work and
complete immutable-read volume are unavailable.

An earlier account failed during setup because the sandbox denied its process
sampler. Its initial command was killed and all eight arms remained unrun. The
original 11,794 files were independently unchanged. A separately approved fresh
account retained identical executable, protocol and driver pins after correcting
host sampling access. The failed account remains preserved: there were **45
total CLI attempts**, including that setup failure.

These are single observations. Copy warming, OS caches and seed migration history
limit causal claims. Increasing input size 100 times barely affected occupied
run time, while occupied-seed CPU work grew substantially. This does not establish
that catalog occupancy alone caused the slowdown.

## Next measured decision

Source inspection found repeated activation validation through importer path
checks and retained Change resolution. Each validation reads, hashes and
strict-decodes the original migration Epoch. The occupied seed's plan is 647,457
bytes versus 1,691 bytes for the small seed; migration-history complexity is
confounded with occupancy. This is a hypothesis, not a measured phase attribution.

Use bounded observational profiling before a storage change. If repeated
activation explains the excess, assess root-bound operation-scoped validation
reuse with final dependency rechecks. Preserve tamper, missing-plan, wrong-root,
concurrent-change and recovery refusals. Do not add a persistent cache or rewrite
storage from these rates alone.

Detailed pinned drivers and raw evidence remain local. This summary records a
historical development control; a fresh checkout can exercise the public
[import workflow](source-imports.md), but does not contain those frozen seeds.
