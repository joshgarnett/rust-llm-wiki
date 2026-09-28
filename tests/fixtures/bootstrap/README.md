# Synthetic bootstrap vault

`vault/` is checked-in canonical authored knowledge. These accepted assertions are not examples of imported model proposals; importing later extraction responses must initially produce `proposed` assertions. `expected.json` provides stable IDs, complete BLAKE3 file/snapshot/quotation hashes, byte spans, authored statuses, and expected semantic outcomes.

Two people called Alex Kim have separate identities. Two organizations participate in opposite directed and negated assertions. A dated employment assertion has a half-open interval. Architecture revision 1 is immutable history; revision 2 is the current head. The long Unicode report provides independent support for the forward assertion, which also has active contradicting evidence. Withdrawing the architecture source leaves its forward assertion and dependent reviewed page current through the report; withdrawing both sources removes all current support. Unicode quotation bytes include a literal backtick fence, and quotation slices ending in newline retain an additional separator newline inside their evidence fence.

The `expected_decisions` entries describe test expectations, not persisted `decision` records or a wire format. Later work packages own extraction/decision/review packet fixtures and synthetic vectors.

Regenerate or verify locally using the repository's BLAKE3 helper:

```sh
cargo build --example fixture_hash
python3 scripts/seed-fixture.py --hash-command target/debug/examples/fixture_hash
python3 scripts/seed-fixture.py --hash-command target/debug/examples/fixture_hash --check
```

The Python script only prepares test data, uses the standard library, and performs no provider calls. Tests must copy this vault into disposable temporary directories before mutations.
