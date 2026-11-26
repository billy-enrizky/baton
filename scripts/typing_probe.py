#!/usr/bin/env python3
"""Measure keystroke-to-quiet in the real TUI through a PTY.

The TUI repaints on every frame, so "rendered" means the output stream went
quiet after a keystroke. That is a proxy for keystroke-to-render that needs no
instrumentation inside the app, and it stays honest: if a repaint is slow to
start, the quiet window starts later.

Each probe types 80 samples by cycling the sample text; three whole-probe
repetitions are compared by their minimum p50 and p95. The larger sample keeps
the p95 stable enough for a 10 percent gate on a shared host. The slash mode
also times a plain character repaint before each slash and gates the
slash-to-control ratio of first responses, so host load that slows both key
paths cancels. Metrics recorded below `--resolution-ms` are reported but not
enforced, because a sub-2 ms p95 moves by more than the 10 percent band between
identical runs on a shared host.
"""

import argparse
import fcntl
import platform
import json
import os
import pty
import select
import signal
import statistics
import struct
import sys
import tempfile
import termios
import time
from pathlib import Path

QUIET_MS = 6.0


def set_size(fd: int, width: int, height: int) -> None:
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))


def drain(fd: int, quiet_ms: float, timeout_s: float) -> float:
    """Read until the stream is quiet for quiet_ms. Returns elapsed seconds."""
    start = time.perf_counter()
    deadline = start + timeout_s
    last = start
    while time.perf_counter() < deadline:
        remaining = quiet_ms / 1000.0
        ready, _, _ = select.select([fd], [], [], remaining)
        now = time.perf_counter()
        if not ready:
            if (now - last) * 1000.0 >= quiet_ms:
                break
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        last = now
    return time.perf_counter() - start


def wait_for_first_output(fd: int, timeout_s: float) -> tuple[bool, bytes]:
    """Reads until the first chunk arrives or the deadline passes."""
    deadline = time.perf_counter() + timeout_s
    while time.perf_counter() < deadline:
        ready, _, _ = select.select([fd], [], [], 0.05)
        if not ready:
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            return False, b""
        if not chunk:
            return False, b""
        return True, chunk
    return False, b""


def wait_for_text(fd: int, needle: bytes, timeout_s: float) -> bool:
    """Reads until the needle appears in the stream or the deadline passes."""
    deadline = time.perf_counter() + timeout_s
    buf = b""
    while time.perf_counter() < deadline:
        ready, _, _ = select.select([fd], [], [], 0.05)
        if not ready:
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            return False
        if not chunk:
            return False
        buf += chunk
        if needle in buf:
            return True
    return False


def reap(pid: int, timeout_s: float) -> int:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            waited, status = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            return 0
        if waited == pid:
            return os.waitstatus_to_exitcode(status)
        time.sleep(0.02)
    os.kill(pid, signal.SIGKILL)
    return -9


