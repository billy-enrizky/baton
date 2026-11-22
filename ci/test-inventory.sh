#!/usr/bin/env bash
# Fails when a green test job discovered nothing, executed too little, or skipped tests.
set -euo pipefail

repo=${1:-.}
floor_file=${2:-}
log_file=${3:-}
cd -- "$repo"

mode=list
if [[ -n "$log_file" ]]; then
  if [[ ! -f "$log_file" || -L "$log_file" ]]; then
    echo "ERROR: TI005 test log $log_file is missing or not a regular file" >&2
    exit 1
  fi
  mode=run
  output=$(cat "$log_file")
else
  output=$(cargo test --locked -- --list 2>/dev/null || true)
fi

discovered=$(grep -c ': test$' <<< "$output" || true)
discovered=${discovered:-0}
passed=$(grep -oE '^test result: ok\. [0-9]+ passed' <<< "$output" | awk '{sum += $4} END {print sum + 0}' || true)
ignored=$(grep -oE '[0-9]+ ignored' <<< "$output" | awk '{sum += $1} END {print sum + 0}' || true)
passed=${passed:-0}
ignored=${ignored:-0}

if [[ "$discovered" -eq 0 && "$passed" -eq 0 ]]; then
  echo "ERROR: TI001 no tests discovered" >&2
  exit 1
fi

if [[ "$mode" == list ]]; then
  echo "test_count=$discovered"
  exit 0
fi

if [[ -n "$floor_file" && -f "$floor_file" ]]; then
  floor=$(tr -dc '0-9' < "$floor_file")
  if [[ -n "$floor" && "$passed" -lt "$floor" ]]; then
    echo "ERROR: TI002 executed $passed tests, below the recorded floor of $floor" >&2
    exit 1
  fi
fi

if [[ "$ignored" -gt 0 ]]; then
  echo "ERROR: TI003 $ignored tests were ignored; a quarantine needs an owner and a linked issue" >&2
  exit 1
fi

if [[ "$discovered" -gt 0 && "$passed" -gt 0 && $((passed + ignored)) -lt "$discovered" ]]; then
  echo "ERROR: TI004 discovered $discovered tests but only executed $passed" >&2
  exit 1
fi

echo "test_count=$passed"
