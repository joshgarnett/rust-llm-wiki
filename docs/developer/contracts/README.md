# Implementation contracts

These contracts define durable authority and public behavior. Use the
[current contract map](../current-contracts.md) for implemented surfaces and tests,
[validation status](../validation-status.md) for qualification limits, and the
[user manual](../../user/README.md) for task instructions.

| Contract | Responsibility |
| --- | --- |
| [Storage and recovery](storage.md) | Canonical Markdown, immutable revisions, identities, retained changes and rebuildable catalog publication. |
| [Record schemas](record-schemas.md) | Flat frontmatter types, states, predicates, qualifiers and evidence spans. |
| [Retrieval and extraction](retrieval.md) | Literal/vector/graph discovery, exact support, extraction packets and bounded selection. |
| [Providers and jobs](providers-jobs.md) | Trusted endpoints/credentials, finite dispatch budgets, usage and uncertainty. |
| [CLI and skills](cli-and-skills.md) | Selection/config, command contracts, output v2, errors, human/agent workflows. |
| [Research handoffs](research-handoffs.md) | Host-owned external tools, guarded local packets, imports and retained progress. |

## Fixed invariants

One Rust executable, bundled SQLite, no required local model/database service or
daemon. Canonical knowledge and source revisions survive cache reconstruction.
Source revisions, IDs and evidence are immutable; headings are independent labels.
Model-derived assertions remain traceable to exact source/revision bytes and explicit
review. Offline/dry-run do not invoke providers or credential helpers. Preferences
cannot authorize endpoints, executable helpers, credentials or TLS changes.

Source citations use zero-based half-open UTF-8 byte ranges against the identified
captured content and named BLAKE3 hashes. A path link is navigation rather than
identity proof, and a graph path cannot establish an unstated new assertion.

Schemas and sealed application/publication authority are shared boundaries. No
caller bypasses dispatcher-managed network accounting or weakens replay/recovery
proof to admit a convenience operation. Research host usage remains unobserved by
CLI accounting. Promises about performance, live compatibility and final release
bytes require their own qualification rather than historical design text.
