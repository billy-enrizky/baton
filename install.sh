#!/bin/sh
set -eu

REPO="${BATON_REPO:-billy-enrizky/baton}"
VERSION="${BATON_VERSION:-latest}"
INSTALL_DIR="${BATON_INSTALL_DIR:-${HOME}/.local/bin}"

say() {
    printf '%s\n' "$*"
}

warn() {
    printf 'warning: %s\n' "$*" >&2
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

download() {
    url=$1
    dest=$2
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 --retry-delay 1 --connect-timeout 10 --max-time 120 "$url" -o "$dest"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --tries=3 --timeout=30 -O "$dest" "$url"
    else
        return 1
    fi
}

sha256_file() {
    file=$1
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{print $1}'
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 "$file" | awk '{print $NF}'
    else
        fail "no SHA-256 tool found; install coreutils or openssl"
    fi
}

case "$INSTALL_DIR" in
    */bin | */bin/) ;;
    *) fail "BATON_INSTALL_DIR must be a bin directory, got $INSTALL_DIR" ;;
esac

os=$(uname -s)
arch=$(uname -m)
case "$os" in
    Darwin)
        target_os=apple-darwin
        archive_ext=tar.gz
        binary_name=baton
        ;;
    Linux)
        target_os=unknown-linux-gnu
        archive_ext=tar.gz
        binary_name=baton
        ;;
    MINGW* | MSYS* | CYGWIN*)
        target_os=pc-windows-msvc
        archive_ext=zip
        binary_name=baton.exe
        ;;
    *)
        fail "unsupported operating system: $os"
        ;;
esac

case "$arch" in
    x86_64 | amd64 | AMD64)
        target_arch=x86_64
        ;;
    arm64 | aarch64 | ARM64)
        target_arch=aarch64
        ;;
    *)
        fail "unsupported CPU architecture: $arch"
        ;;
esac

case "$target_os:$target_arch" in
    pc-windows-msvc:aarch64)
        fail "Windows on ARM is not published yet; use install.ps1 on an x64 Windows machine"
        ;;
esac

target="${target_arch}-${target_os}"
asset="baton-${target}.${archive_ext}"

if [ "$VERSION" = latest ]; then
    base_url="https://github.com/${REPO}/releases/latest/download"
else
    case "$VERSION" in
        v*) ;;
        *) VERSION="v${VERSION}" ;;
    esac
    base_url="https://github.com/${REPO}/releases/download/${VERSION}"
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/baton-install.XXXXXX")
trap 'rm -rf "$tmp"' 0 1 2 15

say "installing Baton for ${target}"

if download "${base_url}/${asset}" "${tmp}/${asset}"; then
    if ! download "${base_url}/SHA256SUMS" "${tmp}/SHA256SUMS"; then
        fail "downloaded ${asset}, but its SHA256SUMS file is missing"
    fi
    expected=$(awk -v name="$asset" '{ file=$2; sub(/^\.\//, "", file); if (file == name) { print $1; exit } }' "${tmp}/SHA256SUMS")
    [ -n "$expected" ] || fail "${asset} is missing from SHA256SUMS"
    actual=$(sha256_file "${tmp}/${asset}")
    [ "$actual" = "$expected" ] || fail "checksum mismatch for ${asset}"

    case "$archive_ext" in
        zip)
            command -v unzip >/dev/null 2>&1 || fail "unzip is required for the Windows archive"
            unzip -q "${tmp}/${asset}" -d "${tmp}"
            ;;
        tar.gz)
            tar -xzf "${tmp}/${asset}" -C "${tmp}"
            ;;
    esac
    [ -f "${tmp}/${binary_name}" ] || fail "${binary_name} is missing from ${asset}"
    mkdir -p "$INSTALL_DIR"
    if command -v install >/dev/null 2>&1; then
        install -m 755 "${tmp}/${binary_name}" "${INSTALL_DIR}/${binary_name}"
    else
        cp "${tmp}/${binary_name}" "${INSTALL_DIR}/${binary_name}"
        chmod 755 "${INSTALL_DIR}/${binary_name}"
    fi
else
    warn "no prebuilt release for ${target}; trying a source build"
    command -v cargo >/dev/null 2>&1 || fail "install Rust from https://rustup.rs, then run this script again"
    command -v git >/dev/null 2>&1 || fail "git is required for a source build"
    prefix=$(dirname "$INSTALL_DIR")
    cargo install --git "https://github.com/${REPO}" --locked --root "$prefix"
    [ -x "${INSTALL_DIR}/${binary_name}" ] || fail "cargo finished without installing ${INSTALL_DIR}/${binary_name}"
fi

"${INSTALL_DIR}/${binary_name}" --version
say ""
say "Baton is installed at ${INSTALL_DIR}/${binary_name}"
case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
        say "Add it to PATH if needed:"
        say "  export PATH=\"${INSTALL_DIR}:\$PATH\""
        ;;
esac
say "Harness CLIs you want to drive still need to be on PATH. Run: baton doctor"
