#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "$0")" && pwd)
quick=false
if [[ "${1:-}" == "--quick" ]]; then
    quick=true
fi
repo_root=${BATON_REPO_ROOT:-}
if [ -z "$repo_root" ]; then
    if [ -f "$PWD/Cargo.toml" ]; then
        repo_root=$PWD
    else
        repo_root=$(cd -- "$root/.." && pwd)
    fi
fi

for fixture in "$root"/fixtures/paths/*.txt; do
  expected="${fixture%.txt}.expected"
  actual=$(bash "$root/classify-paths.sh" --files "$fixture" . .)
  if [[ "$actual" != "$(cat "$expected")" ]]; then
    echo "path fixture failed: $fixture" >&2
    diff -u "$expected" <(printf '%s\n' "$actual") >&2 || true
    exit 1
  fi
done

stale_root=$(mktemp -d)
git -C "$stale_root" init --quiet -b main
git -C "$stale_root" config user.email ci@example.invalid
git -C "$stale_root" config user.name ci
printf 'base\n' > "$stale_root/base.txt"
git -C "$stale_root" add base.txt
git -C "$stale_root" commit --quiet -m base
git -C "$stale_root" switch --quiet -c pr
printf 'pr\n' > "$stale_root/pr.txt"
git -C "$stale_root" add pr.txt
git -C "$stale_root" commit --quiet -m pr
git -C "$stale_root" switch --quiet main
printf 'base side\n' > "$stale_root/base-side.txt"
git -C "$stale_root" add base-side.txt
git -C "$stale_root" commit --quiet -m base-side
stable_base=$(git -C "$stale_root" rev-parse main)
git -C "$stale_root" merge --quiet --no-ff pr -m merge
merge_commit=$(git -C "$stale_root" rev-parse HEAD)
git -C "$stale_root" diff --name-only "$stable_base" pr > "$stale_root/head.diff"
git -C "$stale_root" diff --name-only "$stable_base" "$merge_commit" > "$stale_root/merge.diff"
grep -qx 'base-side.txt' "$stale_root/head.diff"
if grep -qx 'base-side.txt' "$stale_root/merge.diff"; then
  echo "stale-base fixture: merge diff included a base-side change" >&2
  exit 1
fi
grep -qx 'pr.txt' "$stale_root/merge.diff"

type_root=$(mktemp -d)
git -C "$type_root" init --quiet -b main
git -C "$type_root" config user.email ci@example.invalid
git -C "$type_root" config user.name ci
mkdir -p "$type_root/src"
printf 'pub fn f() {}\n' > "$type_root/src/lib.rs"
printf 'docs\n' > "$type_root/README.md"
git -C "$type_root" add -A
git -C "$type_root" commit --quiet -m base
rm "$type_root/src/lib.rs"
ln -s ../README.md "$type_root/src/lib.rs"
git -C "$type_root" add -A
git -C "$type_root" commit --quiet -m typechange
git -C "$type_root" diff --name-only --no-renames --diff-filter=ACDMRT HEAD~1 HEAD > "$type_root/type.diff"
grep -qx 'src/lib.rs' "$type_root/type.diff"
printf 'src/lib.rs\nREADME.md\n' > "$type_root/type-files.txt"
type_selection=$(bash "$root/classify-paths.sh" --files "$type_root/type-files.txt" . .)
if ! grep -q '^full_suite_reason=code$' <<< "$type_selection"; then
  echo "typechange fixture: a .rs typechange did not select code" >&2
  exit 1
fi

valid="$root/fixtures/check-ci/valid.yml"
bash "$root/check-ci.sh" --bootstrap "$valid" >/dev/null
if [[ -f "$repo_root/.github/workflows/ci.yml" ]]; then
  bash "$root/check-ci.sh" --bootstrap "$repo_root/.github/workflows/ci.yml" >/dev/null
  if grep -q 'GITHUB_WORKSPACE/base/ci/\(fmt\|clippy\)-baseline.txt' "$repo_root/.github/workflows/ci.yml"; then
    echo "debt ratchet reads a writable baseline from the base checkout" >&2
    exit 1
  fi
fi

if [[ "$quick" == true ]]; then
    echo "ci quick selftest passed"
    exit 0
fi

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
output=$(bash "$root/check-ci.sh" --base "$valid" "$root/fixtures/check-ci/changed-test-command.yml" 2>&1)
code=$?
set -e
if [[ $code -eq 0 ]] || ! grep -q 'ERROR: CI013 ' <<< "$output"; then
  echo "changed test command did not fail with CI013 without --allow-change" >&2
  exit 1
fi
bash "$root/check-ci.sh" --base "$valid" "$root/fixtures/check-ci/changed-test-command.yml" --allow-change >/dev/null

set +e
output=$(bash "$root/check-ci.sh" --base "$valid" "$root/fixtures/check-ci/changed-job-wiring.yml" --allow-change 2>&1)
code=$?
set -e
if [[ $code -eq 0 ]] || ! grep -q 'ERROR: CI013 job check wiring changed' <<< "$output"; then
  echo "--allow-change admitted a job-level wiring change" >&2
  exit 1
fi

set +e
output=$(bash "$root/check-ci.sh" --base "$valid" "$root/fixtures/check-ci/verify-drops-live-job.yml" --allow-change 2>&1)
code=$?
set -e
if [[ $code -eq 0 ]] || ! grep -q 'ERROR: CI005 verify is missing needs: debt' <<< "$output"; then
  echo "verify stopped requiring a job that still exists" >&2
  printf '%s\n' "$output" >&2
  exit 1
fi

set +e
output=$(bash "$root/check-ci.sh" --base "$valid" "$root/fixtures/check-ci/invalid-allow-change-missing-command.yml" --allow-change 2>&1)
code=$?
set -e
if [[ $code -eq 0 ]] || ! grep -q 'CI014' <<< "$output"; then
  echo "--allow-change skipped a missing required command" >&2
  exit 1
fi

set +e
output=$(bash "$root/check-ci.sh" --base "$valid" "$root/fixtures/check-ci/invalid-unbudgeted-command.yml" --allow-change 2>&1)
code=$?
set -e
if [[ $code -eq 0 ]] || ! grep -q 'CI017' <<< "$output"; then
  echo "--allow-change skipped a missing budget" >&2
  exit 1
fi

set +e
bash "$root/budget.sh" 1 sleep 2
code=$?
set -e
if [[ $code -ne 124 ]]; then
  echo "budget overrun returned $code instead of 124" >&2
  exit 1
fi

bash "$root/budget.sh" 5 true

release_manifest=$(mktemp)
printf '[package]\nname = "fixture"\nversion = "1.2.3"\n\n[dependencies]\nversion = "9.9.9"\n' > "$release_manifest"
bash "$root/release-guard.sh" v1.2.3 "$release_manifest" >/dev/null

check_release_guard() {
  local name=$1
  local tag=$2
  local manifest=$3
  local expected=$4
  set +e
  local output
  output=$(bash "$root/release-guard.sh" "$tag" "$manifest" 2>&1)
  local code=$?
  set -e
  if [[ $code -ne "$expected" ]]; then
    echo "release guard $name returned $code instead of $expected" >&2
    printf '%s\n' "$output" >&2
    exit 1
  fi
}

check_release_guard mismatch v1.2.4 "$release_manifest" 1
check_release_guard no-prefix 1.2.3 "$release_manifest" 1
check_release_guard incomplete v1.2 "$release_manifest" 1
check_release_guard leading-zero v01.2.3 "$release_manifest" 1
check_release_guard missing-manifest v1.2.3 "$(mktemp -d)/missing.toml" 1

malformed_version=$(mktemp)
printf '[package]\nname = "fixture"\nversion = "1.2"\n' > "$malformed_version"
check_release_guard malformed-package-version v1.2.3 "$malformed_version" 1

leading_zero_manifest=$(mktemp)
printf '[package]\nname = "fixture"\nversion = "01.2.3"\n' > "$leading_zero_manifest"
check_release_guard leading-zero-package-version v1.2.3 "$leading_zero_manifest" 1

trailing_token=$(mktemp)
printf '[package]\nname = "fixture"\nversion = "1.2.3" extra\n' > "$trailing_token"
check_release_guard trailing-version-token v1.2.3 "$trailing_token" 1
set +e
output=$(bash "$root/release-guard.sh" 2>&1)
code=$?
set -e
if [[ $code -ne 2 ]]; then
  echo "release guard without arguments returned $code instead of 2" >&2
  printf '%s\n' "$output" >&2
  exit 1
fi

missing_version=$(mktemp)
printf '[package]\nname = "fixture"\n\n[metadata]\nversion = "1.2.3"\n' > "$missing_version"
check_release_guard missing-package-version v1.2.3 "$missing_version" 1

duplicate_version=$(mktemp)
printf '[package]\nname = "fixture"\nversion = "1.2.3"\nversion = "1.2.4"\n' > "$duplicate_version"
check_release_guard duplicate-package-version v1.2.3 "$duplicate_version" 1

integrity_root=$(mktemp -d)
for case in rustfmt wrapper lints allow; do
  mkdir -p "$integrity_root/$case/src"
  : > "$integrity_root/$case/src/lib.rs"
done
printf 'disable_all_formatting = true\n' > "$integrity_root/rustfmt/rustfmt.toml"
mkdir -p "$integrity_root/wrapper/.cargo"
printf '[build]\nrustc-wrapper = "./x.sh"\n' > "$integrity_root/wrapper/.cargo/config.toml"
printf '[lints.clippy]\nall = "allow"\n' > "$integrity_root/lints/Cargo.toml"
printf '#![allow(clippy::all)]\n' > "$integrity_root/allow/src/lib.rs"
for case in warnings cfgattr skip; do
  mkdir -p "$integrity_root/$case/src"
done
printf 'fn a() {}\n' > "$integrity_root/warnings/src/lib.rs"
printf '#![allow(warnings)]\n' > "$integrity_root/warnings/src/lib.rs"
printf '#[cfg_attr(test, allow(clippy::all))]\nfn a() {}\n' > "$integrity_root/cfgattr/src/lib.rs"
printf '#[rustfmt::skip]\nfn a() {}\n' > "$integrity_root/skip/src/lib.rs"
for pair in rustfmt:GI001 wrapper:GI002 lints:GI003 allow:GI004 warnings:GI005 cfgattr:GI005 skip:GI006; do
  case_name=${pair%%:*}
  rule=${pair##*:}
  set +e
  output=$(bash "$root/gate-integrity.sh" "$integrity_root/$case_name" 2>&1)
  code=$?
  set -e
  if [[ $code -eq 0 ]] || ! grep -q "$rule " <<< "$output"; then
    echo "gate integrity did not reject $case_name with $rule" >&2
    exit 1
  fi
done
bash "$root/gate-integrity.sh" "$root/../.."

empty_root=$(mktemp -d)
mkdir -p "$empty_root/src"
printf 'fn main() {}\n' > "$empty_root/src/main.rs"
printf '[package]\nname = "empty"\nversion = "0.1.0"\nedition = "2021"\n' > "$empty_root/Cargo.toml"
set +e
output=$(bash "$root/test-inventory.sh" "$empty_root" 2>&1)
code=$?
set -e
if [[ $code -eq 0 ]] || ! grep -q "TI001" <<< "$output"; then
  echo "test inventory accepted a crate with no tests" >&2
  printf '%s\n' "$output" >&2
  exit 1
fi

low_log=$(mktemp)
printf 'test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n' > "$low_log"
set +e
output=$(bash "$root/test-inventory.sh" "$root/../.." "$root/test-inventory-floor.txt" "$low_log" 2>&1)
code=$?
set -e
if [[ $code -eq 0 ]] || ! grep -q "TI002" <<< "$output"; then
  echo "test inventory accepted fewer executed tests than the floor" >&2
  exit 1
fi

small_floor=$(mktemp)
printf '2\n' > "$small_floor"
good_log=$(mktemp)
printf 'running 2 tests\n\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n' > "$good_log"
good_output=$(bash "$root/test-inventory.sh" "$root/../.." "$small_floor" "$good_log")
if [[ "$good_output" != "test_count=2" ]]; then
  echo "test inventory rejected a matching announced and executed count" >&2
  printf '%s\n' "$good_output" >&2
  exit 1
fi

short_log=$(mktemp)
printf 'running 3 tests\n\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n' > "$short_log"
set +e
output=$(bash "$root/test-inventory.sh" "$root/../.." "$small_floor" "$short_log" 2>&1)
code=$?
set -e
if [[ $code -eq 0 ]] || ! grep -q "TI004" <<< "$output"; then
  echo "test inventory accepted a short executed count" >&2
  exit 1
fi

missing_totals_log=$(mktemp)
printf 'test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n' > "$missing_totals_log"
set +e
output=$(bash "$root/test-inventory.sh" "$root/../.." "$small_floor" "$missing_totals_log" 2>&1)
code=$?
set -e
if [[ $code -eq 0 ]] || ! grep -q "TI004" <<< "$output"; then
  echo "test inventory accepted a log without running test totals" >&2
  exit 1
fi

ignored_log=$(mktemp)
floor=$(tr -dc '0-9' < "$root/test-inventory-floor.txt")
printf 'test result: ok. %s passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.01s\n' "$floor" > "$ignored_log"
set +e
output=$(bash "$root/test-inventory.sh" "$root/../.." "$root/test-inventory-floor.txt" "$ignored_log" 2>&1)
code=$?
set -e
if [[ $code -eq 0 ]] || ! grep -q "TI003" <<< "$output"; then
  echo "test inventory accepted ignored tests" >&2
  exit 1
fi

link_log=$(mktemp -d)/link.log
ln -s /dev/null "$link_log"
set +e
output=$(bash "$root/test-inventory.sh" "$root/../.." "$root/test-inventory-floor.txt" "$link_log" 2>&1)
code=$?
set -e
if [[ $code -eq 0 ]] || ! grep -q "TI005" <<< "$output"; then
  echo "test inventory accepted a symlinked log" >&2
  exit 1
fi

for case in rustc linker; do
  mkdir -p "$integrity_root/$case/src" "$integrity_root/$case/.cargo"
  printf 'fn main() {}\n' > "$integrity_root/$case/src/main.rs"
done
printf '[build]\nrustc = "/bin/false"\n' > "$integrity_root/rustc/.cargo/config.toml"
printf '[target.x86_64-unknown-linux-gnu]\nlinker = "/bin/false"\n' > "$integrity_root/linker/.cargo/config.toml"
for case in rustc linker; do
  set +e
  output=$(bash "$root/gate-integrity.sh" "$integrity_root/$case" 2>&1)
  code=$?
  set -e
  if [[ $code -eq 0 ]] || ! grep -q "GI002 " <<< "$output"; then
    echo "gate integrity accepted a $case override" >&2
    exit 1
  fi
done

timings_log=$(mktemp)
printf '     Running unittests src/lib.rs (target/debug/deps/baton-1)\nrunning 2 tests\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 12.50s\n     Running tests/core.rs (target/debug/deps/core-2)\nrunning 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.40s\n' > "$timings_log"
timings_out=$(mktemp)
bash "$root/test-timings.sh" "$timings_log" "$timings_out" >/dev/null
expected_timings=$(printf 'unittests src/lib.rs\t12.50\t2\ntests/core.rs\t0.40\t1')
colored_log=$(mktemp)
printf '\033[1m\033[92m     Running\033[0m tests/core.rs (target/debug/deps/core-2)\nrunning 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.40s\n' > "$colored_log"
colored_out=$(mktemp)
bash "$root/test-timings.sh" "$colored_log" "$colored_out" >/dev/null
if [[ "$(cat "$colored_out")" != "$(printf 'tests/core.rs\t0.40\t1')" ]]; then
  echo "test timings did not strip cargo color" >&2
  cat "$colored_out" >&2
  exit 1
fi
if [[ "$(cat "$timings_out")" != "$expected_timings" ]]; then
  echo "test timings did not parse the sample log" >&2
  cat "$timings_out" >&2
  exit 1
fi

bash "$root/installer-selftest.sh" "$repo_root"

echo "ci selftest passed"
