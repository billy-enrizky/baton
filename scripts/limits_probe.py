#!/usr/bin/env python3
"""Prove `/limits` reads a snapshot from an idle harness in the real TUI."""

from __future__ import annotations

import argparse
import json
import os
import pty
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / ".agents/skills/verify-baton/scripts"))

from tui_probe import (  # noqa: E402
    Screen,
    binary_identity,
    pump,
    read_available,
    reap,
    set_size,
)

FAKE_CODEX = '''#!/usr/bin/env python3
import json
import os
import sys

if "app-server" not in sys.argv:
    print("codex 0.0.0")
    sys.exit(0)


def send(value):
    sys.stdout.write(json.dumps(value) + "\\n")
    sys.stdout.flush()


reads = 0
for line in sys.stdin:
    try:
        frame = json.loads(line)
    except json.JSONDecodeError:
        continue
    method = frame.get("method")
    request_id = frame.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif method == "account/rateLimits/read":
        reads += 1
        marker = os.environ.get("BATON_PROBE_READS_FILE")
        if marker:
            with open(marker, "a", encoding="utf-8") as handle:
                handle.write(f"{reads}\\n")
        used = 12 if reads == 1 else 34
        snapshot = {
            "planType": "plus",
            "primary": {"usedPercent": used, "windowDurationMins": 300, "resetsAt": 1767000000},
            "secondary": {"usedPercent": 48, "windowDurationMins": 10080, "resetsAt": None},
            "credits": {"hasCredits": True, "unlimited": False, "balance": "12.50"},
        }
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "rateLimits": snapshot,
                "rateLimitsByLimitId": {"codex": snapshot},
                "rateLimitResetCredits": None,
            },
        })
    elif request_id is not None:
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
'''


def write_fake_codex(directory: Path) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "codex"
    path.write_text(FAKE_CODEX)
    path.chmod(0o755)
    return path


def screen_text(transcript: bytearray, width: int, height: int) -> str:
    screen = Screen(width, height)
    screen.feed(bytes(transcript).decode("utf-8", "replace"))
    return "\n".join(screen.lines())


def wait_for_screen(
    fd: int, needle: str, timeout: float, width: int, height: int, transcript: bytearray
) -> str | None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        data = read_available(fd)
        if data:
            transcript.extend(data)
        text = screen_text(transcript, width, height)
        if needle in text:
            return text
        time.sleep(0.05)
    return None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/baton")
    parser.add_argument("--width", type=int, default=120)
    parser.add_argument("--height", type=int, default=40)
    parser.add_argument("--timeout", type=float, default=25.0)
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 2

    root = Path(tempfile.mkdtemp(prefix="baton-limits-probe-"))
    home = root / "home"
    workdir = root / "work"
    fake_bin = root / "bin"
    codex_home = root / "codex"
    reads_file = root / "reads.txt"
    evidence = Path(args.evidence_dir) / f"limits-probe-{int(time.time())}"
    for directory in (home, workdir, codex_home, evidence):
        directory.mkdir(parents=True, exist_ok=True)
    write_fake_codex(fake_bin)
    (home / "config.json").write_text(
        json.dumps({"defaultProvider": "codex", "defaultMode": "single"}) + "\n"
    )

    env = os.environ.copy()
    env["BATON_HOME"] = str(home)
    env["CODEX_HOME"] = str(codex_home)
    env["PATH"] = f"{fake_bin}:{env.get('PATH', '')}"
    env["TERM"] = "xterm-256color"
    env["NO_COLOR"] = "1"
    env["BATON_PROBE_READS_FILE"] = str(reads_file)
    for key in ("CODEX_ICA_API_KEY", "CODEX_API_KEY", "OPENAI_API_KEY"):
        env.pop(key, None)

    pid, master = pty.fork()
    if pid == 0:
        os.chdir(workdir)
        os.execvpe(str(binary), [str(binary)], env)

    set_size(master, args.width, args.height)
    transcript = bytearray()
    exit_code = None
    reaped = False
    passed = False
    error = None
    try:
        if wait_for_screen(master, "Ask anything", args.timeout, args.width, args.height, transcript) is None:
            raise RuntimeError("TUI did not show the composer")
        pump(master, 1.0, transcript)

        os.write(master, b"/limits\r")
        first = wait_for_screen(master, "12% used", args.timeout, args.width, args.height, transcript)
        if first is None:
            raise RuntimeError("the first live snapshot did not render")
        pump(master, 0.3, transcript)
        first = screen_text(transcript, args.width, args.height)
        if "refreshing rate limits from codex" in first:
            raise RuntimeError("the in-flight line stayed after the snapshot arrived")
        (evidence / "first-screen.txt").write_text(first + "\n")

        os.write(master, b"\x1b")
        pump(master, 0.3, transcript)
        os.write(master, b"/limits\r")
        second = wait_for_screen(master, "34% used", args.timeout, args.width, args.height, transcript)
        if second is None:
            raise RuntimeError("the second live snapshot did not render")
        pump(master, 0.3, transcript)
        second = screen_text(transcript, args.width, args.height)
        (evidence / "second-screen.txt").write_text(second + "\n")

        reads = reads_file.read_text().splitlines() if reads_file.exists() else []
        if len(reads) != 2:
            raise RuntimeError(f"expected two on-demand reads, saw {len(reads)}")
        os.write(master, b"\x1b")
        pump(master, 0.3, transcript)
        os.write(master, b"/quit\r")
        pump(master, 0.5, transcript)
        exit_code = reap(pid, 5)
        reaped = True
        passed = exit_code == 0
    except Exception as caught:  # noqa: BLE001
        error = str(caught)
    finally:
        if not reaped:
            try:
                os.write(master, b"\x1b/quit\r")
            except OSError:
                pass
            try:
                exit_code = reap(pid, 5)
            except (ChildProcessError, OSError):
                pass
        try:
            os.close(master)
        except OSError:
            pass
        (evidence / "transcript.raw").write_bytes(bytes(transcript))

    summary = {
        "passed": passed and error is None,
        "binary": binary_identity(binary),
        "error": error,
        "exit_code": exit_code,
        "reads": len(reads_file.read_text().splitlines()) if reads_file.exists() else 0,
        "evidence": str(evidence),
    }
    (evidence / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
