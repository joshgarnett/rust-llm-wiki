# Reproduce the normalized full-check resource control

Run from the repository root on native macOS. Read the [frozen protocol](full-check-resource-protocol.json), version 2, and [build prerequisites](builds.md). This is an offline developer diagnostic using disposable fixtures, not an import or 100k qualification. Python must already provide `blake3`; the scripts install nothing and invoke no providers. Serialize builds, exports and benchmarks.

Keep binaries, provenance, exports, overlays and retained attempts in one existing, canonical, non-symlink account. Use new output names on every attempt; failed artifacts remain in disk accounting. These examples use `/absolute/account`; replace it with a real absolute path before running.

```sh
set -eu
CHECK_ACCOUNT=/absolute/account
mkdir -m 700 "$CHECK_ACCOUNT/bin"
python3 scripts/benchmark_full_check.py --help
```

Freeze release binaries and actual compiler evidence before fixture work. `--config=release` selects Bazel `opt`; filenames do not establish optimization. Capture the source map before and after building, using this function both times:

```sh
check_source_pins() {
  python3 - "$1" <<'PY'
import hashlib, json, pathlib, sys
p = pathlib.Path('.')
patterns = ['src/**/*.rs', 'test_support/**/*.rs', 'schemas/**/*.json',
            'tests/**/*', 'bazel/*.bzl', 'Cargo.toml', 'Cargo.lock',
            'BUILD.bazel', 'scripts/benchmark_source_refresh.py']
files = sorted({f for pattern in patterns for f in p.glob(pattern) if f.is_file()})
pins = {f.as_posix(): hashlib.sha256(f.read_bytes()).hexdigest() for f in files}
with open(sys.argv[1], 'x') as out:
    json.dump(pins, out, indent=2, sort_keys=True); out.write('\n')
PY
}
check_source_pins "$CHECK_ACCOUNT/bin/source-pins-before.json"
python3 scripts/bazel.py -- build --config=release //:lwiki //:unit_tests
check_source_pins "$CHECK_ACCOUNT/bin/source-pins-after.json"
cmp "$CHECK_ACCOUNT/bin/source-pins-before.json" "$CHECK_ACCOUNT/bin/source-pins-after.json"
cp bazel-bin/lwiki "$CHECK_ACCOUNT/bin/lwiki-release"
cp bazel-bin/unit_tests "$CHECK_ACCOUNT/bin/unit-tests-release"
python3 scripts/bazel.py -- aquery --config=release --nofetch --output=jsonproto \
  'mnemonic("Rustc", set(//:lwiki_lib //:lwiki //:unit_tests))' \
  > "$CHECK_ACCOUNT/bin/rust-actions.json"
```

Retain build logs and review the actual actions independently. The following creates the required provenance envelope, using the same hash definitions as the control: executable and source-map hashes cover **file bytes**; each action hash covers its **entire** parsed action dictionary serialized as `json.dumps(action, sort_keys=True, separators=(',', ':')).encode()`. Do not hash only selected flags or hash a reformatted source map.

```sh
python3 - "$CHECK_ACCOUNT" <<'PY'
import hashlib, json, pathlib, sys
b = pathlib.Path(sys.argv[1]) / 'bin'
sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
q = json.loads((b / 'rust-actions.json').read_text())
targets = {t['id']: t['label'] for t in q['targets']}
configs = {c['id']: c['mnemonic'] for c in q['configuration']}
actions = []
for a in q['actions']:
    assert a['mnemonic'] == 'Rustc'
    assert configs[a['configurationId']] == 'darwin_arm64-opt'
    assert [x for x in a['arguments'] if 'opt-level=' in x] == ['--codegen=opt-level=3']
    actions.append({'target': targets[a['targetId']], 'opt_level': '3',
        'action_sha256': hashlib.sha256(json.dumps(a, sort_keys=True,
                            separators=(',', ':')).encode()).hexdigest()})
assert len(actions) == 3
assert {a['target'] for a in actions} == {'//:lwiki_lib', '//:lwiki', '//:unit_tests'}
v = {'version': 1, 'kind': 'full-check-build-provenance',
     'compilation_mode': 'opt', 'rust_opt_level': '3',
     'binary_sha256': sha(b / 'lwiki-release'),
     'holder_sha256': sha(b / 'unit-tests-release'),
     'source_pins_sha256': sha(b / 'source-pins-before.json'), 'rust_actions': actions,
     'compiler_actions_path': str(b / 'rust-actions.json'),
     'compiler_actions_sha256': sha(b / 'rust-actions.json'),
     'source_pins_path': str(b / 'source-pins-before.json')}
with (b / 'build-provenance.json').open('x') as out:
    json.dump(v, out, indent=2, sort_keys=True); out.write('\n')
PY
CHECK_CLI_SHA=$(shasum -a 256 "$CHECK_ACCOUNT/bin/lwiki-release" | awk '{print $1}')
CHECK_UNIT_SHA=$(shasum -a 256 "$CHECK_ACCOUNT/bin/unit-tests-release" | awk '{print $1}')
```

