#!/usr/bin/env python3
"""Prove Ctrl+K opens the palette and runs an action in the real TUI."""

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
from tui_probe import Screen, binary_identity, pump, reap, set_size, wait_for


def screen_text(transcript: bytearray, width: int, height: int) -> str:
    screen = Screen(width, height)
    screen.feed(bytes(transcript).decode("utf-8", "replace"))
    return "\n".join(screen.lines())


def wait_for_screen(
    master: int,
    needle: str,
    timeout: float,
    transcript: bytearray,
    width: int,
    height: int,
) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        pump(master, 0.1, transcript)
        if needle in screen_text(transcript, width, height):
            return True
    return False


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/baton")
    parser.add_argument("--width", type=int, default=120)
    parser.add_argument("--height", type=int, default=34)
    parser.add_argument("--timeout", type=float, default=12.0)
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 2

    home = tempfile.mkdtemp(prefix="baton-palette-home-")
    workdir = tempfile.mkdtemp(prefix="baton-palette-cwd-")
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=workdir, check=True)
    Path(workdir, "README.md").write_text("# palette probe\n")
    subprocess.run(["git", "config", "user.email", "probe@example.com"], cwd=workdir, check=True)
    subprocess.run(["git", "config", "user.name", "probe"], cwd=workdir, check=True)
    subprocess.run(["git", "config", "commit.gpgsign", "false"], cwd=workdir, check=True)
    subprocess.run(["git", "add", "."], cwd=workdir, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "init"], cwd=workdir, check=True)
    Path(home, "config.json").write_text(json.dumps({"defaultProvider": "claude"}) + "\n")

    evidence = Path(args.evidence_dir) / f"palette-probe-{int(time.time())}"
    evidence.mkdir(parents=True, exist_ok=True)
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(workdir)
        env = os.environ.copy()
        env["BATON_HOME"] = home
        env["TERM"] = "xterm-256color"
        env["BATON_CLIPBOARD_COMMAND"] = "cat"
        env["VISUAL"] = "/bin/true"
        env["EDITOR"] = "/bin/true"
        os.execvpe(str(binary), [str(binary)], env)

    transcript = bytearray()
    missing: list[str] = []
    exit_code = None
    try:
        set_size(master, args.width, args.height)
        if not wait_for(master, "Ask anything", args.timeout, transcript):
            raise RuntimeError("TUI did not show its composer")
        pump(master, 2.0, transcript)

        os.write(master, b"\x0b")
        if not wait_for_screen(
            master, "palette ·", args.timeout, transcript, args.width, args.height
        ):
            missing.append("palette header")
        opened = screen_text(transcript, args.width, args.height)
        for value in ["New thread", "Sign in"]:
            if value not in opened:
                missing.append(value)
        (evidence / "palette-screen.txt").write_text(opened + "\n")

        os.write(master, b"worker questions")
        pump(master, 0.4, transcript)
        filtered = screen_text(transcript, args.width, args.height)
        if "Worker questions" not in filtered:
            missing.append("filtered action")
        (evidence / "palette-filtered-screen.txt").write_text(filtered + "\n")

        os.write(master, b"\r")
        if not wait_for_screen(
            master,
            "no open worker questions",
            args.timeout,
            transcript,
            args.width,
            args.height,
        ):
            missing.append("palette action result")
        (evidence / "palette-result-screen.txt").write_text(
            screen_text(transcript, args.width, args.height) + "\n"
        )

        os.write(master, b"/quit\r")
        pump(master, 0.6, transcript)
        exit_code = reap(pid, 5)
        pid = 0
    finally:
        if pid:
            try:
                os.kill(pid, 9)
            except ProcessLookupError:
                pass
        (evidence / "transcript.raw").write_bytes(transcript)
        summary = {
            "passed": not missing and exit_code == 0,
            "binary": binary_identity(binary),
            "missing": missing,
            "exit_code": exit_code,
            "evidence": str(evidence),
        }
        (evidence / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps(summary, indent=2))
        return 0 if summary["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
