#!/usr/bin/env python3
"""Live TUI native rewind: two failed OpenCode turns, /rewind --native, one turn left.

Uses the configured OpenCode key as-is. The expected 401 still admits each user
message and records its native id, so the probe spends no model tokens.
Uses a throwaway git repo and BATON_HOME and never touches the real store.
"""

import argparse
import importlib.util
import json
import os
import pty
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("tui_probe", HERE / "tui_probe.py")
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


def run(binary: str, timeout: float, evidence: Path) -> dict:
    if shutil.which("opencode") is None:
        raise SystemExit("opencode is not installed; the rewind probe needs its 401 fixture")
    repo = Path(tempfile.mkdtemp(prefix="baton-rewind-repo-"))
    home = Path(tempfile.mkdtemp(prefix="baton-rewind-home-"))
    (home / "config.json").write_text(json.dumps({"defaultProvider": "opencode"}))
    (repo / "app.txt").write_text("alpha\n")
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    subprocess.run(["git", "add", "app.txt"], cwd=repo, check=True)
    subprocess.run(
        ["git", "-c", "user.email=test@test", "-c", "user.name=test", "commit", "-q", "-m", "init"],
        cwd=repo,
        check=True,
    )

    pid, master = pty.fork()
    if pid == 0:
        os.chdir(repo)
        env = os.environ.copy()
        env["BATON_HOME"] = str(home)
        env["TERM"] = "xterm-256color"
        os.execvpe(binary, [binary], env)

    probe.set_size(master, 120, 34)
    transcript = bytearray()

    def screen_text() -> str:
        screen = probe.Screen(120, 34)
        screen.feed(bytes(transcript).decode("utf-8", "replace"))
        return "\n".join(screen.lines())

    def wait_idle(timeout: float) -> str:
        deadline = time.time() + timeout
        saw_running = False
        while time.time() < deadline:
            data = probe.read_available(master)
            if data:
                transcript.extend(data)
            text = screen_text()
            if "enter steers the live turn" in text or "Thinking" in text:
                saw_running = True
            elif saw_running or "turn failed" in text or "401" in text:
                probe.pump(master, 0.6, transcript)
                return screen_text()
            time.sleep(0.2)
        raise RuntimeError("a turn never finished")

    try:
        if not probe.wait_for(master, "Ask anything", 15, transcript):
            raise RuntimeError("TUI never became ready")
        probe.pump(master, 3.0, transcript)
        os.write(master, b"/orchestrate off\r")
        probe.pump(master, 0.8, transcript)

        os.write(master, b"first\r")
        first = wait_idle(timeout)
        if "401" not in first and "failed" not in first:
            raise RuntimeError("the first OpenCode turn did not fail with the expected 401")
        os.write(master, b"second\r")
        wait_idle(timeout)

        os.write(master, b"/rewind --native\r")
        probe.pump(master, 0.8, transcript)
        picker = screen_text()
        os.write(master, b"\x1b[B")
        probe.pump(master, 0.2, transcript)
        os.write(master, b"\r")

        deadline = time.time() + 45
        done = False
        while time.time() < deadline:
            data = probe.read_available(master)
            if data:
                transcript.extend(data)
            text = screen_text()
            if "rewound to before turn 2 natively" in text:
                done = True
                break
            if "native rewind failed" in text:
                raise RuntimeError(text[-400:])
            time.sleep(0.2)
        final = screen_text()
        (evidence / "rewind-screen.txt").write_text(final + "\n")

        result = {
            "picker_showed_user_messages": "first" in picker and "second" in picker,
            "rewind_reported": done,
            "composer_prefilled": "second" in final,
        }
        os.write(master, b"\x1b")
        probe.pump(master, 0.3, transcript)
        os.write(master, b"/quit\r")
        probe.pump(master, 0.5, transcript)
        result["exit_code"] = probe.reap(pid, 5)

        conn = sqlite3.connect(home / "baton.sqlite")
        turns = list(conn.execute("SELECT turn, phase, provider_turn_id FROM turns ORDER BY turn"))
        users = list(conn.execute("SELECT turn, text FROM messages WHERE role = 'user' ORDER BY turn"))
        conn.close()
        result["turns"] = turns
        result["user_messages"] = users
        passed = (
            all(result[key] for key in ["picker_showed_user_messages", "rewind_reported", "composer_prefilled"])
            and result["exit_code"] == 0
            and len(turns) == 1
            and len(users) == 1
        )
        result["passed"] = passed
        (evidence / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
        return result
    finally:
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass
        shutil.rmtree(repo, ignore_errors=True)
        shutil.rmtree(home, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=str(Path.home() / ".cargo/bin/baton"))
    parser.add_argument("--timeout", type=float, default=90.0)
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()
    evidence = Path(args.evidence_dir) / f"rewind-probe-{int(time.time())}"
    evidence.mkdir(parents=True, exist_ok=True)
    try:
        result = run(os.path.expanduser(args.binary), args.timeout, evidence)
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(json.dumps({"passed": False, "error": str(error), "evidence": str(evidence)}, indent=2))
        return 1
    print(json.dumps({**result, "evidence": str(evidence)}, indent=2))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
