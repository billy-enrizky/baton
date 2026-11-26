#!/usr/bin/env python3
"""Measure the TUI's resident memory at idle, under load, and after quit.

A terminal app that leaks or grows without bound is a correctness problem for a
user who leaves it open all day. The probe drives the real binary through a
PTY, samples RSS from the OS, and fails above a recorded baseline.
"""

import argparse
import fcntl
import json
import os
import platform
import pty
import select
import signal
import struct
import sys
import tempfile
import termios
import time
from pathlib import Path

SAMPLE_INTERVAL_S = 0.05

FAKE_CODEX = '''#!/usr/bin/env python3
import json
import sys
import time

if "app-server" not in sys.argv:
    print("codex 0.0.0")
    sys.exit(0)


def send(value):
    sys.stdout.write(json.dumps(value) + "\\n")
    sys.stdout.flush()


for line in sys.stdin:
    try:
        frame = json.loads(line)
    except json.JSONDecodeError:
        continue
    method = frame.get("method")
    request_id = frame.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif method == "thread/start":
        send({"jsonrpc": "2.0", "id": request_id, "result": {"thread": {"id": "native-rss", "path": None}}})
    elif method == "thread/resume":
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif method == "model/list":
        send({"jsonrpc": "2.0", "id": request_id, "result": {"data": [], "nextCursor": None}})
    elif method == "turn/start":
        send({"jsonrpc": "2.0", "id": request_id, "result": {"turn": {"id": "turn-rss", "status": "inProgress"}}})
        for index in range(30):
            send({
                "jsonrpc": "2.0",
                "method": "item/agentMessage/delta",
                "params": {
                    "threadId": "native-rss",
                    "turnId": "turn-rss",
                    "itemId": "msg-rss",
                    "delta": "chunk %d " % index,
                },
            })
            time.sleep(0.05)
        send({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {
                "threadId": "native-rss",
                "turn": {
                    "id": "turn-rss",
                    "status": "completed",
                    "items": [
                        {"type": "agentMessage", "id": "msg-rss", "text": "stream done"}
                    ],
                },
            },
        })
    elif request_id is not None:
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
'''



def set_size(fd: int, width: int, height: int) -> None:
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))


def rss_kb(pid: int) -> int | None:
    """Resident set in KiB, from ps so the probe works the same on macOS and Linux."""
    import subprocess

    result = subprocess.run(
        ["ps", "-o", "rss=", "-p", str(pid)],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0 or not result.stdout.strip():
        return None
    try:
        return int(result.stdout.strip().split()[0])
    except ValueError:
        return None


def reap(pid: int, timeout_s: float) -> int | None:
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
    return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/baton")
    parser.add_argument("--width", type=int, default=120)
    parser.add_argument("--height", type=int, default=34)
    parser.add_argument("--ready", default="Ask anything")
    parser.add_argument("--warmup", type=float, default=3.0)
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--baseline", type=Path, help="record these numbers here")
    parser.add_argument("--check", type=Path, help="baseline to compare against")
    parser.add_argument("--tolerance", type=float, default=0.10)
    parser.add_argument("--force", action="store_true")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 2

    home = tempfile.mkdtemp(prefix="baton-rss-home-")
    workdir = tempfile.mkdtemp(prefix="baton-rss-cwd-")
    harness_dir = Path(workdir) / "bin"
    harness_dir.mkdir()
    fake_codex = harness_dir / "codex"
    fake_codex.write_text(FAKE_CODEX)
    fake_codex.chmod(0o755)
    Path(home, "config.json").write_text(
        json.dumps({"defaultProvider": "codex", "defaultMode": "single"}) + "\n"
    )
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(workdir)
        env = os.environ.copy()
        env["BATON_HOME"] = home
        env["PATH"] = f"{harness_dir}:{env.get('PATH', '')}"
        env["TERM"] = "xterm-256color"
        os.execvpe(str(binary), [str(binary)], env)

    set_size(master, args.width, args.height)
    idle_samples: list[int] = []
    load_samples: list[int] = []
    provider_samples: list[int] = []
    transcript = bytearray()

    def pump(seconds: float) -> None:
        deadline = time.perf_counter() + seconds
        while time.perf_counter() < deadline:
            ready, _, _ = select.select([master], [], [], SAMPLE_INTERVAL_S)
            if ready:
                try:
                    transcript.extend(os.read(master, 65536))
                except OSError:
                    return

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
        pump(args.warmup)

        for _ in range(20):
            value = rss_kb(pid)
            if value is not None:
                idle_samples.append(value)
            pump(0.05)

        # Exercise the render-heavy paths: the slash list, a thread switch picker,
        # and typing, without a provider turn.
        for keystrokes in (b"/", b"\x1b", b"the quick brown fox", b"\x03"):
            os.write(master, keystrokes)
            for _ in range(20):
                value = rss_kb(pid)
                if value is not None:
                    load_samples.append(value)
                pump(0.05)

        # A provider turn through a fake Codex app server. The fake streams
        # deltas for about 1.5 seconds so the parent can sample during the turn.
        os.write(master, b"memory test\r")
        for _ in range(40):
            value = rss_kb(pid)
            if value is not None:
                provider_samples.append(value)
            pump(SAMPLE_INTERVAL_S)
        if b"chunk 0" not in transcript:
            print("the provider turn did not stream", file=sys.stderr)
            return 1

        # Two control-c presses exit; the third write can hit EIO once the tty
        # closes, which is an exit, not a failure.
        for _ in range(3):
            try:
                os.write(master, b"\x03")
            except OSError:
                break
            pump(0.3)
        exit_code = reap(pid, 10)
    finally:
        try:
            os.close(master)
        except OSError:
            pass

    if not idle_samples:
        print("no RSS samples collected", file=sys.stderr)
        return 1
    result = {
        "machine": f"{platform.system()}-{platform.machine()}",
        "binary": str(args.binary),
        "idle_mb": max(idle_samples) / 1024.0,
        "load_peak_mb": max(load_samples or idle_samples) / 1024.0,
        "provider_peak_mb": max(provider_samples or idle_samples) / 1024.0,
        "exit_code": exit_code,
    }
    print(
        json.dumps(result, indent=2)
        if args.json
        else (
            f"rss: idle {result['idle_mb']:.1f} MB, load peak "
            f"{result['load_peak_mb']:.1f} MB, provider peak "
            f"{result['provider_peak_mb']:.1f} MB"
        )
    )
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
        for key in ("idle_mb", "load_peak_mb", "provider_peak_mb"):
            limit = baseline[key] * (1.0 + args.tolerance)
            if result[key] > limit:
                failures.append(f"{key} {result[key]:.1f} MB > {limit:.1f} MB ({baseline[key]:.1f} baseline)")
        for failure in failures:
            print(f"regression: {failure}", file=sys.stderr)
        return 1 if failures else 0
    return 0


if __name__ == "__main__":
    sys.exit(main())
