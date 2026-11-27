#!/usr/bin/env python3
"""Prove thread tabs open, render, and switch in the real TUI."""

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


def screen_text(width: int, height: int, transcript: bytearray) -> str:
    screen = Screen(width, height)
    screen.feed(bytes(transcript).decode("utf-8", "replace"))
    return "\n".join(screen.lines())


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

    root = Path(tempfile.mkdtemp(prefix="baton-tabs-"))
    home = root / "home"
    workdir = root / "work"
    evidence = Path(args.evidence_dir) / f"tabs-probe-{int(time.time())}"
    home.mkdir(parents=True, exist_ok=True)
    workdir.mkdir(parents=True, exist_ok=True)
    evidence.mkdir(parents=True, exist_ok=True)
    (home / "config.json").write_text(
        json.dumps({"defaultProvider": "codex", "defaultMode": "single"}) + "\n"
    )

    env = os.environ.copy()
    env["BATON_HOME"] = str(home)
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
        text = screen_text(args.width, args.height, transcript)
        if "1:New thread" in text:
            raise RuntimeError("the tab strip appeared with a single tab")

        os.write(master, b"/new second\r")
        if not wait_for_screen(
            master, "2:second", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("/new did not add the second tab")
        if "1:New thread" not in screen_text(args.width, args.height, transcript):
            raise RuntimeError("the first tab disappeared after /new")
        pump(master, 0.3, transcript)
        (evidence / "two-tabs-screen.txt").write_text(
            screen_text(args.width, args.height, transcript) + "\n"
        )

        # Alt+1 is ESC followed by the digit in a legacy terminal stream.
        os.write(master, b"\x1b1")
        if not wait_for_screen(
            master, "opened New thread", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("Alt+1 did not switch to the first tab")

        os.write(master, b"\x1b2")
        if not wait_for_screen(
            master, "opened second", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("Alt+2 did not restore the unsaved draft tab")
        pump(master, 0.3, transcript)
        (evidence / "switched-screen.txt").write_text(
            screen_text(args.width, args.height, transcript) + "\n"
        )

        os.write(master, b"\x1b9")
        if not wait_for_screen(
            master, "no tab 9", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("Alt+9 without a tab did not report an empty slot")
        pump(master, 0.3, transcript)
        (evidence / "missing-tab-screen.txt").write_text(
            screen_text(args.width, args.height, transcript) + "\n"
        )

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
