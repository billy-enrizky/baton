#!/usr/bin/env python3
"""Point the exported docs at the production origin and verify the export."""

from __future__ import annotations

import argparse
import html
import re
import sys
from pathlib import Path
from urllib.parse import urlparse

CANONICAL = re.compile(r'<link data-mint-head rel="canonical" href="([^"]+)">')
OG_URL = re.compile(r'<meta data-mint-head property="og:url" content="([^"]+)">')
FAVICON = re.compile(
    r'<link data-mint-head rel="icon" type="image/png" sizes="32x32" href="([^"]+)">'
)
BROWSERCONFIG = re.compile(
    r'<meta data-mint-head name="msapplication-config"[^>]*>'
)
BROWSERCONFIG_ESCAPED = re.compile(
    r'\\u003cmeta data-mint-head name=\\"msapplication-config\\".*?\\">'
)


def replace_origin(text: str, site_url: str) -> str:
    text = text.replace("https://local.", site_url)
    text = text.replace('"subdomain":"localhost"', f'"subdomain":"{urlparse(site_url).hostname}"')
    text = BROWSERCONFIG.sub("", text)
    return BROWSERCONFIG_ESCAPED.sub("", text)


def write_sitemap(docs_root: Path, urls: list[str]) -> None:
    rows = "\n".join(f"  <url><loc>{html.escape(url)}</loc></url>" for url in urls)
    body = (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n'
        f"{rows}\n"
        "</urlset>\n"
    )
    (docs_root / "sitemap.xml").write_text(body)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--docs-root", default="site/docs")
    parser.add_argument("--site-root", default="site")
    parser.add_argument("--site-url", default="https://baton.openbrowser.me")
    parser.add_argument("--base-path", default="/docs")
    args = parser.parse_args()

    site_url = args.site_url.rstrip("/")
    base_path = "/" + args.base_path.strip("/")
    docs_root = Path(args.docs_root)
    site_root = Path(args.site_root)
    if not docs_root.is_dir():
        print(f"missing docs export: {docs_root}", file=sys.stderr)
        return 2

    pages = sorted(docs_root.rglob("index.html"))
    if not pages:
        print(f"no pages in {docs_root}", file=sys.stderr)
        return 1

    errors: list[str] = []
    generated_favicon = docs_root / "favicons" / "favicon-32x32.png"
    if not generated_favicon.is_file():
        errors.append(f"missing {generated_favicon}")
    generated_ico = docs_root / "favicons" / "favicon.ico"
    if not generated_ico.is_file():
        errors.append(f"missing {generated_ico}")
    elif not generated_ico.read_bytes().startswith(b"\x00\x00\x01\x00"):
        errors.append(f"{generated_ico} is not an ICO file")

    canonicals: set[str] = set()
    for page in pages:
        path = page.resolve()
        text = replace_origin(path.read_text(encoding="utf-8"), site_url)
        path.write_text(text, encoding="utf-8")

        relative = page.parent.relative_to(docs_root).as_posix()
        label = "docs" if relative == "." else f"docs/{relative}"
        if (
            "https://local." in text
            or '"subdomain":"localhost"' in text
            or "mintlify.s3.us-west-1.amazonaws.com/local/" in text
        ):
            errors.append(f"{label}: local origin remains")
        canonical = CANONICAL.search(text)
        if canonical is None:
            errors.append(f"{label}: no canonical link")
            continue
        canonical_url = canonical.group(1)
        if not canonical_url.startswith(site_url + base_path):
            errors.append(f"{label}: canonical is {canonical_url}")
        if urlparse(canonical_url).netloc != urlparse(site_url).netloc:
            errors.append(f"{label}: canonical host is not the production host")
        canonicals.add(canonical_url)

        og_url = OG_URL.search(text)
        if og_url is None:
            errors.append(f"{label}: no og:url")
        elif og_url.group(1) != canonical_url:
            errors.append(f"{label}: og:url does not match canonical")
        favicon = FAVICON.search(text)
        expected_favicon = f"{base_path}/favicons/favicon-32x32.png"
        if favicon is None:
            errors.append(f"{label}: no PNG favicon")
        elif favicon.group(1) != expected_favicon:
            errors.append(f"{label}: favicon is {favicon.group(1)}")

    urls = sorted(canonicals)
    write_sitemap(docs_root, urls)

    ico = site_root / "favicon.ico"
    if not ico.is_file():
        errors.append(f"missing {ico}")
    elif not ico.read_bytes().startswith(b"\x00\x00\x01\x00"):
        errors.append(f"{ico} is not an ICO file")

    robots = site_root / "robots.txt"
    expected_sitemap = f"Sitemap: {site_url}{base_path}/sitemap.xml"
    if not robots.is_file():
        errors.append(f"missing {robots}")
    elif expected_sitemap not in robots.read_text(encoding="utf-8"):
        errors.append(f"{robots} does not name {expected_sitemap}")

    remaining = sorted(
        path.relative_to(docs_root).as_posix()
        for path in docs_root.rglob("*")
        if path.is_file() and path.suffix in {".html", ".json", ".txt", ".xml"}
        and any(
            marker in path.read_text(encoding="utf-8", errors="replace")
            for marker in (
                "https://local.",
                '"subdomain":"localhost"',
                "mintlify.s3.us-west-1.amazonaws.com/local/",
            )
        )
    )
    errors.extend(f"{path}: local origin remains" for path in remaining)

    if errors:
        print("docs export validation failed", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1

    print(f"docs export: {len(pages)} pages, {len(urls)} canonical URLs, production origin {site_url}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
