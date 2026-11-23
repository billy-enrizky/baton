#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "$0")" && pwd)

fmt_root=$(mktemp -d)
mkdir -p "$fmt_root/src"
printf '[package]\nname = "fmt-fixture"\nversion = "0.1.0"\nedition = "2021"\n' > "$fmt_root/Cargo.toml"
printf 'pub fn f( ){ }\n' > "$fmt_root/src/lib.rs"
printf '0\n' > "$fmt_root/fmt-baseline.txt"
set +e
output=$(CARGO_TERM_COLOR=always bash "$root/fmt-debt.sh" "$fmt_root" "$fmt_root/fmt-baseline.txt" 2>&1)
code=$?
set -e
if [[ $code -eq 0 ]] || ! grep -q 'format debt grew' <<< "$output"; then
  echo "format ratchet did not reject a format diff over the baseline" >&2
  printf '%s\n' "$output" >&2
  exit 1
fi

clippy_root=$(mktemp -d)
mkdir -p "$clippy_root/src"
printf '[package]\nname = "clippy-fixture"\nversion = "0.1.0"\nedition = "2021"\n' > "$clippy_root/Cargo.toml"
printf 'pub fn f(value: &Vec<u8>) -> usize { value.len() }\n' > "$clippy_root/src/lib.rs"
(cd "$clippy_root" && cargo generate-lockfile --quiet)
printf '0\n' > "$clippy_root/clippy-baseline.txt"
set +e
output=$(CARGO_TERM_COLOR=always bash "$root/clippy-debt.sh" "$clippy_root" "$clippy_root/clippy-baseline.txt" 2>&1)
code=$?
set -e
if [[ $code -eq 0 ]] || ! grep -q 'clippy debt grew' <<< "$output"; then
  echo "clippy ratchet did not reject a lint over the baseline" >&2
  printf '%s\n' "$output" >&2
  exit 1
fi

printf '1\n' > "$clippy_root/clippy-baseline-one.txt"
set +e
output=$(CARGO_TERM_COLOR=always bash "$root/clippy-debt.sh" "$clippy_root" "$clippy_root/clippy-baseline-one.txt" 2>&1)
code=$?
set -e
if [[ $code -ne 0 ]] || ! grep -q 'clippy debt: 1 <= 1' <<< "$output"; then
  echo "clippy ratchet counted compiler summary lines as debt" >&2
  printf '%s\n' "$output" >&2
  exit 1
fi

echo "debt self-test passed"
