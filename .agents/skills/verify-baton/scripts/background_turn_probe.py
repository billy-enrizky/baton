#!/usr/bin/env python3
"""Prove a tab switch leaves a live turn running in the background."""

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
        send({"jsonrpc": "2.0", "id": request_id, "result": {"thread": {"id": "native-bg", "path": None}}})
    elif method == "thread/resume":
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif method == "model/list":
        send({"jsonrpc": "2.0", "id": request_id, "result": {"data": [], "nextCursor": None}})
    elif method == "turn/start":
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"turn": {"id": "turn-bg", "status": "inProgress"}},
        })
        send({
            "jsonrpc": "2.0",
            "method": "item/agentMessage/delta",
            "params": {
                "threadId": "native-bg",
                "turnId": "turn-bg",
                "itemId": "msg-bg",
                "delta": "alpha ",
            },
        })
        time.sleep(3.5)
        send({
            "jsonrpc": "2.0",
            "method": "item/agentMessage/delta",
            "params": {
                "threadId": "native-bg",
                "turnId": "turn-bg",
                "itemId": "msg-bg",
                "delta": "omega",
            },
        })
        send({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {
                "threadId": "native-bg",
                "turn": {
                    "id": "turn-bg",
                    "status": "completed",
                    "items": [
                        {
                            "type": "agentMessage",
                            "id": "msg-bg",
                            "text": "alpha omega",
                        }
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


def screen_text(width: int, height: int, transcript: bytearray) -> str:
    screen = Screen(width, height)
    screen.feed(bytes(transcript).decode("utf-8", "replace"))
    return "\n".join(screen.lines())


def wait_for_screen(
    fd: int,
    needle: str,
    timeout: float,
    width: int,
    height: int,
    transcript: bytearray,
    present: bool = True,
) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        data = read_available(fd)
        if data:
            transcript.extend(data)
        text = screen_text(width, height, transcript)
        if (needle in text) == present:
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

    root = Path(tempfile.mkdtemp(prefix="baton-background-turn-"))
    home = root / "home"
    workdir = root / "work"
    fake_bin = root / "bin"
    evidence = Path(args.evidence_dir) / f"background-turn-probe-{int(time.time())}"
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
    env["NO_COLOR"] = "1"

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

        os.write(master, b"stream please\r")
        if not wait_for_screen(
            master, "alpha", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the fake turn never streamed its first delta")

        os.write(master, b"/new second\r")
        if not wait_for_screen(
            master, "2:second", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("/new did not add the second tab")
        if not wait_for_screen(
            master, "1:New thread *", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the running tab did not keep its marker in the background")

        os.write(master, b"draft text")
        if not wait_for_screen(
            master, "draft text", 5.0, args.width, args.height, transcript
        ):
            raise RuntimeError("the second tab composer did not accept input while the turn ran")
        pump(master, 0.3, transcript)
        (evidence / "background-screen.txt").write_text(
            screen_text(args.width, args.height, transcript) + "\n"
        )

        os.write(master, b"\x1b1")
        if not wait_for_screen(
            master, "alpha", 5.0, args.width, args.height, transcript
        ):
            raise RuntimeError("Alt+1 did not restore the live streaming buffer")

        os.write(master, b"\x1b2")
        if not wait_for_screen(
            master, "2:second", 5.0, args.width, args.height, transcript
        ):
            raise RuntimeError("Alt+2 did not return to the second tab")
        # Draw the running marker at least once before waiting for it to clear.
        pump(master, 0.3, transcript)
        if not wait_for_screen(
            master,
            "1:New thread *",
            5.0,
            args.width,
            args.height,
            transcript,
            present=False,
        ):
            raise RuntimeError("the background turn marker never cleared after completion")

        os.write(master, b"\x1b1")
        if not wait_for_screen(
            master, "alpha omega", 8.0, args.width, args.height, transcript
        ):
            raise RuntimeError("the completed background reply did not reach the first tab")
        pump(master, 0.3, transcript)
        (evidence / "finished-screen.txt").write_text(
            screen_text(args.width, args.height, transcript) + "\n"
        )

        os.write(master, b"\x1b2")
        if not wait_for_screen(
            master, "draft text", 5.0, args.width, args.height, transcript
        ):
            raise RuntimeError("the second tab lost its composer draft")

        # The second tab still holds its draft, so quit with the idle double Ctrl+C.
        os.write(master, b"\x03")
        pump(master, 0.2, transcript)
        os.write(master, b"\x03")
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
        palette_colors = (
            b"\x1b[38;2;",
            b"\x1b[38;5;",
            b"\x1b[48;2;",
            b"\x1b[48;5;",
        )
        if any(sequence in transcript for sequence in palette_colors):
            error = "NO_COLOR still produced palette colors"
            passed = False
        elif b"\x1b[7m" not in transcript:
            error = "NO_COLOR lost the reverse-video caret and selection"
            passed = False
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
