#!/usr/bin/env python3
"""Verify the upstream and Baton paths cited by docs/parity.md, and the status vocabulary."""

import argparse
import logging
import os
import re
import subprocess
import sys
from pathlib import Path


LOGGER = logging.getLogger("check_parity_paths")
UPSTREAM_ROW = re.compile(
    r"^\| (T3 Code|Orca|Codex) \|.*\| `([^`]+)` \| `origin/main` \| `([0-9a-f]{40})` \|$"
)
PATH_TOKEN = re.compile(r"`([^`]+)`")
BATON_PATH = re.compile(
    r"^(?:(?:src|tests|docs|scripts|ci|\.agents|\.github)/[\w./-]+|"
    r"Cargo\.toml|Cargo\.lock|README\.md|install\.sh|install\.ps1)$"
)
STATUSES = {
    "Equivalent",
    "Partial",
    "Missing",
    "Unmeasured",
    "Design decision",
    "N/A in a TUI",
}


def parse_table_cells(line: str) -> list[str]:
    stripped = line.strip()
    if stripped.startswith("|") and stripped.endswith("|"):
        stripped = stripped[1:-1]
    return [cell.strip() for cell in stripped.split("|")]


def parse_upstreams(document: Path) -> dict[str, tuple[Path, str]]:
    upstreams: dict[str, tuple[Path, str]] = {}
    for line in document.read_text().splitlines():
        match = UPSTREAM_ROW.match(line)
        if match:
            name, checkout, commit = match.groups()
            if checkout.startswith("$"):
                variable = checkout[1:]
                resolved = os.environ.get(variable)
                if not resolved:
                    raise ValueError(
                        f"missing checkout environment variable {variable}; "
                        "set it to the upstream repository path"
                    )
                upstreams[name] = (Path(resolved).expanduser(), commit)
            else:
                upstreams[name] = (Path(checkout).expanduser(), commit)
    missing = {"T3 Code", "Orca", "Codex"} - set(upstreams)
    if missing:
        raise ValueError(f"missing upstream rows: {sorted(missing)}")
    return upstreams


def iter_evidence(document: Path):
    for line_number, line in enumerate(document.read_text().splitlines(), 1):
        if not line.startswith("|") or line.startswith("|---"):
            continue
        cells = parse_table_cells(line)
        if len(cells) != 7:
            continue
        for group in cells[1].split(";"):
            match = re.match(r"(T3|Orca|Codex):\s*(.*)", group.strip())
            if not match:
                continue
            label = {"T3": "T3 Code", "Orca": "Orca", "Codex": "Codex"}[match.group(1)]
            for path in PATH_TOKEN.findall(match.group(2)):
                yield line_number, label, path


def iter_baton_evidence(document: Path):
    for line_number, line in enumerate(document.read_text().splitlines(), 1):
        if not line.startswith("|") or line.startswith("|---"):
            continue
        cells = parse_table_cells(line)
        if len(cells) != 7:
            continue
        for path in PATH_TOKEN.findall(cells[3]):
            if BATON_PATH.match(path):
                yield line_number, path


def iter_unprefixed_evidence(document: Path):
    for line_number, line in enumerate(document.read_text().splitlines(), 1):
        if not line.startswith("|") or line.startswith("|---"):
            continue
        cells = parse_table_cells(line)
        if len(cells) != 7:
            continue
        for group in cells[1].split(";"):
            claim = group.strip()
            if re.match(r"(T3|Orca|Codex):", claim):
                continue
            has_upstream_path = any(
                UPSTREAM_PATH.match(path) for path in PATH_TOKEN.findall(claim)
            )
            if UPSTREAM_MENTION.match(claim) or has_upstream_path:
                yield line_number, claim


