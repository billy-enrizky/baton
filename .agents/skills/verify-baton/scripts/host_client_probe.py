#!/usr/bin/env python3
"""Prove a live host refuses CLI clients and shows the owner notice in the TUI."""

from __future__ import annotations

import argparse
import json
import os
import pty
import select
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from tui_probe import Screen, binary_identity, pump, reap, set_size


def screen_text(transcript: bytearray, width: int, height: int) -> str:
    screen = Screen(width, height)
    screen.feed(bytes(transcript).decode("utf-8", "replace"))
    return "\n".join(screen.lines())


def wait_for_text(
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


def wait_for_host(process: subprocess.Popen[str], timeout: float) -> str:
    deadline = time.time() + timeout
    assert process.stderr is not None
    while time.time() < deadline:
        ready, _, _ = select.select([process.stderr], [], [], 0.2)
        if ready:
            line = process.stderr.readline()
            if not line:
                break
            if "host ready" in line:
                return line.strip()
        if process.poll() is not None:
            break
    raise RuntimeError("host did not become ready")


def thread_count(home: str) -> int:
    path = Path(home) / "baton.sqlite"
    connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    try:
        return int(connection.execute("SELECT count(*) FROM threads").fetchone()[0])
    finally:
        connection.close()


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

    home = tempfile.mkdtemp(prefix="baton-host-client-home-")
    cwd = tempfile.mkdtemp(prefix="baton-host-client-cwd-")
    evidence = Path(args.evidence_dir) / f"host-client-probe-{int(time.time())}"
    evidence.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["BATON_HOME"] = home
    env["TERM"] = "xterm-256color"
    env["NO_COLOR"] = "1"
    for name in (
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "VISUAL",
        "EDITOR",
        "BATON_EDITOR_CAPTURE",
        "BATON_EDITOR_FAIL",
    ):
        env.pop(name, None)

    host: subprocess.Popen[str] | None = None
    master: int | None = None
    tui_pid: int | None = None
    failures: list[str] = []
    checks: dict[str, object] = {}
    try:
        host = subprocess.Popen(
            [str(binary), "serve", "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            env=env,
            text=True,
        )
        ready = wait_for_host(host, args.timeout)
        checks["ready"] = ready
        owner_pid = host.pid

        for command, command_args in (
            ("threads", []),
            ("mcp", []),
            ("worktrees", ["--clean"]),
        ):
            output = subprocess.run(
                [str(binary), command, *command_args],
                env=env,
                stdin=subprocess.DEVNULL,
                capture_output=True,
                text=True,
                timeout=args.timeout,
            )
            expected = (
                f"baton: pid {owner_pid} started "
                if output.returncode == 4 and output.stderr.startswith(f"baton: pid {owner_pid} started ")
                else ""
            )
            passed = (
                output.returncode == 4
                and bool(expected)
                and "owns this store" in output.stderr
                and f"baton host takeover --pid {owner_pid}" in output.stderr
            )
            label = " ".join([command, *command_args])
            checks[label] = {
                "exit": output.returncode,
                "stderr": output.stderr.strip(),
                "passed": passed,
            }
            if not passed:
                failures.append(f"{label} did not refuse with exit 4 and the owner message")

        before = thread_count(home)
        tui_pid, master = pty.fork()
        if tui_pid == 0:
            os.chdir(cwd)
            os.execvpe(str(binary), [str(binary)], env)
        set_size(master, args.width, args.height)
        transcript = bytearray()
        owner_visible = wait_for_text(
            master,
            "owns this store",
            args.timeout,
            transcript,
            args.width,
            args.height,
        )
        first_screen = screen_text(transcript, args.width, args.height)
        (evidence / "tui-owner.txt").write_text(first_screen + "\n")
        checks["tui_owner_notice"] = owner_visible
        if not owner_visible:
            failures.append("the TUI did not show the owner notice")

        os.write(master, b"hello\r")
        pump(master, 1.0, transcript)
        screen = screen_text(transcript, args.width, args.height)
        after = thread_count(home)
        send_blocked = (
            "owns this store" in screen
            and f"baton host takeover --pid {owner_pid}" in screen
            and after == before
        )
        (evidence / "tui-after-send.txt").write_text(screen + "\n")
        checks["tui_send_blocked"] = send_blocked
        checks["threads_before"] = before
        checks["threads_after"] = after
        if not send_blocked:
            failures.append("a typed prompt changed the store or replaced the owner notice")

        os.write(master, b"/quit\r")
        pump(master, 0.5, transcript)
        tui_exit = reap(tui_pid, args.timeout)
        checks["tui_exit"] = tui_exit
        if tui_exit != 0:
            failures.append(f"the TUI exited {tui_exit} after a locked /quit")
        tui_pid = None

        assert host.stdin is not None
        host.stdin.close()
        host_exit = host.wait(timeout=args.timeout)
        checks["host_exit"] = host_exit
        if host_exit != 0:
            failures.append(f"the host exited {host_exit} after stdin EOF")
        host = None

        result = {
            "passed": not failures,
            "binary": binary_identity(binary),
            "checks": checks,
            "failures": failures,
            "evidence": str(evidence),
        }
        (evidence / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2))
        return 0 if not failures else 1
    finally:
        if tui_pid is not None:
            try:
                os.kill(tui_pid, 9)
            except OSError:
                pass
            reap(tui_pid, 2.0)
        if master is not None:
            try:
                os.close(master)
            except OSError:
                pass
        if host is not None:
            host.kill()
            host.wait(timeout=5)


if __name__ == "__main__":
    raise SystemExit(main())
