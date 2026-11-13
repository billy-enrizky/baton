#!/usr/bin/env python3
"""Live greeting regression: `hi` in default orchestrate mode answers directly.

Guards the reported `goal · hi` / `run failed · planner returned no tasks`
regression. Uses a throwaway git repo and BATON_HOME; spends one small model
turn on the configured Codex default.
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
    if shutil.which("codex") is None:
        raise SystemExit("codex is not installed; the greeting probe needs one Codex turn")
    repo = Path(tempfile.mkdtemp(prefix="baton-greeting-repo-"))
    home = Path(tempfile.mkdtemp(prefix="baton-greeting-home-"))
    (home / "config.json").write_text(json.dumps({"defaultProvider": "codex"}))
    (repo / "README.md").write_text("greeting probe\n")
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    subprocess.run(["git", "add", "README.md"], cwd=repo, check=True)
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
    try:
        if not probe.wait_for(master, "Ask anything", 15, transcript):
            raise RuntimeError("TUI never became ready")
        probe.pump(master, 3.0, transcript)
        os.write(master, b"hi\r")

        deadline = time.time() + timeout
        saw_running = False
        done = False
        while time.time() < deadline:
            data = probe.read_available(master)
            if data:
                transcript.extend(data)
            screen = probe.Screen(120, 34)
            screen.feed(bytes(transcript).decode("utf-8", "replace"))
            text = "\n".join(screen.lines())
            if "enter steers the live turn" in text or "Thinking" in text:
                saw_running = True
            elif saw_running:
                done = True
                break
            time.sleep(0.2)
        if not done:
            raise RuntimeError("the greeting turn never finished")
        probe.pump(master, 1.0, transcript)
        screen = probe.Screen(120, 34)
        screen.feed(bytes(transcript).decode("utf-8", "replace"))
        final = "\n".join(screen.lines())
        (evidence / "greeting-screen.txt").write_text(final + "\n")
        raw = bytes(transcript).decode("utf-8", "replace")
        result = {
            "no_planner_failure": "planner returned no tasks" not in raw and "run failed" not in raw,
            "no_orchestration_line": "goal ·" not in final,
            "reply_present": "codex ·" in final,
        }
        os.write(master, b"/quit\r")
        probe.pump(master, 0.5, transcript)
        result["exit_code"] = probe.reap(pid, 5)

        conn = sqlite3.connect(home / "baton.sqlite")
        turns = list(conn.execute("SELECT turn, phase FROM turns ORDER BY turn"))
        runs = list(conn.execute("SELECT id FROM orchestration_runs"))
        conn.close()
        result["turns"] = turns
        result["orchestration_runs"] = runs
        passed = (
            result["no_planner_failure"]
            and result["no_orchestration_line"]
            and result["exit_code"] == 0
            and len(turns) == 1
            and turns[0][1] == "completed"
            and len(runs) == 0
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
    evidence = Path(args.evidence_dir) / f"greeting-probe-{int(time.time())}"
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
