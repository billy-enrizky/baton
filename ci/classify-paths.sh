#!/usr/bin/env bash
set -euo pipefail

files_file=
event=
while [[ ${1:-} == --* ]]; do
  case "$1" in
    --files)
      files_file=${2:?--files requires a path}
      shift 2
      ;;
    --event)
      event=${2:?--event requires a name}
      shift 2
      ;;
    *)
      printf 'unknown option: %s\n' "$1" >&2
      exit 2
      ;;
  esac
done

head_dir=${1:-.}
base_dir=${2:-$head_dir}
event=${event:-${GITHUB_EVENT_NAME:-pull_request}}

full_reason=
files=

if [[ -n "$files_file" ]]; then
  files=$(cat "$files_file")
elif [[ "$event" == merge_group || "$event" == push || "$event" == workflow_dispatch ]]; then
  full_reason=$event
elif [[ "$event" == pull_request ]]; then
  if ! base_sha=$(git -C "$base_dir" rev-parse HEAD 2>/dev/null); then
    full_reason=unknown
  elif ! files=$(git -C "$head_dir" diff --name-only --no-renames --diff-filter=ACDMRT "$base_sha" HEAD 2>/dev/null); then
    full_reason=unknown
  fi
else
  full_reason=unknown
fi

if [[ -n "$full_reason" ]]; then
  printf '%s\n' \
    'build_needed=true' \
    'check_needed=true' \
    'debt_needed=true' \
    'docs_only=false' \
    "full_suite_reason=$full_reason" \
    'has_tests=true' \
    'test_needed=true'
  exit 0
fi

if [[ -z "${files//[$'\n\t ']/}" ]]; then
  printf '%s\n' \
    'build_needed=true' \
    'check_needed=true' \
    'debt_needed=true' \
    'docs_only=false' \
    'full_suite_reason=empty' \
    'has_tests=true' \
    'test_needed=true'
  exit 0
fi

code=false
meta=false
unknown=false

while IFS= read -r file; do
  [[ -z "$file" ]] && continue
  case "$file" in
    *.rs|Cargo.toml|Cargo.lock|rust-toolchain.toml|build.rs)
      code=true
      ;;
    .github/workflows/*|ci/*|docs/ci.md|docs/parity.md)
      meta=true
      ;;
    docs/*|*.md|LICENSE)
      ;;
    *)
      unknown=true
      ;;
  esac
done <<< "$files"

if [[ "$unknown" == true ]]; then
  reason=unknown
elif [[ "$code" == true ]]; then
  reason=code
elif [[ "$meta" == true ]]; then
  reason=meta
else
  printf '%s\n' \
    'build_needed=false' \
    'check_needed=false' \
    'debt_needed=false' \
    'docs_only=true' \
    'full_suite_reason=docs' \
    'has_tests=false' \
    'test_needed=false'
  exit 0
fi

printf '%s\n' \
  'build_needed=true' \
  'check_needed=true' \
  'debt_needed=true' \
  'docs_only=false' \
  "full_suite_reason=$reason" \
  'has_tests=true' \
  'test_needed=true'
