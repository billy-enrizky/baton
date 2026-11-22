#!/usr/bin/env bash
# Rejects head-controlled configuration that can silence a gate without changing a command.
set -euo pipefail

repo=${1:-.}
cd -- "$repo"

fail() {
  echo "ERROR: $*" >&2
  exit 1
}

for file in rustfmt.toml .rustfmt.toml clippy.toml .clippy.toml; do
  [[ -e "$file" ]] && fail "GI001 $file is not allowed; formatting and lint settings come from pinned tool defaults"
done

for file in .cargo/config.toml .cargo/config .cargo/config.toml.example; do
  if [[ -e "$file" ]]; then
    fail "GI002 $file is not allowed; it can replace the compiler, linker, flags, or registry"
  fi
done

if [[ -f Cargo.toml ]] && grep -qE '^\[lints' Cargo.toml; then
  fail "GI003 [lints] in Cargo.toml can silence Clippy"
fi

if grep -rqE '#!?\[allow\(clippy::(all|pedantic|nursery|correctness)' --include='*.rs' src tests 2>/dev/null; then
  fail "GI004 blanket Clippy allow in tracked Rust sources"
fi

if grep -rqE 'allow\(warnings\)|cfg_attr\([^)]*allow\(clippy::' --include='*.rs' src tests 2>/dev/null; then
  fail "GI005 warning or conditional Clippy suppression in tracked Rust sources"
fi

if grep -rqE '#\[rustfmt::skip\]' --include='*.rs' src tests 2>/dev/null; then
  fail "GI006 rustfmt skip in tracked Rust sources"
fi

echo "gate integrity passed"
