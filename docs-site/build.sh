#!/bin/sh
# Validate the Mintlify source and render the static export that Cloudflare
# Pages serves under /docs. Mintlify hosting itself cannot serve a subpath.
set -eu

root=$(cd -- "$(dirname -- "$0")" && pwd)
cd "$root"

npx -y mint@latest validate
npx -y mint@latest export --base-path /docs --output "$root/docs-export.zip"

work=$(mktemp -d)
unzip -q "$root/docs-export.zip" -d "$work"
mkdir -p "$root/../site/docs"
rsync -a --delete \
  --exclude 'README.md' \
  --exclude 'serve.js' \
  --exclude 'Start Docs.command' \
  --exclude 'Start Docs.bat' \
  "$work/" "$root/../site/docs/"
rm -f "$root/docs-export.zip"
printf 'docs built into %s\n' "$root/../site/docs"
