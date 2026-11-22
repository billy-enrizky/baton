#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "$0")/.." && pwd)
repo_root=${1:-$root}
tmp=$(mktemp -d "${TMPDIR:-/tmp}/baton-installer-selftest.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

release="$tmp/release"
install="$tmp/install/bin"
fake="$tmp/bin"
build="$tmp/build"
mkdir -p "$release" "$install" "$fake" "$build"

cat >"$build/baton" <<'SH'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then
    echo "baton 0.3.0"
    exit 0
fi
echo fake baton
SH
chmod 755 "$build/baton"
tar -C "$build" -czf "$release/baton-x86_64-unknown-linux-gnu.tar.gz" baton
if command -v sha256sum >/dev/null 2>&1; then
    (cd "$release" && sha256sum ./* >SHA256SUMS)
else
    (cd "$release" && shasum -a 256 ./* >SHA256SUMS)
fi

cat >"$fake/uname" <<'SH'
#!/bin/sh
case "$1" in
    -s) echo Linux ;;
    -m) echo x86_64 ;;
    *) echo Linux ;;
esac
SH
cat >"$fake/curl" <<'SH'
#!/bin/sh
out=
url=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o)
            out=$2
            shift 2
            ;;
        --retry | --retry-delay | --connect-timeout | --max-time)
            shift 2
            ;;
        http*)
            url=$1
            shift
            ;;
        *)
            shift
            ;;
    esac
done
[ -n "$out" ] || exit 2
name=${url##*/}
source_file="$BATON_TEST_RELEASE/$name"
[ -f "$source_file" ] || exit 22
cp "$source_file" "$out"
SH
chmod 755 "$fake/uname" "$fake/curl"

BATON_TEST_RELEASE="$release" \
BATON_INSTALL_DIR="$install" \
PATH="$fake:$PATH" \
    "$repo_root/install.sh" >/dev/null
[ "$("$install/baton" --version)" = "baton 0.3.0" ]

printf 'corrupt\n' >>"$release/baton-x86_64-unknown-linux-gnu.tar.gz"
if BATON_TEST_RELEASE="$release" \
    BATON_INSTALL_DIR="$tmp/bad/bin" \
    PATH="$fake:$PATH" \
    "$repo_root/install.sh" >/dev/null 2>&1; then
    echo "installer accepted a checksum mismatch" >&2
    exit 1
fi

echo "installer selftest passed"
