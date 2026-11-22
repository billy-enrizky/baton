#!/usr/bin/env bash
# Counts discovered tests so a green run that executed nothing cannot pass.
set -euo pipefail

repo=${1:-.}
floor_file=${2:-}
cd -- "$repo"

output=$(cargo test --locked -- --list 2>/dev/null || true)
count=$(grep -c ': test$' <<< "$output" || true)
count=${count:-0}

if [[ "$count" -eq 0 ]]; then
  echo "ERROR: TI001 no tests discovered" >&2
  exit 1
fi

if [[ -n "$floor_file" && -f "$floor_file" ]]; then
  floor=$(tr -dc '0-9' < "$floor_file")
  if [[ -n "$floor" && "$count" -lt "$floor" ]]; then
    echo "ERROR: TI002 discovered $count tests, below the recorded floor of $floor" >&2
    exit 1
  fi
fi

echo "test_count=$count"
