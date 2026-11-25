#!/usr/bin/env python3
"""Measure baton startup and fail a regression above a recorded baseline.

The numbers are machine-specific. `--check` refuses a baseline recorded on a
different machine unless `--force` is passed, because comparing across hosts
measures the host, not the change.

`--check` also refuses a baseline without a recorded control spawn, because a
busy host can slow every process by 20 percent or more while the product is
unchanged. Each sample times a control process next to `baton --version`, and
the compared number is their minimum ratio, which rejects noise that only adds
time. The whole sample repeats three times by default and the minimum ratio and
phase values across repeats are compared, because one batch can catch a
transient slow or fast control spawn. Phase checks add `--slack-ms` so a
sub-millisecond phase is not gated inside its own measurement noise.
"""

import argparse
import json
import logging
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

LOGGER = logging.getLogger("bench")
PHASES = ("config", "store", "reap", "hydrate", "total", "process")


def control_command() -> list[str]:
    if os.name == "nt":
        return ["cmd", "/c", "exit"]
    for candidate in ("/usr/bin/true", "/bin/true"):
        if Path(candidate).exists():
            return [candidate]
    found = shutil.which("true")
    return [found] if found else []


def run_once(command: list[str], home: Path) -> float:
    start = time.perf_counter()
    subprocess.run(
        command,
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


def summarise_min(samples: list[dict[str, float]]) -> dict[str, float]:
    return {phase: min(s[phase] for s in samples) for phase in PHASES}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=Path("target/debug/baton"))
    parser.add_argument("--runs", type=int, default=9)
    parser.add_argument(
        "--repeat",
        type=int,
        default=3,
        help="whole-sample repetitions; the minimum ratio and phases are compared",
    )
    parser.add_argument("--check", type=Path, help="baseline JSON to compare against")
    parser.add_argument("--tolerance", type=float, default=0.10)
    parser.add_argument(
        "--resolution-ms",
        type=float,
        default=0.1,
        help="phases recorded below this minimum are reported but not enforced",
    )
    parser.add_argument(
        "--slack-ms",
        type=float,
        default=0.1,
        help="absolute slack added to each enforced phase limit for measurement noise",
    )
    parser.add_argument("--force", action="store_true", help="compare across machines")
    parser.add_argument("--write-baseline", type=Path)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    logging.basicConfig(level=logging.INFO, format="%(message)s")
    if not args.binary.exists():
        LOGGER.error("%s does not exist; run `cargo build --bin baton`", args.binary)
        return 2

    machine = f"{platform.system()}-{platform.machine()}"
    control = control_command()
    repeats = []

    def measure() -> dict[str, object]:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            starts = [
                run_once([str(args.binary), "--version"], home)
                for _ in range(args.runs)
            ]
            controls = (
                [run_once(control, home) for _ in range(args.runs)] if control else []
            )
            phase_samples = [
                startup_sample(args.binary, home) for _ in range(args.runs)
            ]
            startup = summarise(phase_samples)
            startup_min = summarise_min(phase_samples)

        process_start_ms = statistics.median(starts)
        process_start_min_ms = min(starts)
        control_start_ms = statistics.median(controls) if controls else None
        control_start_min_ms = min(controls) if controls else None
        spawn_overhead_ratio = (
            process_start_ms / control_start_ms if control_start_ms else None
        )
        spawn_overhead_min_ratio = (
            process_start_min_ms / control_start_min_ms
            if control_start_min_ms
            else None
        )
        return {
            "machine": machine,
            "binary": str(args.binary),
            "runs": args.runs,
            "repeats": args.repeat,
            "process_start_ms": process_start_ms,
            "process_start_min_ms": process_start_min_ms,
            "process_start_p95_ms": sorted(starts)[
                max(0, int(len(starts) * 0.95) - 1)
            ],
            "control_command": control[0] if control else None,
            "control_start_ms": control_start_ms,
            "control_start_min_ms": control_start_min_ms,
            "spawn_overhead_ratio": spawn_overhead_ratio,
            "spawn_overhead_min_ratio": spawn_overhead_min_ratio,
            "startup_ms": startup,
            "startup_min_ms": startup_min,
        }

    for _ in range(max(1, args.repeat)):
        repeats.append(measure())

    ratios = [
        sample["spawn_overhead_min_ratio"]
        for sample in repeats
        if sample["spawn_overhead_min_ratio"] is not None
    ]
    best = (
        min(
            repeats,
            key=lambda sample: sample["spawn_overhead_min_ratio"]
            if sample["spawn_overhead_min_ratio"] is not None
            else float("inf"),
        )
        if ratios
        else repeats[0]
    )
    result = dict(best)
    result["spawn_overhead_min_ratio"] = min(ratios) if ratios else None
    result["startup_min_ms"] = {
        phase: min(sample["startup_min_ms"][phase] for sample in repeats)
        for phase in PHASES
    }
    process_start_ms = result["process_start_ms"]
    process_start_min_ms = result["process_start_min_ms"]
    control_start_min_ms = result["control_start_min_ms"]
    spawn_overhead_min_ratio = result["spawn_overhead_min_ratio"]
    startup = result["startup_ms"]
    startup_min = result["startup_min_ms"]

    if args.write_baseline:
        args.write_baseline.write_text(json.dumps(result, indent=2) + "\n")
        LOGGER.info("wrote %s", args.write_baseline)
    if args.json:
        print(json.dumps(result, indent=2))

    if not args.check:
        if not args.json:
            LOGGER.info(
                "process start p50 %.1f ms, p95 %.1f ms, control %.2f ms, ratio %.2f; phases %s",
                result["process_start_ms"],
                result["process_start_p95_ms"],
                result["control_start_ms"] or 0.0,
                result["spawn_overhead_ratio"] or 0.0,
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

    recorded_ratio = baseline.get("spawn_overhead_min_ratio")
    if spawn_overhead_min_ratio is None:
        LOGGER.error("no control process is available; cannot normalize process start")
        return 2
    if recorded_ratio is None:
        LOGGER.error(
            "baseline %s predates the control spawn; re-record it with --write-baseline",
            args.check,
        )
        return 2
    LOGGER.info(
        "process start min %.2f ms on a %.2f ms control spawn (ratio %.2f, baseline %.2f); p50 %.1f ms",
        process_start_min_ms,
        control_start_min_ms,
        spawn_overhead_min_ratio,
        recorded_ratio,
        process_start_ms,
    )

    comparisons = [("spawn_overhead_min_ratio", spawn_overhead_min_ratio, recorded_ratio)]
    for phase in PHASES:
        if phase == "process":
            LOGGER.info(
                "startup.process %.2f ms includes runtime init outside Baton's work and is reported, not enforced",
                startup[phase],
            )
            continue
        recorded = baseline["startup_min_ms"][phase]
        if recorded < args.resolution_ms:
            LOGGER.info(
                "startup.%s recorded at %.3f ms is below the %.2f ms resolution floor; not enforced",
                phase,
                recorded,
                args.resolution_ms,
            )
            continue
        comparisons.append((f"startup.{phase}", startup_min[phase], recorded))

    failures = []
    for label, current, recorded in comparisons:
        slack = args.slack_ms if label.startswith("startup.") else 0.0
        limit = recorded * (1.0 + args.tolerance) + slack
        if current > limit:
            unit = "" if label == "spawn_overhead_min_ratio" else " ms"
            failures.append(
                f"{label} {current:.2f}{unit} > {limit:.2f}{unit} ({recorded:.2f} baseline)"
            )

    for failure in failures:
        LOGGER.error("regression: %s", failure)
    if failures:
        return 1
    LOGGER.info("within %.0f%% of the recorded baseline", args.tolerance * 100)
    return 0


if __name__ == "__main__":
    sys.exit(main())