def measure(binary: Path, args: argparse.Namespace) -> dict | None:
    """Run the probe once and return its percentiles, or None on a setup failure."""
    home = tempfile.mkdtemp(prefix="baton-typing-home-")
    workdir = tempfile.mkdtemp(prefix="baton-typing-cwd-")
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(workdir)
        env = os.environ.copy()
        env["BATON_HOME"] = home
        env["TERM"] = "xterm-256color"
        os.execvpe(str(binary), [str(binary)], env)

    set_size(master, args.width, args.height)
    samples = []
    controls = []
    firsts = []
    try:
        deadline = time.time() + args.timeout
        seen = b""
        while time.time() < deadline:
            ready, _, _ = select.select([master], [], [], 0.1)
            if ready:
                seen += os.read(master, 65536)
                if args.ready.encode() in seen:
                    break
        else:
            print("TUI did not show the composer", file=sys.stderr)
            return None
        drain(master, QUIET_MS, args.timeout)
        time.sleep(args.warmup)

        if args.slash:
            for _ in range(args.runs):
                # Same composer repaint without the suggestion list. The ratio
                # of the first response to this control cancels host load.
                os.write(master, b"x")
                start = time.perf_counter()
                ok, _chunk = wait_for_first_output(master, args.timeout)
                if not ok:
                    print("composer repaint did not start", file=sys.stderr)
                    return None
                controls.append((time.perf_counter() - start) * 1000.0)
                drain(master, QUIET_MS, 1.0)
                os.write(master, b"\x7f")
                drain(master, QUIET_MS, 1.0)
                start = time.perf_counter()
                os.write(master, b"/")
                ok, chunk = wait_for_first_output(master, args.timeout)
                if not ok:
                    print("suggestion list did not start", file=sys.stderr)
                    return None
                firsts.append((time.perf_counter() - start) * 1000.0)
                if args.suggestion.encode() not in chunk:
                    hit = wait_for_text(
                        master, args.suggestion.encode(), args.timeout
                    )
                    if not hit:
                        print("suggestion list did not render", file=sys.stderr)
                        return None
                samples.append((time.perf_counter() - start) * 1000.0)
                os.write(master, b"\x1b")
                drain(master, QUIET_MS, 1.0)
        else:
            for index in range(args.runs):
                char = args.text[index % len(args.text)]
                os.write(master, char.encode())
                samples.append(drain(master, QUIET_MS, args.timeout) * 1000.0)

        # Two control-c presses within a second is the documented exit, and it
        # leaves the typed composer text alone.
        os.write(master, b"\x03")
        drain(master, QUIET_MS, 1.0)
        os.write(master, b"\x03")
        drain(master, QUIET_MS, 1.0)
        exit_code = reap(pid, 5)
    finally:
        try:
            os.close(master)
        except OSError:
            pass

    if not samples:
        print("no samples collected", file=sys.stderr)
        return None
    ordered = sorted(samples)
    p95_index = max(0, int(len(ordered) * 0.95) - 1)
    result = {
        "machine": f"{platform.system()}-{platform.machine()}",
        "samples": len(samples),
        "p50_ms": statistics.median(samples),
        "p95_ms": ordered[p95_index],
        "max_ms": ordered[-1],
        "exit_code": exit_code,
    }
    if controls:
        ratios = sorted(
            first / control
            for first, control in zip(firsts, controls)
            if control > 0
        )
        control_ordered = sorted(controls)
        result["control_p50_ms"] = statistics.median(controls)
        result["control_p95_ms"] = control_ordered[
            max(0, int(len(control_ordered) * 0.95) - 1)
        ]
        result["ratio_p50"] = statistics.median(ratios)
        result["ratio_p95"] = ratios[max(0, int(len(ratios) * 0.95) - 1)]
    return result

