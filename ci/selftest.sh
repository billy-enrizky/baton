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
  echo "changed test command passed without --allow-change" >&2
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

ignored_log=$(mktemp)
printf 'test result: ok. 400 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.01s\n' > "$ignored_log"
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

echo "ci selftest passed"
