#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'usage: release-guard.sh <vX.Y.Z> [Cargo.toml]\n' >&2
    exit 2
}

if [[ $# -lt 1 || $# -gt 2 ]]; then
    usage
fi

tag=$1
manifest=${2:-Cargo.toml}

if [[ ! "$tag" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
    printf 'release tag must be vX.Y.Z, got %s\n' "$tag" >&2
    exit 1
fi

if [[ ! -r "$manifest" ]]; then
    printf 'manifest not readable: %s\n' "$manifest" >&2
    exit 1
fi

versions=$(awk '
    /^\[package\][[:space:]]*$/ { in_package = 1; next }
    /^\[/ { in_package = 0 }
    in_package && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"[^"]+"[[:space:]]*$/ {
        line = $0
        sub(/^[^"]*"/, "", line)
        sub(/".*$/, "", line)
        print line
    }
' "$manifest")

count=$(printf '%s\n' "$versions" | grep -c . || true)
if [[ "$count" -eq 0 ]]; then
    printf 'no package version found in %s\n' "$manifest" >&2
    exit 1
fi
if [[ "$count" -ne 1 ]]; then
    printf 'expected one package version in %s, found %s\n' "$manifest" "$count" >&2
    exit 1
fi
if [[ ! "$versions" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
    printf 'package version is not semver: %s\n' "$versions" >&2
    exit 1
fi

expected="v${versions}"
if [[ "$tag" != "$expected" ]]; then
    printf 'tag %s does not match package version %s\n' "$tag" "$versions" >&2
    exit 1
fi

printf 'release tag %s matches package version %s\n' "$tag" "$versions"
