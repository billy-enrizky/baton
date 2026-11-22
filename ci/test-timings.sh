#!/usr/bin/env bash
# Records how long each test binary took, so shards can be filled from data
# rather than from a guess. Reads the cargo test log captured by the test job.
set -euo pipefail

log=${1:?usage: ci/test-timings.sh <cargo-test-log> [output.json]}
output=${2:-}

if [[ ! -f "$log" ]]; then
  echo "timings: no log at $log; nothing recorded" >&2
  exit 0
fi

awk '
  /^     Running / {
    label = $0
    sub(/^     Running /, "", label)
    sub(/ \(.*/, "", label)
    next
  }
  /^test result: / {
    seconds = ""
    for (i = 1; i <= NF; i++) {
      if ($i ~ /^[0-9]+(\.[0-9]+)?s$/) { seconds = $i; sub(/s$/, "", seconds) }
    }
    passed = ""
    for (i = 1; i <= NF; i++) { if ($i == "passed;") { passed = $(i - 1) } }
    if (label != "" && seconds != "") {
      printf "%s\t%s\t%s\n", label, seconds, passed
      label = ""
    }
  }
' "$log" | sort -t $'\t' -k2,2gr | tee "${output:-/dev/stdout}" |
  awk -F '\t' 'BEGIN { printf "test timings (seconds):\n" } { printf "  %-34s %8ss  %s tests\n", $1, $2, $3 }'
