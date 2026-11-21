#!/usr/bin/env bash
set -euo pipefail

root=${1:-.}
script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
baseline_file=${2:-$script_dir/fmt-baseline.txt}
baseline=$(tr -d '[:space:]' < "$baseline_file")

set +e
output=$(cd "$root" && cargo fmt --all -- --check 2>&1)
code=$?
set -e

if [[ $code -ne 0 && -z "$output" ]]; then
  echo "cargo fmt failed without output" >&2
  exit 1
fi

count=$(printf '%s\n' "$output" | grep -c . || true)
count=${count:-0}

if (( count > baseline )); then
  echo "format debt grew: $count > $baseline" >&2
  exit 1
fi

echo "format debt: $count <= $baseline"
