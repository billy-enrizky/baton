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

valid="$root/fixtures/check-ci/valid.yml"
bash "$root/check-ci.sh" --bootstrap "$valid" >/dev/null

for invalid in "$root"/fixtures/check-ci/invalid-*.yml; do
  expected_file="${invalid%.yml}.expected"
  expected_rule=$(tr -d '[:space:]' < "$expected_file")
  set +e
  output=$(bash "$root/check-ci.sh" --bootstrap "$invalid" 2>&1)
  code=$?
  set -e
  if [[ $code -eq 0 ]]; then
    echo "invalid workflow passed: $invalid" >&2
    exit 1
  fi
  if ! grep -q "ERROR: $expected_rule " <<< "$output"; then
    echo "invalid workflow failed without $expected_rule: $invalid" >&2
    printf '%s\n' "$output" >&2
    exit 1
  fi
done

set +e
bash "$root/check-ci.sh" --base "$valid" "$root/fixtures/check-ci/changed-test-command.yml" >/dev/null 2>&1
code=$?
set -e
if [[ $code -eq 0 ]]; then
  echo "changed test command passed without ci-change" >&2
  exit 1
fi
bash "$root/check-ci.sh" --base "$valid" "$root/fixtures/check-ci/changed-test-command.yml" --allow-change >/dev/null

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
