#!/usr/bin/env bash
# Local qualification only: disposable fixtures/mock providers, no publication.
set -euo pipefail

task_repo_root=$(cd "$(dirname "$0")/.." && pwd)
cd "$task_repo_root"
task_evidence_dir=${1:-$(mktemp -d "${TMPDIR:-/tmp}/lwiki-qualification.XXXXXX")}
mkdir -p "$task_evidence_dir"
task_evidence_dir=$(cd "$task_evidence_dir" && pwd)

cargo fmt --all -- --check > "$task_evidence_dir/fmt.log" 2>&1
cargo clippy --locked --offline --all-targets -- -D warnings > "$task_evidence_dir/clippy.log" 2>&1
cargo test --locked --offline --all-targets > "$task_evidence_dir/tests.log" 2>&1
cargo test --locked --offline --doc > "$task_evidence_dir/doctests.log" 2>&1
cargo build --locked --offline --release > "$task_evidence_dir/release.log" 2>&1
cargo test --locked --offline --release --test skill_export -- --test-threads=1 > "$task_evidence_dir/release-skill.log" 2>&1
cargo metadata --locked --offline --format-version 1 > "$task_evidence_dir/dependencies.json"
cp target/release/lwiki "$task_evidence_dir/lwiki"

# The shipped binary runs with only native system tools available on PATH.
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

if command -v otool >/dev/null 2>&1; then
    otool -L "$task_binary" > "$task_evidence_dir/linkage.txt"
elif command -v ldd >/dev/null 2>&1; then
    ldd "$task_binary" > "$task_evidence_dir/linkage.txt"
fi
printf '%s\n' "$task_evidence_dir"
