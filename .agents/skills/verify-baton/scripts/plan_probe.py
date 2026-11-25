#!/usr/bin/env python3
"""Drive a Codex plan through the TUI and prove /plan renders it."""

from __future__ import annotations

import argparse
import json
import os
import pty
import shutil
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

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
import sys

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
        send({"jsonrpc": "2.0", "id": request_id, "result": {"thread": {"id": "native-plan", "path": None}}})
    elif method == "thread/resume":
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif method == "model/list":
        send({"jsonrpc": "2.0", "id": request_id, "result": {"data": [], "nextCursor": None}})
    elif method == "turn/start":
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"turn": {"id": "turn-plan", "status": "inProgress"}},
        })
        send({
            "jsonrpc": "2.0",
            "method": "turn/plan/updated",
            "params": {
                "threadId": "native-plan",
                "turnId": "turn-plan",
                "explanation": "Two checks",
                "plan": [
                    {"step": "Inspect the parser", "status": "completed"},
                    {"step": "Add the regression", "status": "inProgress"},
                ],
            },
        })
        send({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {
                "threadId": "native-plan",
                "turn": {
                    "id": "turn-plan",
                    "status": "completed",
                    "items": [
                        {"type": "agentMessage", "id": "msg-plan", "text": "plan ready"}
                    ],
                },
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


def wait_for_screen(
    fd: int, needle: str, timeout: float, width: int, height: int, transcript: bytearray
) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        data = read_available(fd)
        if data:
            transcript.extend(data)
        screen = Screen(width, height)
        screen.feed(bytes(transcript).decode("utf-8", "replace"))
        if needle in "\n".join(screen.lines()):
            return True
        time.sleep(0.05)
    return False


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

    root = Path(tempfile.mkdtemp(prefix="baton-plan-"))
    home = root / "home"
    workdir = root / "work"
    fake_bin = root / "bin"
    evidence = Path(args.evidence_dir) / f"plan-probe-{int(time.time())}"
    home.mkdir(parents=True, exist_ok=True)
    workdir.mkdir(parents=True, exist_ok=True)
    evidence.mkdir(parents=True, exist_ok=True)
    write_fake_codex(fake_bin)
    (home / "config.json").write_text(
        json.dumps({"defaultProvider": "codex", "defaultMode": "single"}) + "\n"
    )

    env = os.environ.copy()
    env["BATON_HOME"] = str(home)
    env["PATH"] = f"{fake_bin}:{env.get('PATH', '')}"
    env["TERM"] = "xterm-256color"

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
        if not wait_for_screen(
            master, "Ask anything", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("TUI did not show the composer")
        pump(master, 1.5, transcript)
        os.write(master, b"make a plan\r")
        if not wait_for_screen(
            master, "plan ready", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the fake Codex turn did not complete")
        os.write(master, b"/plan\r")
        if not wait_for_screen(
            master, "plan · 1/2 done", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("/plan did not open the overlay")
        for value in ["Two checks", "✓ Inspect the parser", "▸ Add the regression"]:
            if not wait_for_screen(
                master, value, 8.0, args.width, args.height, transcript
            ):
                raise RuntimeError(f"the plan overlay omitted {value!r}")
        pump(master, 0.3, transcript)
        screen = Screen(args.width, args.height)
        screen.feed(bytes(transcript).decode("utf-8", "replace"))
        text = "\n".join(screen.lines())
        (evidence / "plan-screen.txt").write_text(text + "\n")
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
        (evidence / "transcript.raw").write_bytes(transcript)
        shutil.rmtree(root, ignore_errors=True)

    summary = {
        "passed": passed,
        "binary": binary_identity(binary),
        "error": error,
        "exit_code": exit_code,
        "evidence": str(evidence),
    }
    (evidence / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
