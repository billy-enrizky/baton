#!/usr/bin/env python3
"""Measure keystroke-to-quiet in the real TUI through a PTY.

The TUI repaints on every frame, so "rendered" means the output stream went
quiet after a keystroke. That is a proxy for keystroke-to-render that needs no
instrumentation inside the app, and it stays honest: if a repaint is slow to
start, the quiet window starts later.
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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/baton")
    parser.add_argument("--runs", type=int, default=40)
    parser.add_argument("--text", default="the quick brown fox jumps over 13 lazy dogs")
    parser.add_argument("--width", type=int, default=120)
    parser.add_argument("--height", type=int, default=34)
    parser.add_argument("--ready", default="Ask anything")
    parser.add_argument("--warmup", type=float, default=3.0)
    parser.add_argument("--timeout", type=float, default=8.0)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--baseline", type=Path, help="record the result to this file")
    parser.add_argument("--check", type=Path, help="baseline file to compare against")
    parser.add_argument("--tolerance", type=float, default=0.10)
    parser.add_argument("--force", action="store_true", help="compare across machines")
    args = parser.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 2

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
            return 1
        drain(master, QUIET_MS, args.timeout)
        time.sleep(args.warmup)

        for index, char in enumerate(args.text):
            if index >= args.runs:
                break
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
        return 1
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
    print(json.dumps(result, indent=2) if args.json else
          f"typing: {result['samples']} keys, p50 {result['p50_ms']:.1f} ms, p95 {result['p95_ms']:.1f} ms, max {result['max_ms']:.1f} ms")
    if exit_code != 0:
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
        failures = []
        for key in ("p50_ms", "p95_ms"):
            limit = baseline[key] * (1.0 + args.tolerance)
            if result[key] > limit:
                failures.append(f"{key} {result[key]:.2f} ms > {limit:.2f} ms ({baseline[key]:.2f} baseline)")
        for failure in failures:
            print(f"regression: {failure}", file=sys.stderr)
        return 1 if failures else 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
