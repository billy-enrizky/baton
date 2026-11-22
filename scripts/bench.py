#!/usr/bin/env python3
"""Measure baton startup and fail a regression above a recorded baseline.

The numbers are machine-specific. `--check` refuses a baseline recorded on a
different machine unless `--force` is passed, because comparing across hosts
measures the host, not the change.
"""

import argparse
import json
import logging
import platform
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

LOGGER = logging.getLogger("bench")
PHASES = ("config", "store", "reap", "hydrate", "total", "process")


def run_once(binary: Path, args: list[str], home: Path) -> float:
    start = time.perf_counter()
    subprocess.run(
        [str(binary), *args],
        env={"BATON_HOME": str(home), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"},
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return (time.perf_counter() - start) * 1000.0


def startup_sample(binary: Path, home: Path) -> dict[str, float]:
    result = subprocess.run(
        [str(binary), "bench", "startup", "--json", "--repeat", "1"],
        env={"BATON_HOME": str(home), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"},
        capture_output=True,
        text=True,
        check=True,
    )
    payload = json.loads(result.stdout)
    median = payload["median_ms"]
    return {
        "config": median["config"],
        "store": median["store"],
        "reap": median["reap"],
        "hydrate": median["hydrate"],
        "total": median["total"],
        "process": payload["process_ms"],
    }


def summarise(samples: list[dict[str, float]]) -> dict[str, float]:
    return {phase: statistics.median(s[phase] for s in samples) for phase in PHASES}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=Path("target/debug/baton"))
    parser.add_argument("--runs", type=int, default=9)
    parser.add_argument("--check", type=Path, help="baseline JSON to compare against")
    parser.add_argument("--tolerance", type=float, default=0.10)
    parser.add_argument("--force", action="store_true", help="compare across machines")
    parser.add_argument("--write-baseline", type=Path)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    logging.basicConfig(level=logging.INFO, format="%(message)s")
    if not args.binary.exists():
        LOGGER.error("%s does not exist; run `cargo build --bin baton`", args.binary)
        return 2

    machine = f"{platform.system()}-{platform.machine()}"
    with tempfile.TemporaryDirectory() as tmp:
        home = Path(tmp)
        starts = [run_once(args.binary, ["--version"], home) for _ in range(args.runs)]
        startup = summarise([startup_sample(args.binary, home) for _ in range(max(3, args.runs // 3))])

    result = {
        "machine": machine,
        "binary": str(args.binary),
        "runs": args.runs,
        "process_start_ms": statistics.median(starts),
        "process_start_p95_ms": sorted(starts)[max(0, int(len(starts) * 0.95) - 1)],
        "startup_ms": startup,
    }

    if args.write_baseline:
        args.write_baseline.write_text(json.dumps(result, indent=2) + "\n")
        LOGGER.info("wrote %s", args.write_baseline)
    if args.json:
        print(json.dumps(result, indent=2))

    if not args.check:
        if not args.json:
            LOGGER.info(
                "process start p50 %.1f ms, p95 %.1f ms; phases %s",
                result["process_start_ms"],
                result["process_start_p95_ms"],
                {k: round(v, 2) for k, v in startup.items()},
            )
        return 0

    baseline = json.loads(args.check.read_text())
    if baseline["machine"] != machine and not args.force:
        LOGGER.error(
            "baseline is from %s, this host is %s; pass --force to compare anyway",
            baseline["machine"],
            machine,
        )
        return 2

    failures = []
    for label, current, recorded in (
        ("process_start_ms", result["process_start_ms"], baseline["process_start_ms"]),
        *((f"startup.{phase}", startup[phase], baseline["startup_ms"][phase]) for phase in PHASES),
    ):
        limit = recorded * (1.0 + args.tolerance)
        if current > limit:
            failures.append(f"{label} {current:.2f} ms > {limit:.2f} ms ({recorded:.2f} baseline)")

    for failure in failures:
        LOGGER.error("regression: %s", failure)
    if failures:
        return 1
    LOGGER.info("within %.0f%% of the recorded baseline", args.tolerance * 100)
    return 0


if __name__ == "__main__":
    sys.exit(main())
