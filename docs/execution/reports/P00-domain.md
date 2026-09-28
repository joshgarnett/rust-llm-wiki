# P00 domain worker report

Completed 2026-09-28 on `impl/autonomous-v1`, assigned baseline `a97679c`. Exclusive implementation paths: `src/domain/types.rs`, `src/domain/records.rs`, `tests/contracts.rs`. No shared surface, Cargo, registry, schema, state, Git, credentials, or live-provider mutation by this worker.

## Implementation

Validated `RecordId`, `Blake3Hash`, `VaultRelativePath`, and `ByteSpan` reject malformed constructors and serde inputs. IDs retain case-sensitive syntax, generate UUIDv7 by kind, and use deterministic packet hashes for extraction packets. Paths reject escape, absolute/backslash, control characters, empty/dot components, trailing dot/space, unsafe characters, and platform device names. Byte spans validate order and provide exact range/UTF-8 checked text slicing; evidence frontmatter separately requires nonempty spans.

Shared references, snapshots, eligibility, revision alias, complete CLI `ErrorCode` registry, serializable `WikiError`, and `Result<T>` are implemented. Error-code spelling is SCREAMING_SNAKE_CASE with the documented exit families.

`CanonicalRecord::new(BTreeMap<String, Value>)` and `from_value(Value)` validate all 13 record kinds, common fields, lifecycle/enumeration values, precise primitive types, local reference syntax, dates, RFC3339 UTC (`Z`/`z`/`+00:00`), BLAKE3 hashes, payload paths, navigation wikilinks, complete/failed extraction fields, packet fingerprint identity, mention actions, exact decimals, predicate/object/literal matrix, numeric units, and half-open date order. Custom deserialize reuses validation. User metadata survives unchanged; unsupported `wiki_` fields fail. `add_alias` supports entity-only decisions; mention-bound aliases require extraction context. Packet-local mention identifiers are strings, not canonical IDs. Extraction source and revision arrays are independently unique sets and may have different lengths.

Canonical accepted fixtures remain distinct from import wire proposals. Cross-record kind/ownership/existence, activation support, revision immutability, supersession cycles, body quotations, and navigation/body projection consistency remain later package responsibilities.

## Public API

Newtypes: `new`, `as_str`, `Display`, `FromStr`, `TryFrom<String>`, string conversion and validating serde. Additional methods: `RecordId::generate`, `RecordId::packet(&Blake3Hash)`; `Blake3Hash::digest`, `hex`; `ByteSpan::new`, `start`, `end`, `len`, `is_empty`, `slice`. `CanonicalRecord` provides `fields`, `into_fields`, `id`, `kind`, `title`, `field`, `string`. Typed reference/snapshot fields are public; validated primitive fields are private.

## Checks and limits

- `rustfmt --edition 2024 src/domain/types.rs src/domain/records.rs tests/contracts.rs` completed.
- `cargo test --locked --offline --test contracts` passed **4/4** tests after final refinements. Names: `id_hash_and_path_newtypes`, `contracts_valid_examples`, `contracts_invalid_types_and_references`, `predicate_literal_and_qualifier_matrix`. Valid examples cover all 13 kinds and also validate against root-owned `schemas/record-v1.json` with format checking. Malformed examples check typed errors and deserialize rejection. Regression cases cover empty evidence, entity-only/mention-bound aliases, and unknown-offset timestamps.
- `cargo clippy --locked --offline --lib -- -D warnings` passed after final changes. An earlier run found one needless lifetime, corrected before handoff.

No full-suite or native filesystem/recovery/provider checks run by this worker. Root retains integrated-suite and independent-review gates. No outstanding root-owned dependency/export delta: the integrated `domain` modules and Cargo dependencies support these files. Root schema currently requires mention fields for the three mention actions; optional follow-up: also encode extraction requirement for `add_alias` carrying mention IDs, matching production validation.

Final checked SHA-256 fingerprints:

```text
f0888798ff0921cb20e77e7513c69c5b7de89c5f053bd20b721ee29139e25fb9  src/domain/types.rs
a31ba98d025ba63e6fdb3a4812f8882ce3d11542ad0c822fda2d0978ff7f7229  src/domain/records.rs
30f5d5da6260460abf77fa298008d60239cf9d4a06015389721612bcf6b70945  tests/contracts.rs
6d9c28a348608d1962f899556d7a6eeeaf3b12ea35504f4ab30a06941d04bef5  schemas/record-v1.json
e7b4e5cefac77cb093983b5134530602961d964668b09afa0df190c68b2d088b  Cargo.lock
```