def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/baton")
    parser.add_argument(
        "--runs",
        type=int,
        default=80,
        help="keystrokes per probe; a larger sample stabilizes the p95",
    )
    parser.add_argument("--text", default="the quick brown fox jumps over 13 lazy dogs")
    parser.add_argument(
        "--slash",
        action="store_true",
        help="measure time from `/` to the suggestion list instead of typing letters",
    )
    parser.add_argument("--suggestion", default="switch", help="text that proves the list rendered")
    parser.add_argument("--width", type=int, default=120)
    parser.add_argument("--height", type=int, default=34)
    parser.add_argument("--ready", default="Ask anything")
    parser.add_argument("--warmup", type=float, default=3.0)
    parser.add_argument("--timeout", type=float, default=8.0)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--baseline", type=Path, help="record the result to this file")
    parser.add_argument("--check", type=Path, help="baseline file to compare against")
    parser.add_argument("--tolerance", type=float, default=0.10)
    parser.add_argument(
        "--repeat",
        type=int,
        default=3,
        help="whole-probe repetitions; the minimum percentile is compared",
    )
    parser.add_argument(
        "--slack-ms",
        type=float,
        default=0.1,
        help="absolute slack added to each limit for measurement noise",
    )
    parser.add_argument(
        "--resolution-ms",
        type=float,
        default=2.0,
        help="metrics recorded below this are reported but not enforced",
    )
    parser.add_argument("--force", action="store_true", help="compare across machines")
    args = parser.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 2

    results = []
    for _ in range(args.repeat):
        result = measure(binary, args)
        if result is None:
            return 1
        results.append(result)
    machine = results[0]["machine"]
    p50s = [r["p50_ms"] for r in results]
    p95s = [r["p95_ms"] for r in results]
    result = {
        "machine": machine,
        "samples": results[0]["samples"],
        "runs": len(results),
        "p50_ms": statistics.median(p50s),
        "p50_min_ms": min(p50s),
        "p95_ms": statistics.median(p95s),
        "p95_min_ms": min(p95s),
        "max_ms": max(r["max_ms"] for r in results),
        "exit_code": max(r["exit_code"] for r in results),
    }
    if "ratio_p50" in results[0]:
        result["control_p50_ms"] = statistics.median(
            r["control_p50_ms"] for r in results
        )
        result["control_p95_ms"] = statistics.median(
            r["control_p95_ms"] for r in results
        )
        result["ratio_p50_min"] = min(r["ratio_p50"] for r in results)
        result["ratio_p95_min"] = min(r["ratio_p95"] for r in results)
    label = "slash" if args.slash else "typing"
    print(json.dumps(result, indent=2) if args.json else
          f"{label}: {result['runs']} runs x {result['samples']} samples, "
          f"p50 min {result['p50_min_ms']:.1f} ms (median {result['p50_ms']:.1f}), "
          f"p95 min {result['p95_min_ms']:.1f} ms (median {result['p95_ms']:.1f}), max {result['max_ms']:.1f}")
    if result["exit_code"] != 0:
        return 1
    if args.baseline:
        args.baseline.write_text(json.dumps(result, indent=2) + "\n")
        print(f"wrote {args.baseline}", file=sys.stderr)
    if args.check:
        baseline = json.loads(args.check.read_text())
        if baseline["machine"] != result["machine"] and not args.force:
            print(
                f"baseline is from {baseline['machine']}, this host is {result['machine']}; pass --force",
                file=sys.stderr,
            )
            return 2
        if "p50_min_ms" not in baseline:
            print(
                f"baseline {args.check} predates repeated samples; re-record it with --baseline",
                file=sys.stderr,
            )
            return 2
        if "ratio_p50_min" in baseline:
            print(
                f"slash p50 {result['p50_min_ms']:.2f} ms on a "
                f"{result['control_p50_ms']:.2f} ms control; "
                f"p95 {result['p95_min_ms']:.2f} ms on a "
                f"{result['control_p95_ms']:.2f} ms control",
                file=sys.stderr,
            )
            failures = []
            for key, absolute in (
                ("ratio_p50_min", "p50_min_ms"),
                ("ratio_p95_min", "p95_min_ms"),
            ):
                if baseline[absolute] < args.resolution_ms:
                    print(
                        f"{key} recorded on a {baseline[absolute]:.2f} ms "
                        f"baseline below the {args.resolution_ms:.2f} ms "
                        "resolution floor; not enforced",
                        file=sys.stderr,
                    )
                    continue
                limit = baseline[key] * (1.0 + args.tolerance)
                if result[key] > limit:
                    failures.append(
                        f"{key} {result[key]:.2f} > {limit:.2f} "
                        f"({baseline[key]:.2f} baseline)"
                    )
            for failure in failures:
                print(f"regression: {failure}", file=sys.stderr)
            return 1 if failures else 0
        failures = []
        for key in ("p50_min_ms", "p95_min_ms"):
            if baseline[key] < args.resolution_ms:
                print(
                    f"{key} recorded at {baseline[key]:.2f} ms is below the "
                    f"{args.resolution_ms:.2f} ms resolution floor; not enforced",
                    file=sys.stderr,
                )
                continue
            limit = baseline[key] * (1.0 + args.tolerance) + args.slack_ms
            if result[key] > limit:
                failures.append(f"{key} {result[key]:.2f} ms > {limit:.2f} ms ({baseline[key]:.2f} baseline)")
        for failure in failures:
            print(f"regression: {failure}", file=sys.stderr)
        return 1 if failures else 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
