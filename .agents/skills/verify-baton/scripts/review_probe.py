#!/usr/bin/env python3
"""Live review journey: one Codex turn, /review, save a line note, verify it is unsent.

Uses a throwaway git repo and BATON_HOME. Never touches the real store.
Prints one JSON summary and exits 0 only when every step passed.
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
        raise SystemExit("codex is not installed; the review probe needs one live Codex turn")
    repo = Path(tempfile.mkdtemp(prefix="baton-review-repo-"))
    home = Path(tempfile.mkdtemp(prefix="baton-review-home-"))
    (home / "config.json").write_text(json.dumps({"defaultProvider": "codex"}))
    (repo / "app.txt").write_text("alpha\nbeta\ngamma\n")
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
    try:
        if not probe.wait_for(master, "Ask anything", 15, transcript):
            raise RuntimeError("TUI never became ready")
        probe.pump(master, 3.0, transcript)
        os.write(master, b"/orchestrate off\r")
        probe.pump(master, 1.0, transcript)
        prompt = (
            "Use your file editing tool to rewrite app.txt so its second line is exactly "
            "'beta changed'. Keep the first line 'alpha' and the third line 'gamma'. "
            "Reply with exactly REVIEW_OK when done."
        )
        os.write(master, prompt.encode() + b"\r")

        deadline = time.time() + timeout
        edited = False
        while time.time() < deadline:
            data = probe.read_available(master)
            if data:
                transcript.extend(data)
            if "beta changed" in (repo / "app.txt").read_text():
                edited = True
                break
            time.sleep(0.1)
        if not edited:
            raise RuntimeError("the worker never edited app.txt")

        deadline = time.time() + timeout
        idle = False
        while time.time() < deadline:
            data = probe.read_available(master)
            if data:
                transcript.extend(data)
            screen = probe.Screen(120, 34)
            screen.feed(bytes(transcript).decode("utf-8", "replace"))
            text = "\n".join(screen.lines())
            if "enter steers the live turn" not in text and "Thinking" not in text:
                idle = True
                break
            time.sleep(0.3)
        if not idle:
            raise RuntimeError("the turn never returned to idle")

        probe.pump(master, 0.6, transcript)
        os.write(master, b"/review\r")
        probe.pump(master, 1.0, transcript)
        screen1 = probe.Screen(120, 34)
        screen1.feed(bytes(transcript).decode("utf-8", "replace"))
        joined1 = "\n".join(screen1.lines())
        (evidence / "review-screen.txt").write_text(joined1 + "\n")

        os.write(master, b"n")
        probe.pump(master, 0.3, transcript)
        os.write(master, b"keep this wording")
        probe.pump(master, 0.3, transcript)
        os.write(master, b"\r")
        probe.pump(master, 0.8, transcript)
        screen2 = probe.Screen(120, 34)
        screen2.feed(bytes(transcript).decode("utf-8", "replace"))
        joined2 = "\n".join(screen2.lines())
        (evidence / "review-note-screen.txt").write_text(joined2 + "\n")

        result = {
            "review_opened": "review turn 1" in joined1,
            "diff_shown": "beta changed" in joined1,
            "note_saved": "notes: 1" in joined2,
            "note_prompt_gone": "press s to send them to the agent" in joined2,
        }
        os.write(master, b"\x1b")
        probe.pump(master, 0.4, transcript)
        os.write(master, b"/quit\r")
        probe.pump(master, 0.5, transcript)
        result["exit_code"] = probe.reap(pid, 5)

        conn = sqlite3.connect(home / "baton.sqlite")
        rows = list(conn.execute("SELECT path, line, body, sent_at FROM review_notes"))
        conn.close()
        result["stored_notes"] = rows
        passed = (
            all(result[key] for key in ["review_opened", "diff_shown", "note_saved", "note_prompt_gone"])
            and result["exit_code"] == 0
            and len(rows) == 1
            and rows[0][2] == "keep this wording"
            and rows[0][3] is None
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
    evidence = Path(args.evidence_dir) / f"review-probe-{int(time.time())}"
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
