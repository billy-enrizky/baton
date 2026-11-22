#!/usr/bin/env python3
"""Check local Markdown links in the maintained Baton documents."""

import argparse
import logging
import re
import sys
from pathlib import Path


LOGGER = logging.getLogger("check_doc_links")
LINK = re.compile(r"\[[^\]]+\]\(([^)]+)\)")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("documents", nargs="*", type=Path, default=[
        Path("README.md"),
        Path("docs/parity.md"),
        Path("docs/ci.md"),
        Path("docs/ci-acceptance.md"),
    ])
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()

    logging.basicConfig(level=logging.INFO if args.verbose else logging.WARNING, format="%(message)s")
    missing: list[tuple[str, str]] = []
    checked = 0
    for document in args.documents:
        if not document.is_file():
            missing.append((str(document), "document missing"))
            continue
        for target in LINK.findall(document.read_text()):
            if target.startswith(("http://", "https://", "mailto:", "#")):
                continue
            path = target.split("#", 1)[0]
            if not path:
                continue
            checked += 1
            resolved = (document.parent / path).resolve()
            if not resolved.exists():
                missing.append((str(document), target))
            elif args.verbose:
                LOGGER.info("ok %s -> %s", document, target)

    for document, target in missing:
        LOGGER.error("%s missing local link %s", document, target)
    LOGGER.warning("checked %s local links, %s missing", checked, len(missing))
    return 0 if not missing else 1


if __name__ == "__main__":
    sys.exit(main())
