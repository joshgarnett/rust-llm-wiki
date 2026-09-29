#!/usr/bin/env bash
# Local qualification only: disposable fixtures/mock providers, no publication.
set -euo pipefail

task_repo_root=$(cd "$(dirname "$0")/.." && pwd -P)
cd "$task_repo_root"
if [[ $# -gt 1 ]]; then
    printf 'Usage: %s [NEW_EVIDENCE_DIRECTORY]\n' "$0" >&2
    exit 2
fi
if [[ -n "${1:-}" ]]; then
    task_evidence_dir=$1
    mkdir -p "$(dirname "$task_evidence_dir")"
    # Refuse every existing path, including an empty directory or a symlink.
    mkdir "$task_evidence_dir"
else
    task_evidence_dir=$(mktemp -d "${TMPDIR:-/tmp}/lwiki-qualification.XXXXXX")
fi
task_evidence_dir=$(cd "$task_evidence_dir" && pwd -P)
printf 'Qualification evidence: %s\n' "$task_evidence_dir" >&2
task_stage=toolchain
trap 'task_status=$?; if [[ $task_status -ne 0 ]]; then printf "Qualification failed during %s; evidence: %s\n" "$task_stage" "$task_evidence_dir" >&2; fi' EXIT
task_host=$(python3 scripts/bazel.py --print-target)
test -n "$task_host"

task_stage=fmt
python3 scripts/bazel.py -- test --nofetch --nocache_test_results //:format > "$task_evidence_dir/fmt.log" 2>&1
task_stage=clippy
python3 scripts/bazel.py -- test --nofetch --nocache_test_results //:clippy > "$task_evidence_dir/clippy.log" 2>&1
task_stage=tests
python3 scripts/bazel.py -- build --nofetch //:test_build > "$task_evidence_dir/test-build.log" 2>&1
python3 scripts/bazel.py -- test --nofetch --nocache_test_results --zip_undeclared_test_outputs //:test > "$task_evidence_dir/tests.log" 2>&1
task_testlogs=$(python3 scripts/bazel.py -- info --nofetch bazel-testlogs)
cp "$task_testlogs/retrieval_baseline_test/test.outputs/outputs.zip" "$task_evidence_dir/retrieval-baseline.zip"
task_stage=release
python3 scripts/build_artifact.py "$task_evidence_dir/artifact" > "$task_evidence_dir/release.log" 2>&1
task_stage=release-skill
python3 scripts/bazel.py -- test --nofetch --config=release --nocache_test_results //:skill_export_test --test_arg=--test-threads=1 > "$task_evidence_dir/release-skill.log" 2>&1
task_stage=metadata
cargo metadata --locked --offline --no-deps --format-version 1 > "$task_evidence_dir/package.json"
cp "$task_evidence_dir/artifact/lwiki" "$task_evidence_dir/lwiki"

# The shipped binary runs with only native system tools available on PATH.
task_stage=artifact-smoke
task_binary="$task_evidence_dir/lwiki"
task_vault="$task_evidence_dir/vault"
env -i PATH=/usr/bin:/bin "$task_binary" --version > "$task_evidence_dir/version.txt"
env -i PATH=/usr/bin:/bin "$task_binary" --json capabilities > "$task_evidence_dir/capabilities.json"
env -i PATH=/usr/bin:/bin "$task_binary" --json --offline init "$task_vault" > "$task_evidence_dir/init.json"
printf 'Local qualification evidence. Exact identifier QUAL-731.\n' > "$task_evidence_dir/source.txt"
env -i PATH=/usr/bin:/bin "$task_binary" --wiki "$task_vault" --json --offline source add "$task_evidence_dir/source.txt" > "$task_evidence_dir/capture.json"
env -i PATH=/usr/bin:/bin "$task_binary" --wiki "$task_vault" --json --offline search QUAL-731 --mode literal > "$task_evidence_dir/search.json"
env -i PATH=/usr/bin:/bin "$task_binary" --wiki "$task_vault" --json --offline context qualification --max-bytes 12000 --max-tokens 3000 > "$task_evidence_dir/context.json"
env -i PATH=/usr/bin:/bin "$task_binary" --wiki "$task_vault" --json --offline index rebuild > "$task_evidence_dir/rebuild.json"
env -i PATH=/usr/bin:/bin "$task_binary" --wiki "$task_vault" --json --offline check > "$task_evidence_dir/check.json"
env -i PATH=/usr/bin:/bin "$task_binary" --json --offline skill export --target codex --output "$task_evidence_dir/host-fixture" > "$task_evidence_dir/skill-export.json"

python3 - "$task_evidence_dir" <<'PY'
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
for name in ("capabilities", "init", "capture", "search", "context", "rebuild", "check", "skill-export"):
    result = json.loads((root / (name + ".json")).read_text())
    if result["ok"] is not True or result["meta"]["network_used"] is not False:
        raise SystemExit("failed offline smoke: " + name)
search = json.loads((root / "search.json").read_text())["data"]
context = json.loads((root / "context.json").read_text())["data"]
if not any("QUAL-731" in hit["excerpt"]["text"] for hit in search["hits"]):
    raise SystemExit("literal smoke did not retrieve the captured identifier")
if "QUAL-731" not in context["text"] or not context["passages"]:
    raise SystemExit("context smoke omitted the captured evidence")
if json.loads((root / "check.json").read_text())["data"]["error_count"] != 0:
    raise SystemExit("vault check reported errors")
PY

task_stage=linkage
if command -v otool >/dev/null 2>&1; then
    otool -L "$task_binary" > "$task_evidence_dir/linkage.txt"
elif command -v ldd >/dev/null 2>&1; then
    ldd "$task_binary" > "$task_evidence_dir/linkage.txt"
fi
printf 'All qualification stages passed.\n' > "$task_evidence_dir/qualification.txt"
printf '%s\n' "$task_evidence_dir"
