#!/usr/bin/env bash
set -euo pipefail

root=${1:-.}
script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
baseline_file=${2:-$script_dir/clippy-baseline.txt}
baseline=$(tr -d '[:space:]' < "$baseline_file")

set +e
output=$(cd "$root" && cargo clippy --locked --all-targets -- -D warnings 2>&1)
code=$?
set -e

if [[ $code -ne 0 && $code -ne 101 ]]; then
  echo "clippy did not complete normally (exit $code)" >&2
  printf '%s\n' "$output" | tail -80 >&2
  exit 1
fi

if [[ $code -eq 101 ]] && ! grep -q 'clippy::' <<< "$output"; then
  echo "clippy failed without a clippy lint diagnostic" >&2
  printf '%s\n' "$output" | tail -80 >&2
  exit 1
fi

count=$(printf '%s\n' "$output" | grep -c '^error:' || true)
count=${count:-0}

if (( count > baseline )); then
  echo "clippy debt grew: $count > $baseline" >&2
  printf '%s\n' "$output" | tail -80 >&2
  exit 1
fi

echo "clippy debt: $count <= $baseline"
