#!/usr/bin/env python3
"""Drive ACP markdown and file plans through the real TUI."""

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


FAKE_AGY = '''#!/usr/bin/env python3
import json
import sys

if "acp" not in sys.argv:
    print("agy 1.0.0")
    sys.exit(0)


def send(value):
    sys.stdout.write(json.dumps(value) + "\\n")
    sys.stdout.flush()


def update(value):
    send({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {"sessionId": "acp-plan-1", "update": value},
    })


turn = 0
for line in sys.stdin:
    try:
        frame = json.loads(line)
    except json.JSONDecodeError:
        continue
    method = frame.get("method")
    request_id = frame.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": request_id, "result": {"protocolVersion": 2, "authMethods": []}})
    elif method == "session/new":
        send({"jsonrpc": "2.0", "id": request_id, "result": {"sessionId": "acp-plan-1"}})
    elif method in {
        "session/resume",
        "session/load",
        "session/set_model",
        "session/set_mode",
        "session/set_config_option",
    }:
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif method == "session/prompt":
        turn += 1
        if turn == 1:
            update({
                "sessionUpdate": "plan_update",
                "plan": {
                    "type": "markdown",
                    "planId": "plan-markdown",
                    "content": "# Ship it\\n\\n- run checks",
                },
            })
            update({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "markdown plan ready"},
            })
        else:
            update({
                "sessionUpdate": "plan_update",
                "plan": {
                    "type": "file",
                    "planId": "plan-file",
                    "uri": "file:///workspace/PLAN.md",
                },
            })
            update({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "file plan ready"},
            })
        update({"sessionUpdate": "state_update", "state": "idle", "stopReason": "end_turn"})
        send({"jsonrpc": "2.0", "id": request_id, "result": {"stopReason": "end_turn"}})
    elif request_id is not None:
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
'''


def write_fake_agy(directory: Path) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "agy"
    path.write_text(FAKE_AGY)
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

    root = Path(tempfile.mkdtemp(prefix="baton-acp-plan-"))
    home = root / "home"
    workdir = root / "work"
    fake_bin = root / "bin"
    evidence = Path(args.evidence_dir) / f"acp-plan-probe-{int(time.time())}"
    home.mkdir(parents=True, exist_ok=True)
    workdir.mkdir(parents=True, exist_ok=True)
    evidence.mkdir(parents=True, exist_ok=True)
    write_fake_agy(fake_bin)
    (home / "config.json").write_text(
        json.dumps(
            {
                "defaultProvider": "antigravity",
                "defaultMode": "single",
            }
        )
        + "\n"
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
    passed = False
    error = None
    try:
        if not wait_for_screen(
            master, "Ask anything", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("TUI did not show the composer")
        pump(master, 1.0, transcript)

        os.write(master, b"markdown plan\r")
        if not wait_for_screen(
            master, "markdown plan ready", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the markdown ACP turn did not complete")
        pump(master, 0.5, transcript)
        os.write(master, b"/plan\r")
        if not wait_for_screen(
            master, "plan · markdown", 8.0, args.width, args.height, transcript
        ):
            raise RuntimeError("the markdown plan did not open in /plan")
        pump(master, 0.3, transcript)
        markdown_screen = Screen(args.width, args.height)
        markdown_screen.feed(bytes(transcript).decode("utf-8", "replace"))
        markdown_text = "\n".join(markdown_screen.lines())
        (evidence / "screen-markdown.txt").write_text(markdown_text + "\n")
        for marker in ["plan · markdown", "Ship it", "• run checks"]:
            if marker not in markdown_text:
                raise RuntimeError(f"markdown screen is missing {marker!r}")
        os.write(master, b"\x1b")
        pump(master, 0.3, transcript)

        os.write(master, b"file plan\r")
        if not wait_for_screen(
            master, "file plan ready", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the file ACP turn did not complete")
        pump(master, 0.5, transcript)
        os.write(master, b"/plan\r")
        if not wait_for_screen(
            master, "plan · file", 8.0, args.width, args.height, transcript
        ):
            raise RuntimeError("the file plan did not open in /plan")
        pump(master, 0.3, transcript)
        file_screen = Screen(args.width, args.height)
        file_screen.feed(bytes(transcript).decode("utf-8", "replace"))
        file_text = "\n".join(file_screen.lines())
        (evidence / "screen-file.txt").write_text(file_text + "\n")
        for marker in ["plan · file", "file plan: file:///workspace/PLAN.md"]:
            if marker not in file_text:
                raise RuntimeError(f"file screen is missing {marker!r}")
        passed = True
    except Exception as caught:  # noqa: BLE001
        error = str(caught)
    finally:
        try:
            if passed:
                os.write(master, b"\x1b")
                pump(master, 0.3, transcript)
                os.write(master, b"/quit\r")
                pump(master, 0.5, transcript)
            else:
                os.write(master, b"\x03\x03")
                pump(master, 0.5, transcript)
        except OSError:
            pass
        exit_code = reap(pid, 5)
        (evidence / "transcript.raw").write_bytes(bytes(transcript))
        summary = {
            "passed": passed and exit_code == 0 and error is None,
            "binary": binary_identity(binary),
            "error": error,
            "exit_code": exit_code,
            "evidence": str(evidence),
        }
        (evidence / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps(summary, indent=2))
        shutil.rmtree(root, ignore_errors=True)
    return 0 if passed and exit_code == 0 and error is None else 1


if __name__ == "__main__":
    sys.exit(main())
