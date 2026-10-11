#!/usr/bin/env bash
# Disposable native qualification; --archive tests existing final release bytes.
set -euo pipefail
task_repo_root=$(cd "$(dirname "$0")/.." && pwd -P)
cd "$task_repo_root"
exec python3 scripts/qualify_artifact.py "$@"
