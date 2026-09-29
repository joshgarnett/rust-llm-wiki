# P14 independent review

Status: bounded independent review and historical local forward test complete. R1 fixed and independently verified. Root subsequently implemented anchored export; Astra-P14-export-review.md records the scoped race closure and fresh native packaging/race gates. The forward artifact below remains the explicitly identified pre-anchor binary. Read-only source review; this report is the only repository write. No Cargo/Git/network/host installation/real vault/nested delegation.

## Scope and static findings

Read execution playbook/checkpoint, P14 package/plan, linked CLI packaging and agent-integration contracts, maintained skill/resources, `src/cli/skill_export.rs`, CLI argument/dispatch/module wiring and `tests/skill_export.rs`. Read skill-creator instructions for independent forward testing; no authored test is counted as executed evidence.

**R1 (closed by source fix and independent runtime refusal): incomplete package could be reused as complete.** In initial `src/cli/skill_export.rs` SHA-256 `29a792a1a86bc2039814f6e8558aaec14a0b912fa1523b5b441b62789df7b3e4`, lines 151–156 convert literal Unix backslashes to directory separators. `agents\\openai.yaml` at package root can therefore substitute for missing `agents/openai.yaml`, provided bytes match; `seen` counts entries rather than unique expected paths. Equivalent aliases can replace references or duplicate a normalized name while omitting another required resource. Root acknowledged and assigned actual component/unique-membership fix plus adversarial runtime fixture to owner. Final source uses actual Normal components, rejects literal backslash filenames, and tracks expected path membership with BTreeSet. Independent frozen-binary control replaced `agents/openai.yaml` with exact-byte root `agents\\openai.yaml`, removed the real parent directory, then attempted reuse: exit 4 `CONTENT_CONFLICT` / `separator-like package filename`, package unchanged. Artifact `/private/tmp/lwiki-p14-forward-b_ontgew/alias-regression.json`, SHA-256 `2e87a8f6c68205a332ddd89d25c92c55ab06fd1ec5c8c767bbf6188670cf09cc`. This is a post-fix refusal test, not a runtime demonstration of the original bug.

**Concurrency limit (separate root/Astra adjudication pending):** initial lines 21–78, 205–287 check ancestors by pathname, then reopen those paths for directory/file creation and verification. Concurrent rename/symlink replacement of an already checked ancestor can redirect writes or reads. `create_new` prevents overwriting a leaf; Unix `O_NOFOLLOW` protects the leaf verification read. Neither anchors every ancestor. Static symlink refusal is implemented and accurately described by worker; mutable-directory race safety has not been established.

Other observed properties: release registry and actual Clap leaves generate command reference; version/commands/schemas and BLAKE3 resource/package checksums are retained in manifest. Export dispatch occurs before vault/preference discovery; `--stage` refuses. Static existing symlinks, altered/partial/extra packages and alternate known discovery copies refuse. New resources precede SKILL.md; manifest completes last; errors retain created-path partial metadata. No whole-tree atomicity or directory barrier is claimed.

Skill guidance preserves exact revision/evidence citations, stable IDs, homonyms, contradictions, author hashes, staged materialization, full evidence review and authorization. It distinguishes host-agent advisory spend from dispatcher guarantees, remote embeddings from local storage, and unsupported/budget stops. Maintained recipe is actual CLI input with dynamic returned IDs/hashes; final exported recipe runtime evidence is recorded below.

## Initial source fingerprints (SHA-256)

- `src/cli/arguments.rs`: `96120686d9f79993ecab69fb5755840273ed14a4011ca76a8dafa5e515dc520b`
- `src/cli/dispatch.rs`: `7eeebf9fd483eed66e0bbd0289b47f10a7956450209488cba23eecd594245330`
- `src/cli/mod.rs`: `aecacd91335d99db2d26edbcd624e618c16857dd76581642c4c5febdfe5ab2ca`
- `skills/llm-wiki/SKILL.md`: `afecc17b274f35a4f2e89534e52285191ef55e0d43fe0f6556866f3baf79677f`
- `skills/llm-wiki/references/workflows.md`: `09af8d347201fe426ad2590d133de5ea01de4924be6600b83c407045ce9687e3`
- `skills/llm-wiki/references/examples.json`: `7b25f0fe5ac49bf8e8a969177b8fd76abdc38773473bff84010640d41041ad33`
- `skills/llm-wiki/agents/openai.yaml`: `e2f334d89fdf99043c8fb519347a0a27220ef93f46827a58c313e3f83635e66b`
- `tests/skill_export.rs`: `6ed42ce23d4b9431e49c993ab00389ba1db39c3219f62a333b24c34e3f211fcc`

## Final checked artifacts and gate evidence

