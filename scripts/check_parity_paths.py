#!/usr/bin/env python3
"""Verify the upstream and Baton paths cited by docs/parity.md, and the status vocabulary."""

import argparse
import logging
import re
import subprocess
import sys
from pathlib import Path


LOGGER = logging.getLogger("check_parity_paths")
UPSTREAM_ROW = re.compile(r"^\| (T3 Code|Orca) \|.*\| `([^`]+)` \| `origin/main` \| `([0-9a-f]{40})` \|$")
PATH_TOKEN = re.compile(r"`([^`]+)`")
BATON_PATH = re.compile(r"^(src|tests|docs|scripts|ci)/[\w./-]+$")
STATUSES = {
    "Equivalent",
    "Partial",
    "Missing",
    "Unmeasured",
    "Design decision",
    "N/A in a TUI",
}


def parse_upstreams(document: Path) -> dict[str, tuple[Path, str]]:
    upstreams: dict[str, tuple[Path, str]] = {}
    for line in document.read_text().splitlines():
        match = UPSTREAM_ROW.match(line)
        if match:
            name, checkout, commit = match.groups()
            upstreams[name] = (Path(checkout), commit)
    missing = {"T3 Code", "Orca"} - set(upstreams)
    if missing:
        raise ValueError(f"missing upstream rows: {sorted(missing)}")
    return upstreams


def iter_evidence(document: Path):
    for line_number, line in enumerate(document.read_text().splitlines(), 1):
        if not line.startswith("|") or line.startswith("|---"):
            continue
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        if len(cells) != 7:
            continue
        for group in cells[1].split(";"):
            match = re.match(r"(T3|Orca):\s*(.*)", group.strip())
            if not match:
                continue
            label = "T3 Code" if match.group(1) == "T3" else "Orca"
            for path in PATH_TOKEN.findall(match.group(2)):
                yield line_number, label, path


def iter_baton_evidence(document: Path):
    for line_number, line in enumerate(document.read_text().splitlines(), 1):
        if not line.startswith("|") or line.startswith("|---"):
            continue
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        if len(cells) != 7:
            continue
        for path in PATH_TOKEN.findall(cells[3]):
            if BATON_PATH.match(path):
                yield line_number, path


DELIVERY_ROW = re.compile(r"^\| (D[0-9]+[a-z]?) \|")
PLAN_CODE = re.compile(r"\bD[0-9]+[a-z]?\b")


def iter_plan_codes(document: Path):
    for line_number, line in enumerate(document.read_text().splitlines(), 1):
        if not line.startswith("| ") or line.startswith("|---") or line.count("|") != 8:
            continue
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        if len(cells) != 7:
            continue
        for code in PLAN_CODE.findall(cells[6]):
            yield line_number, code


def delivery_codes(document: Path) -> set[str]:
    codes = set()
    for line in document.read_text().splitlines():
        match = DELIVERY_ROW.match(line)
        if match:
            codes.add(match.group(1))
    return codes


def iter_statuses(document: Path):
    for line_number, line in enumerate(document.read_text().splitlines(), 1):
        if not line.startswith("|") or line.startswith("|---"):
            continue
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        if len(cells) == 7 and cells[2] not in {"", "Baton status"}:
            yield line_number, cells[2]


def exists(checkout: Path, commit: str, path: str) -> bool:
    result = subprocess.run(
        ["git", "-C", str(checkout), "cat-file", "-e", f"{commit}:{path}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return result.returncode == 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--document", type=Path, default=Path("docs/parity.md"))
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()

    logging.basicConfig(level=logging.INFO if args.verbose else logging.WARNING, format="%(message)s")
    try:
        upstreams = parse_upstreams(args.document)
    except (OSError, ValueError) as error:
        LOGGER.error("%s", error)
        return 2

    checked = 0
    missing: list[tuple[int, str, str]] = []
    for line_number, label, path in iter_evidence(args.document):
        checkout, commit = upstreams[label]
        checked += 1
        if not exists(checkout, commit, path):
            missing.append((line_number, label, path))
        elif args.verbose:
            LOGGER.info("ok %s %s", label, path)

    for line_number, label, path in missing:
        LOGGER.error("docs/parity.md:%s missing %s path %s", line_number, label, path)

    baton_checked = 0
    baton_missing: list[tuple[int, str]] = []
    for line_number, path in iter_baton_evidence(args.document):
        baton_checked += 1
        if not (args.repo / path).exists():
            baton_missing.append((line_number, path))
        elif args.verbose:
            LOGGER.info("ok baton %s", path)

    for line_number, path in baton_missing:
        LOGGER.error("docs/parity.md:%s missing Baton path %s", line_number, path)

    defined = delivery_codes(args.document)
    bad_code: list[tuple[int, str]] = []
    for line_number, code in iter_plan_codes(args.document):
        if code not in defined:
            bad_code.append((line_number, code))

    for line_number, code in bad_code:
        LOGGER.error("docs/parity.md:%s Plan references undefined slice %s", line_number, code)

    bad_status: list[tuple[int, str]] = []
    for line_number, status in iter_statuses(args.document):
        if status not in STATUSES:
            bad_status.append((line_number, status))

    for line_number, status in bad_status:
        LOGGER.error("docs/parity.md:%s unknown status %s", line_number, status)

    LOGGER.warning(
        "checked %s upstream paths, %s Baton paths, %s statuses, %s slice references; %s missing, %s unknown statuses, %s undefined slices",
        checked,
        baton_checked,
        checked,
        len(defined),
        len(missing) + len(baton_missing),
        len(bad_status),
        len(bad_code),
    )
    return 0 if not (missing or baton_missing or bad_status or bad_code) else 1


if __name__ == "__main__":
    sys.exit(main())
