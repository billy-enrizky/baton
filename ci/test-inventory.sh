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

if [[ "$mode" == list ]]; then
  discovered=$(grep -c ': test$' <<< "$output" || true)
  discovered=${discovered:-0}
  if [[ "$discovered" -eq 0 ]]; then
    echo "ERROR: TI001 no tests discovered" >&2
    exit 1
  fi
  echo "test_count=$discovered"
  exit 0
fi

announced=$(grep -oE '^running [0-9]+ tests?$' <<< "$output" | awk '{sum += $2} END {print sum + 0}' || true)
passed=$(grep -oE '^test result: ok\. [0-9]+ passed' <<< "$output" | awk '{sum += $4} END {print sum + 0}' || true)
ignored=$(grep -oE '[0-9]+ ignored' <<< "$output" | awk '{sum += $1} END {print sum + 0}' || true)
announced=${announced:-0}
passed=${passed:-0}
ignored=${ignored:-0}

if [[ "$announced" -eq 0 && "$passed" -eq 0 ]]; then
  echo "ERROR: TI001 no tests discovered" >&2
  exit 1
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

if [[ "$announced" -eq 0 ]]; then
  echo "ERROR: TI004 test log has no running test totals" >&2
  exit 1
fi

executed=$((passed + ignored))
if [[ "$executed" -ne "$announced" ]]; then
  echo "ERROR: TI004 announced $announced tests but executed $executed" >&2
  exit 1
fi

echo "test_count=$passed"