Root supplied frozen `/private/tmp/lwiki-p14-review-artifact/lwiki`; independently verified SHA-256 `b9daf0343977221ba4a7bd1a68469eb71c32d8e7e76058f91fe7d324b5d704c2`. Read actual `/private/tmp/lwiki-p14-runtime-final.log`: all four mandatory parents passed, compile 3.78 s / execution 8.18 s; log SHA-256 `a26cddf139255e0d96ed2ae8ff7fb0c5a216f67c8619e3720bdcab51249b169d`. I did not rerun Cargo. These are observed worker/root test results, separate from the independent task.

Independently checked all 11 P14 source/package/test/CLI files against `/private/tmp/lwiki-p14-final-checked-hashes.json`, whose SHA-256 is `5ae9ed1d9d71c9dafb14355c97a7360a9a4e900f50735dfeb246e54f9fc57eb4`. Reviewed selected-map SHA-256 (Python sorted JSON encoding) `a46964f683cb1d801a9f820afdb834445fcb26bfebc0c906baaab3ba54181039`. Final changed files:

- exporter: `5bca7ae5344987a1e605bafbf763ce2b477aded6d59b60d090df675cc810892c`
- tests: `b8adb08a0207c7877918296a3a4c784708ac5b5fc6148e485ace4a6462eb3184`
- workflows: `af85bc286f2f54a08295fe5cf14d2f327815f14310ba43fd776d21b35c0a2a58`
- examples: `25d0cf5b35351a668d7450acd18b2b19c3d30dbd360b19bb3cfb03912f33dccc`
- maintained generated commands: `59c9d7def5379630fa719a12deab772d79874b6dd52c1cf43fe3947699431787`
- fixture manifest: `cef225d4205bed2b1dcdf3c608bcef5a6a66a00b653653a915dff6effb99b612`

Read final recipe/test changes: exact repeated-name mention spans, required authored page status, genuine simulated external author edit before stale-hash refusal, bounded empty partial Current context and explicit hard Snapshot budget stop, structured unavailable semantic Usage. Independent package manifest equals maintained fixture; exported generated commands equal maintained reference. Read root main machine parse-error wiring, SHA-256 `1632e80a9dfaa5a80bf95e95c7122908dfa32c2df18e155577bd01133e7d7ad2`.

Applied skill-creator validation instructions: cached read-only PyYAML plus `PYTHONDONTWRITEBYTECODE=1`, `quick_validate.py skills/llm-wiki` and then actual exported package both passed `Skill is valid!`. This proves syntax/scaffold validation only.

## Independent local forward test

Artifacts: `/private/tmp/lwiki-p14-forward-b_ontgew`; retains `request.txt`, `setup.json`, actual exported skill, `transcript.jsonl`, individual raw JSON results, `result.md`, and before/after canonical SHA-256 maps. No intended answer was supplied. Literal request:

> Find what this disposable wiki says about Tool usage. Cite the exact captured source revision and passage, distinguish maintained/used relationships, and report any evidence gap. Do not change canonical Markdown.

Fixture preparation used the actual binary to initialize a new vault, capture raw `tests/fixtures/p13/source.md` and export the skill to a separate disposable project. The subsequent task used the actual exported SKILL/workflow/generated command reference: capabilities/version compared with manifest; literal `Tool` search bounded to 5 hits/240 excerpt bytes; lexical `context uses Tool` bounded to 12000 bytes/3000 estimated tokens; relationship `graph query Tool`; follow returned captured content path by bounded byte-range read. The first range 25..61 included the previous sentence terminator; transcript preserves it, and refined read 27..61 returns exactly `Ada maintains Tool. She uses Tool.` Hash equals the cited full captured content hash. Runtime source/revision IDs and paths were taken from outputs, never invented.

Actual result distinguishes explicit maintenance by Ada from pronoun-attributed usage, describes the possible pronoun reading without asserting resolved graph identity, and reports the empty graph assertion result as an evidence gap. It cites source `source_01a0eaa1-3633-7f70-a097-d04ea3797242`, immutable revision `revision_01a0eaa1-3633-7f70-a097-d05453c7301c`, exact bytes 27..61 within the returned 0..81 passage, and captured content/quote hash `blake3:5bb6770fd9e0b75f11410d53d77467b8849e82f62900fd71eaa6fe1fc3d5f7d7`.

All 8 canonical Markdown paths remained byte-for-byte identical after the task (compared name→SHA-256 maps). Every JSON command reported `network_used:false`, successful exit and empty stderr. Context rendered 614 bytes / 154 estimated tokens within chosen bounds. Rebuildable SQLite/operational index maintenance during reads is permitted; this task does not prove a no-index-write claim. Transcript SHA-256 `eba0d0bd309f727fade8d9c4933ebae183ac59676def7a4b55476cd13eb978e8`; actual result SHA-256 `e61cc6052ffe6d3308643595acf03e5269cfb6e401c7422b8f0b3c08f2a0220a`.

Local independent retrieval/citation behavior passed. This is one realistic agent task, not host natural discovery/invocation, all possible skill behavior, live provider compatibility, native other-platform qualification, concurrent filesystem security, or power-loss durability. Host-agent spend remains unknown; no live remote work occurred. Root owns integration acceptance and separate concurrency decision.