This action example matches the Apple Silicon configuration used by the frozen control. Another host configuration needs explicit review. Optional top-level provenance fields can retain the build command and compiler version; obtain `--version --verbose` from the actual compiler executable in the action, not an ambient `rustc`. Required JSON is at most 1 MiB, regular, single-link and inside the account. The runner validates the envelope and rechecks its hash; supplied metadata alone does not prove compilation. A stable source map is selected repository-input evidence, not a complete hermetic toolchain proof.

Create closed exports and corresponding accepted G8 overlays with the tracked tools. Both commands preserve failures; their own limits also apply. The overlay prerequisite deliberately runs the separate general-query diagnostic. Its private setup cache is **excluded** from full-check input: only the 133 individually hash-bound canonical overlay files are copied, and public rebuild performs all derivation in the new vault.

```sh
for CHECK_TIER in 1000 10000; do
  python3 scripts/export_refresh_fixture.py \
    --unit-test-binary "$CHECK_ACCOUNT/bin/unit-tests-release" \
    --binary-sha256 "$CHECK_UNIT_SHA" --source-count "$CHECK_TIER" \
    --seed 731 --bytes-per-source 100000 --export-seconds 1800 --max-disk-gib 8 \
    --workdir "$CHECK_ACCOUNT/export-$CHECK_TIER"
  python3 scripts/benchmark_general_queries.py \
    --binary "$CHECK_ACCOUNT/bin/lwiki-release" --binary-sha256 "$CHECK_CLI_SHA" \
    --unit-binary "$CHECK_ACCOUNT/bin/unit-tests-release" --unit-sha256 "$CHECK_UNIT_SHA" \
    --account "$CHECK_ACCOUNT" --preseed "$CHECK_ACCOUNT/export-$CHECK_TIER/fixture" \
    --workdir "$CHECK_ACCOUNT/general-$CHECK_TIER" --tier "$CHECK_TIER"
done
```

Full-check requires each G8 `report.json` to say `passed`, its `setup-result.json` to match the report, and its ownership/seed/source identities and all overlay hashes to match the corresponding export. No ignored session report is required. There is currently no tracked overlay-only preparer: if the G8 prerequisite is unavailable or fails, retain that failure and resolve the prerequisite; do not substitute a private prebuild or rewrite IDs. The frozen overlay has 127 reviewed pages, one draft, and five graph/evidence records; expected check diagnostics are empty.

Run the tiny rehearsal, then 1k, then 10k with identical binary, provenance, supervisor, validator and protocol pins. Existing completed matching reports under the account are required for each next tier. A changed pin requires a fresh prerequisite chain. All full-check workdirs must be new direct children of the account and contain spaces.

```sh
check_control() {
  python3 scripts/benchmark_full_check.py \
    --binary "$CHECK_ACCOUNT/bin/lwiki-release" --binary-sha256 "$CHECK_CLI_SHA" \
    --holder-binary "$CHECK_ACCOUNT/bin/unit-tests-release" --holder-sha256 "$CHECK_UNIT_SHA" \
    --build-provenance "$CHECK_ACCOUNT/bin/build-provenance.json" --account "$CHECK_ACCOUNT" \
    --preseed "$CHECK_ACCOUNT/export-$2/fixture" --overlay "$CHECK_ACCOUNT/general-$2" \
    --workdir "$CHECK_ACCOUNT/full check $1 001" --tier "$1"
}
check_control tiny 1000
check_control 1000 1000
check_control 10000 10000
```

Tiny copies the first two sources of the validated 1k export plus its unchanged overlay; it is a subset rehearsal. Each full tier copies all matching canonical sources. The runner never opens original seed SQLite through SQLite, copies derived state, purges caches or calls providers. It performs two public normalized rebuilds around a real held QuerySnapshot, then full check/cited-query/authored-ID pairs before and after a managed second-source refresh. Holder release reuses its original transaction with an explicitly test-only final probe budget.

Read `report.json`, raw command outputs and holder evidence, including every failure. Limits remain the tracked protocol's 100 GiB account, 32 GiB free floor, 8 GiB bulk/1 GiB query RSS, 30-minute rebuild/check-pair bounds and four-hour tier deadline. Forecast admission can refuse 10k after 1k. Per-command TMPDIR/SQLITE_TMPDIR stays inside the account; only cache/temp/output/RSS/free space are sampled during calls. Full inventories and pins stay outside the check-to-cited interval. Tiny uses 50 ms samples to prove actual scratch placement; full tiers use one second. Sampled physical scratch and returned logical bytes remain separate. Monitoring affects elapsed time, transient peaks can be missed, and these controls establish no statistical tail, cold-cache, broad history, all-mode, unseen-answer or 100k result.
