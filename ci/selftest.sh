#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "$0")" && pwd)

for fixture in "$root"/fixtures/paths/*.txt; do
  expected="${fixture%.txt}.expected"
  actual=$(bash "$root/classify-paths.sh" --files "$fixture" . .)
  if [[ "$actual" != "$(cat "$expected")" ]]; then
    echo "path fixture failed: $fixture" >&2
    diff -u "$expected" <(printf '%s\n' "$actual") >&2 || true
    exit 1
  fi
done

bash "$root/check-ci.sh" "$root/fixtures/check-ci/valid.yml" >/dev/null

for invalid in "$root"/fixtures/check-ci/invalid-*.yml; do
  if bash "$root/check-ci.sh" "$invalid" >/dev/null 2>&1; then
    echo "invalid workflow passed: $invalid" >&2
    exit 1
  fi
done

set +e
bash "$root/budget.sh" 1 sleep 2
code=$?
set -e
if [[ $code -ne 124 ]]; then
  echo "budget overrun returned $code instead of 124" >&2
  exit 1
fi

bash "$root/budget.sh" 5 true
echo "ci selftest passed"
