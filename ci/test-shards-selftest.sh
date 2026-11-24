#!/usr/bin/env bash
# Checks the shard plan is complete, non-overlapping, and matches the workflow matrix.
set -euo pipefail

root=$(cd -- "$(dirname -- "$0")" && pwd)
repo_root=${BATON_REPO_ROOT:-}
if [[ -z "$repo_root" ]]; then
  if [[ -f "$PWD/Cargo.toml" ]]; then
    repo_root=$PWD
  else
    repo_root=$(cd -- "$root/.." && pwd)
  fi
fi
tsv="$root/test-shards.tsv"

fail() {
  echo "test shard selftest: $*" >&2
  exit 1
}

expected_shards="tui orchestrate git rest integration"
row_count=$(awk -F '\t' '$1 !~ /^#/ && NF { count++ } END { print count + 0 }' "$tsv")
[[ "$row_count" -eq 5 ]] || fail "expected 5 shard rows, found $row_count"
for shard in $expected_shards; do
  count=$(awk -F '\t' -v shard="$shard" '$1 == shard { count++ } END { print count + 0 }' "$tsv")
  [[ "$count" -eq 1 ]] || fail "shard $shard appears $count times"
done

rest=$(awk -F '\t' '$1 == "rest" { print $3 }' "$tsv")
for skip in tui::tests orchestrate::tests git_workflow::tests; do
  grep -q -- "--skip $skip" <<< "$rest" || fail "rest shard does not skip $skip"
done

integration=$(awk -F '\t' '$1 == "integration" { print $3 }' "$tsv")
shopt -s nullglob
integration_files=("$repo_root"/tests/*.rs)
shopt -u nullglob
[[ "${#integration_files[@]}" -gt 0 ]] || fail "no integration test files found"
for file in "${integration_files[@]}"; do
  name=$(basename "$file" .rs)
  grep -q -- "--test $name" <<< "$integration" || fail "integration shard does not run $name"
done

workflow="$repo_root/.github/workflows/ci.yml"
[[ -f "$workflow" ]] || fail "missing workflow $workflow"
for shard in $expected_shards; do
  args=$(awk -F '\t' -v shard="$shard" '$1 == shard { print $3 }' "$tsv")
  grep -q -- "- shard: $shard" "$workflow" || fail "workflow lacks shard $shard"
  grep -Fq -- "command: $args" "$workflow" || fail "workflow shard $shard does not match: $args"
done
grep -q 'cargo test --locked ${{ matrix.command }}' "$workflow" || fail "workflow does not run the matrix command with cargo test"
echo "test shard plan passed"
