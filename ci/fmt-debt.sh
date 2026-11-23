#!/usr/bin/env bash
set -euo pipefail

root=${1:-.}
script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
baseline_file=${2:-$script_dir/fmt-baseline.txt}
baseline=$(tr -d '[:space:]' < "$baseline_file")

set +e
output=$(cd "$root" && CARGO_TERM_COLOR=never cargo fmt --all -- --check 2>&1)
code=$?
set -e

if [[ $code -ne 0 ]] && ! grep -q '^Diff in ' <<< "$output"; then
  echo "cargo fmt did not produce format diffs" >&2
  printf '%s\n' "$output" | tail -40 >&2
  exit 1
fi

count=$(printf '%s\n' "$output" | grep -c '^Diff in ' || true)
count=${count:-0}

if (( count > baseline )); then
  echo "format debt grew: $count > $baseline" >&2
  exit 1
fi

echo "format debt: $count <= $baseline"
