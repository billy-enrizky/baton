#!/usr/bin/env python3
"""Prove install and update for a project skill in the real TUI."""

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
    parser.add_argument("--height", type=int, default=34)
    parser.add_argument("--timeout", type=float, default=20.0)
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 2

    root = Path(tempfile.mkdtemp(prefix="baton-skills-probe-"))
    home = root / "home"
    workdir = root / "work"
    claude_config = root / "claude-config"
    codex_home = root / "codex-home"
    evidence = Path(args.evidence_dir) / f"skills-probe-{int(time.time())}"
    for directory in (home, workdir, claude_config, codex_home, evidence):
        directory.mkdir(parents=True, exist_ok=True)
    source = claude_config / "skills" / "demo"
    source.mkdir(parents=True)
    (source / "SKILL.md").write_text(
        "---\nname: demo\ndescription: Demo checks\n---\nversion one\n"
    )
    (home / "config.json").write_text(
        json.dumps({"defaultProvider": "claude", "defaultMode": "single"}) + "\n"
    )

    env = os.environ.copy()
    env.update(
        {
            "BATON_HOME": str(home),
            "CLAUDE_CONFIG_DIR": str(claude_config),
            "CODEX_HOME": str(codex_home),
            "TERM": "xterm-256color",
            "NO_COLOR": "1",
        }
    )
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
        if wait_for_screen(
            master, "Ask anything", args.timeout, args.width, args.height, transcript
        ) is None:
            raise RuntimeError("TUI did not show the composer")
        pump(master, 1.0, transcript)

        os.write(master, b"/skills\r")
        picker = wait_for_screen(
            master,
            "demo (claude · global)",
            args.timeout,
            args.width,
            args.height,
            transcript,
        )
        if picker is None:
            raise RuntimeError("global skill row did not render")
        for marker in ("i install", "u update"):
            if marker not in picker:
                raise RuntimeError(f"picker is missing {marker}")
        (evidence / "picker-screen.txt").write_text(picker + "\n")

        os.write(master, b"i")
        installed = wait_for_screen(
            master,
            "installed $demo into .claude/skills/demo",
            args.timeout,
            args.width,
            args.height,
            transcript,
        )
        if installed is None:
            raise RuntimeError("install notice did not render")
        project = workdir / ".claude" / "skills" / "demo" / "SKILL.md"
        if not project.is_file():
            raise RuntimeError("project skill was not installed")
        (evidence / "install-screen.txt").write_text(installed + "\n")

        (source / "SKILL.md").write_text(
            "---\nname: demo\ndescription: Updated checks\n---\nversion two\n"
        )
        os.write(master, b"/skills\r")
        project_picker = wait_for_screen(
            master,
            "demo (claude · project)",
            args.timeout,
            args.width,
            args.height,
            transcript,
        )
        if project_picker is None:
            raise RuntimeError("project skill row did not render")
        os.write(master, b"u")
        updated = wait_for_screen(
            master,
            "updated $demo in .claude/skills/demo",
            args.timeout,
            args.width,
            args.height,
            transcript,
        )
        if updated is None:
            raise RuntimeError("update notice did not render")
        if "version two" not in project.read_text():
            raise RuntimeError("project skill content was not updated")
        (evidence / "update-screen.txt").write_text(updated + "\n")

        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)
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
        "evidence": str(evidence),
    }
    (evidence / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
