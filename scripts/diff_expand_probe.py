#!/usr/bin/env python3
"""Prove a long diff collapses in the real TUI and Ctrl+T expands it."""

from __future__ import annotations

import argparse
import json
import os
import pty
import subprocess
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

LINES = 60
SECOND_LINES = 40
# The viewport is bottom-sticky, so the expanded tail shows the last row of the
# final file rather than of the first.
LAST_LINE = f"probe line {SECOND_LINES:03d}"

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


def rewrite():
    targets = (
        ("BATON_PROBE_FILE", "BATON_PROBE_LINES", 60),
        ("BATON_PROBE_SECOND_FILE", "BATON_PROBE_SECOND_LINES", 40),
    )
    for variable, count_variable, default in targets:
        target = os.environ.get(variable)
        if not target:
            continue
        count = int(os.environ.get(count_variable, str(default)))
        body = "".join(f"probe line {index:03d}\\n" for index in range(1, count + 1))
        with open(target, "w", encoding="utf-8") as handle:
            handle.write(body)


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
        send({"jsonrpc": "2.0", "id": request_id, "result": {"thread": {"id": "native-diff", "path": None}}})
    elif method == "thread/resume":
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif method == "model/list":
        send({"jsonrpc": "2.0", "id": request_id, "result": {"data": [], "nextCursor": None}})
    elif method == "turn/start":
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"turn": {"id": "turn-diff", "status": "inProgress"}},
        })
        rewrite()
        send({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {
                "threadId": "native-diff",
                "turn": {
                    "id": "turn-diff",
                    "status": "completed",
                    "items": [
                        {"type": "agentMessage", "id": "msg-diff", "text": "rewrote the file"}
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


def wait_for_absence(
    fd: int, needle: str, timeout: float, width: int, height: int, transcript: bytearray
) -> str | None:
    """A frame paints progressively, so wait for the row to disappear, not to appear."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        data = read_available(fd)
        if data:
            transcript.extend(data)
        text = screen_text(transcript, width, height)
        if needle not in text:
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

    root = Path(tempfile.mkdtemp(prefix="baton-diff-expand-"))
    home = root / "home"
    workdir = root / "work"
    fake_bin = root / "bin"
    evidence = Path(args.evidence_dir) / f"diff-expand-{int(time.time())}"
    home.mkdir(parents=True, exist_ok=True)
    workdir.mkdir(parents=True, exist_ok=True)
    evidence.mkdir(parents=True, exist_ok=True)
    write_fake_codex(fake_bin)

    source = workdir / "app.txt"
    second = workdir / "lib.txt"
    source.write_text("".join(f"alpha line {index:03d}\n" for index in range(1, LINES + 1)))
    second.write_text("".join(f"beta line {index:03d}\n" for index in range(1, SECOND_LINES + 1)))
    for command in (
        ["git", "init", "-q"],
        ["git", "add", "app.txt", "lib.txt"],
        [
            "git",
            "-c",
            "user.name=probe",
            "-c",
            "user.email=probe@example.com",
            "commit",
            "-q",
            "-m",
            "seed",
        ],
    ):
        subprocess.run(command, cwd=workdir, check=True, capture_output=True)

    (home / "config.json").write_text(
        json.dumps({"defaultProvider": "codex", "defaultMode": "single"}) + "\n"
    )

    env = os.environ.copy()
    env["BATON_HOME"] = str(home)
    env["PATH"] = f"{fake_bin}:{env.get('PATH', '')}"
    env["TERM"] = "xterm-256color"
    env["NO_COLOR"] = "1"
    env["BATON_PROBE_FILE"] = str(source)
    env["BATON_PROBE_LINES"] = str(LINES)
    env["BATON_PROBE_SECOND_FILE"] = str(second)
    env["BATON_PROBE_SECOND_LINES"] = str(SECOND_LINES)

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
        pump(master, 1.5, transcript)
        os.write(master, b"rewrite app.txt\r")
        if (
            wait_for_screen(
                master, "ctrl+t expand", args.timeout, args.width, args.height, transcript
            )
            is None
        ):
            raise RuntimeError("the long diff did not collapse with a ctrl+t hint")
        collapsed = wait_for_absence(
            master, LAST_LINE, args.timeout, args.width, args.height, transcript
        )
        if collapsed is None:
            raise RuntimeError("the collapsed diff still shows the last row")
        (evidence / "collapsed-screen.txt").write_text(collapsed + "\n")
        if "Edited 2 files (+100 -100)" not in collapsed:
            raise RuntimeError("the multi-file diff header is missing")
        for name in ("app.txt", "lib.txt"):
            if name not in collapsed:
                raise RuntimeError(f"the collapsed preview dropped {name}")
        os.write(master, b"\x14")
        expanded = wait_for_screen(
            master, LAST_LINE, args.timeout, args.width, args.height, transcript
        )
        if expanded is None:
            raise RuntimeError("Ctrl+T did not reveal the last row")
        if "ctrl+t collapse" not in expanded:
            raise RuntimeError("the expanded diff has no collapse hint")
        (evidence / "expanded-screen.txt").write_text(expanded + "\n")
        os.write(master, b"\x14")
        recollapsed = wait_for_absence(
            master, LAST_LINE, args.timeout, args.width, args.height, transcript
        )
        if recollapsed is None:
            raise RuntimeError("a second Ctrl+T did not collapse the diff")
        (evidence / "recollapsed-screen.txt").write_text(recollapsed + "\n")
        if "ctrl+t expand" not in recollapsed:
            raise RuntimeError("the recollapsed diff has no expand hint")
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
        "lines": LINES,
        "evidence": str(evidence),
    }
    (evidence / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
