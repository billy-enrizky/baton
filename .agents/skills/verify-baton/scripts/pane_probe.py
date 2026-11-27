#!/usr/bin/env python3
"""Prove the secondary pane opens, swaps focus, and closes in the real TUI."""

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


turns = 0
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
        send({"jsonrpc": "2.0", "id": request_id, "result": {"thread": {"id": "native-pane", "path": None}}})
    elif method == "thread/resume":
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif method == "model/list":
        send({"jsonrpc": "2.0", "id": request_id, "result": {"data": [], "nextCursor": None}})
    elif method == "turn/start":
        turns += 1
        reply = "reply one" if turns == 1 else "reply two"
        turn_id = f"turn-{turns}"
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"turn": {"id": turn_id, "status": "inProgress"}},
        })
        send({
            "jsonrpc": "2.0",
            "method": "item/agentMessage/delta",
            "params": {
                "threadId": "native-pane",
                "turnId": turn_id,
                "itemId": f"msg-{turns}",
                "delta": reply,
            },
        })
        send({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {
                "threadId": "native-pane",
                "turn": {
                    "id": turn_id,
                    "status": "completed",
                    "items": [
                        {
                            "type": "agentMessage",
                            "id": f"msg-{turns}",
                            "text": reply,
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
) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        data = read_available(fd)
        if data:
            transcript.extend(data)
        if needle in screen_text(width, height, transcript):
            return True
        time.sleep(0.05)
    return False


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/baton")
    parser.add_argument("--width", type=int, default=140)
    parser.add_argument("--height", type=int, default=40)
    parser.add_argument("--timeout", type=float, default=25.0)
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 2

    root = Path(tempfile.mkdtemp(prefix="baton-pane-"))
    home = root / "home"
    workdir = root / "work"
    fake_bin = root / "bin"
    evidence = Path(args.evidence_dir) / f"pane-probe-{int(time.time())}"
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

        os.write(master, b"alpha prompt\r")
        if not wait_for_screen(
            master, "reply one", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the first fake turn did not complete")

        os.write(master, b"/new second\r")
        if not wait_for_screen(
            master, "2:second", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("/new did not add the second tab")
        os.write(master, b"beta prompt\r")
        if not wait_for_screen(
            master, "reply two", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the second fake turn did not complete")

        os.write(master, b"/pane\r")
        if not wait_for_screen(
            master, "pane · enter show", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("/pane did not open the picker")
        os.write(master, b"\r")
        if not wait_for_screen(
            master, "pane 1alpha prompt", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the pane did not show the first thread")
        if not wait_for_screen(
            master, "alpha prompt", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the pane did not render the first transcript")
        pump(master, 0.3, transcript)
        (evidence / "pane-screen.txt").write_text(
            screen_text(args.width, args.height, transcript) + "\n"
        )

        os.write(master, b"\x1bw")
        if not wait_for_screen(
            master,
            "focused pane: alpha prompt",
            args.timeout,
            args.width,
            args.height,
            transcript,
        ):
            raise RuntimeError("Alt+W did not focus the first pane")
        if not wait_for_screen(
            master, "pane 2second", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the pane did not swap to the second thread")
        if not wait_for_screen(
            master, "beta prompt", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the swapped pane did not render the second transcript")
        pump(master, 0.3, transcript)
        (evidence / "focused-screen.txt").write_text(
            screen_text(args.width, args.height, transcript) + "\n"
        )

        os.write(master, b"/pane close\r")
        if not wait_for_screen(
            master,
            "secondary pane closed",
            args.timeout,
            args.width,
            args.height,
            transcript,
        ):
            raise RuntimeError("/pane close did not close the pane")
        pump(master, 0.3, transcript)
        (evidence / "closed-screen.txt").write_text(
            screen_text(args.width, args.height, transcript) + "\n"
        )

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