def iter_bad_upstream_evidence(document: Path):
    for line_number, line in enumerate(document.read_text().splitlines(), 1):
        if not line.startswith("|") or line.startswith("|---"):
            continue
        cells = parse_table_cells(line)
        if len(cells) != 7:
            continue
        for group in cells[1].split(";"):
            claim = group.strip()
            match = re.match(r"(T3|Orca|Codex):\s*(.*)", claim)
            if not match:
                continue
            body = match.group(2)
            if not PATH_TOKEN.findall(body):
                yield line_number, claim, "no backticked path"
                continue
            unquoted = PATH_TOKEN.sub("", body)
            if re.search(r"(?:apps|packages|src|docs|codex-rs|mobile)/[\w./-]+", unquoted):
                yield line_number, claim, "unquoted path"


DELIVERY_ROW = re.compile(r"^\| (D[0-9]+[a-z]?) \|")
PLAN_CODE = re.compile(r"\bD[0-9]+[a-z]?\b")
UPSTREAM_MENTION = re.compile(r"^(T3 Code|T3|Orca|Codex)\b")
UPSTREAM_PATH = re.compile(
    r"^(?:(?:apps|packages|src|docs|codex-rs|mobile)/|[\w.-]+\.(?:json|ts|tsx|md|mjs|toml)$)"
)


def iter_plan_codes(document: Path):
    for line_number, line in enumerate(document.read_text().splitlines(), 1):
        if not line.startswith("| ") or line.startswith("|---"):
            continue
        cells = parse_table_cells(line)
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
        cells = parse_table_cells(line)
        if len(cells) == 7 and cells[2] not in {"", "Baton status"}:
            yield line_number, cells[2]


def iter_broken_capability_rows(document: Path):
    in_capability_table = False
    for line_number, line in enumerate(document.read_text().splitlines(), 1):
        if line.startswith("| Capability | Upstream evidence | Baton status |"):
            in_capability_table = True
            continue
        if not in_capability_table:
            continue
        if not line.strip():
            in_capability_table = False
            continue
        if not line.startswith("|"):
            in_capability_table = False
            continue
        cells = parse_table_cells(line)
        if len(cells) != 7:
            yield line_number, len(cells)


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

    unprefixed = list(iter_unprefixed_evidence(args.document))
    for line_number, claim in unprefixed:
        LOGGER.error(
            "docs/parity.md:%s upstream evidence needs a T3:, Orca:, or Codex: prefix: %s",
            line_number,
            claim,
        )

    bad_upstream = list(iter_bad_upstream_evidence(args.document))
    for line_number, claim, reason in bad_upstream:
        LOGGER.error(
            "docs/parity.md:%s upstream evidence has %s: %s",
            line_number,
            reason,
            claim,
        )

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
    plan_refs = list(iter_plan_codes(args.document))
    bad_code: list[tuple[int, str]] = []
    for line_number, code in plan_refs:
        if code not in defined:
            bad_code.append((line_number, code))

    for line_number, code in bad_code:
        LOGGER.error("docs/parity.md:%s Plan references undefined slice %s", line_number, code)

    statuses = list(iter_statuses(args.document))
    bad_status: list[tuple[int, str]] = []
    for line_number, status in statuses:
        if status not in STATUSES:
            bad_status.append((line_number, status))

    for line_number, status in bad_status:
        LOGGER.error("docs/parity.md:%s unknown status %s", line_number, status)

    broken_rows = list(iter_broken_capability_rows(args.document))
    for line_number, cell_count in broken_rows:
        LOGGER.error(
            "docs/parity.md:%s capability row has %s cells, expected 7",
            line_number,
            cell_count,
        )

    LOGGER.warning(
        "checked %s upstream paths, %s Baton paths, %s statuses, %s plan references; %s missing, %s unprefixed, %s bad upstream, %s broken rows, %s unknown statuses, %s undefined slices",
        checked,
        baton_checked,
        len(statuses),
        len(plan_refs),
        len(missing) + len(baton_missing),
        len(unprefixed),
        len(bad_upstream),
        len(broken_rows),
        len(bad_status),
        len(bad_code),
    )
    return (
        0
        if not (
            missing
            or baton_missing
            or unprefixed
            or broken_rows
            or bad_status
            or bad_code
        )
        else 1
    )


if __name__ == "__main__":
    sys.exit(main())
