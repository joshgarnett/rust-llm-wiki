# Frozen Source change compatibility fixtures

The accepted v3 fixtures were emitted by the native accepted 0.2.0 candidate016 CLI, SHA256 ec1478bdd0a14f3bfc1a9d89c0eeb2c8b0f130515d2068153d81cd8739ede146. Six offline calls on a disposable vault generated initialization, normalized rebuild, Source capture, scalar refresh, Page initialization, and a staged scalar refresh. Provider calls: zero. The JSON capsules store exact canonical, retained proof, delta, journal, payload and receipt bytes as hex. The small SQLite file is the checkpointed exact base for pending compatibility replay; its WAL was empty. It is a test artifact, not a cache import facility.

The v2 capsule was independently specified from deployed commit60603e231299c206f334bd42b2f742b3ad40511f: original field order, row/proof/delta version2, scalar source_id and v1 publication-domain derivation. A Python3.9.6/blake3 1.0.8 generator encoded and checksummed these expected bytes independently of the candidate serializer. It uses compatible accepted v3 Source row values and the shared exact base database. It was not emitted by a historical v2 CLI and does not qualify an historical v2 cache or later v3 policy consistency. The staged database has schema version3, proof layout2 and revision ownership1, as required for legacy row replay.

Tests materialize only disposable copies, check literal old proof/receipt/delta bytes and hashes, and use ordinary exact apply/repeat. Passing terminal replay alone does not establish pending replay or native crash safety. Fixture regeneration allocates new IDs/times; preserve these frozen inputs.

| File | SHA256 |
|---|---|
| accepted-old-v3.json | 85919dda4fed2216c28b9b3318cfe3e1661f85c7c9d306e6a9b7665978ec34e1 |
| accepted-old-v3-pending.json | 13bf255a6a41617ddf11b54203e068e78311803a04469c067dc905e43bb08b46 |
| accepted-old-v3-base.sqlite | 014e56a13a2a1d8d7cfd5c1015305a6de52e072921a330a24526191aea712c3c |
| specified-old-v2-pending.json | a8e2cbe458a97d5abf7bac18f3a0b66a44f1acadcd3f1a6e59be375876c7acb7 |

The pending replay materializer also restores the exact accepted producer's retained SQLite sidecars. The checkpointed database header remains WAL mode (2,2); the empty WAL and 32,768-byte SHM image are frozen inputs, not synthesized recovery state. They bring the four capsules/database plus sidecar data to 994,323 bytes, below 1 MiB. Both public pending tests validate their hashes before materialization.

- `accepted-old-v3-base.sqlite-wal`: 0 bytes; SHA256 `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`; BLAKE3 `blake3:af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262`.
- `accepted-old-v3-base.sqlite-shm`: 32768 bytes; SHA256 `fd4c9fda9cd3f9ae7c962b0ddf37232294d55580e1aa165aa06129b8549389eb`; BLAKE3 `blake3:29bf5078fb9d5a3e0d4f3baa659688073bc3748bdda77638608203e606b61ee7`.
