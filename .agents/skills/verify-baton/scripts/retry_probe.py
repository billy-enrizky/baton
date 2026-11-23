#!/usr/bin/env python3
"""Live TUI retry: seed one failed turn, then retry it from /retry.

A fake Claude harness completes the retry without a model call, so the probe
spends no tokens and needs no credentials. It uses a throwaway BATON_HOME and
working directory and never touches the real store.
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


def seed_failed_turn(binary: str, env: dict, cwd: Path, home: Path) -> str:
    created = subprocess.run(
        [binary, "-C", str(cwd), "new", "retry fixture"],
        env=env,
        capture_output=True,
        text=True,
        timeout=20,
        check=True,
    )
    if "created thread" not in created.stdout:
        raise RuntimeError(f"baton new did not create a thread: {created.stdout!r}")
    conn = sqlite3.connect(home / "baton.sqlite")
    row = conn.execute(
        "SELECT id FROM threads WHERE cwd = ? ORDER BY created_at DESC LIMIT 1",
        (str(cwd),),
    ).fetchone()
    if row is None:
        conn.close()
        raise RuntimeError("baton new left no thread row")
    thread_id = row[0]
    stamp = "2026-11-22T00:00:00Z"
    conn.execute(
        "UPDATE threads SET current_provider = 'claude', current_model = 'claude-sonnet-5-5', permission_mode = 'default' WHERE id = ?",
        (thread_id,),
    )
    for turn, prompt, error in [
        (1, "retry seed", "fixture failure"),
        (10, "retry seed ten", "fixture failure ten"),
    ]:
        conn.execute(
            "INSERT INTO messages (thread_id, turn, role, provider, model, text, created_at) VALUES (?, ?, 'user', 'claude', NULL, ?, ?)",
            (thread_id, turn, prompt, stamp),
        )
        conn.execute(
            "INSERT INTO turns (thread_id, turn, provider, model, phase, error, provider_turn_id, started_at, ended_at) VALUES (?, ?, 'claude', NULL, 'failed', ?, NULL, ?, ?)",
            (thread_id, turn, error, stamp, stamp),
        )
    conn.execute(
        "INSERT INTO provider_sessions (thread_id, provider, native_session_id, model, rollout_path, updated_at) VALUES (?, 'claude', 'fake-session', NULL, NULL, ?)",
        (thread_id, stamp),
    )
    conn.commit()
    conn.close()
    return thread_id


def run(binary: str, timeout: float, evidence: Path) -> dict:
    home = Path(tempfile.mkdtemp(prefix="baton-retry-home-"))
    cwd = Path(tempfile.mkdtemp(prefix="baton-retry-cwd-"))
    shim = Path(tempfile.mkdtemp(prefix="baton-retry-shim-"))
    claude_config = Path(tempfile.mkdtemp(prefix="baton-retry-claude-"))
    args_log = shim / "claude-args"
    stdin_log = shim / "claude-stdin"
    fake_claude = shim / "claude"
    fake_claude.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' \"$@\" > \"$BATON_RETRY_ARGS\"\n"
        "IFS= read -r line || exit 1\n"
        "printf '%s\\n' \"$line\" > \"$BATON_RETRY_STDIN\"\n"
        "printf '%s\\n' '{\"type\":\"stream_event\",\"session_id\":\"fake-claude-session\",\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"retried \"}}}'\n"
        "printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"retried successfully\",\"session_id\":\"fake-claude-session\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}'\n"
    )
    fake_claude.chmod(0o755)
    subprocess.run(
        ["git", "init", "-q"],
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=10,
        check=True,
    )
    env = os.environ.copy()
    env["BATON_HOME"] = str(home)
    env["CLAUDE_CONFIG_DIR"] = str(claude_config)
    env["BATON_RETRY_ARGS"] = str(args_log)
    env["BATON_RETRY_STDIN"] = str(stdin_log)
    env["PATH"] = f"{shim}:{env.get('PATH', '')}"
    try:
        thread_id = seed_failed_turn(binary, env, cwd, home)
        pid, master = pty.fork()
        if pid == 0:
            os.chdir(cwd)
            child_env = env.copy()
            child_env["TERM"] = "xterm-256color"
            os.execvpe(binary, [binary, "-t", thread_id], child_env)

        transcript = bytearray()
        reaped = False

        def screen_text() -> str:
            screen = probe.Screen(120, 34)
            screen.feed(bytes(transcript).decode("utf-8", "replace"))
            return "\n".join(screen.lines())

        try:
            probe.set_size(master, 120, 34)
            if not probe.wait_for(master, "Ask anything", 15, transcript):
                raise RuntimeError("TUI never became ready")
            probe.pump(master, 3.0, transcript)
            os.write(master, b"/retry ")
            probe.pump(master, 0.4, transcript)
            completion = screen_text()
            (evidence / "completion-screen.txt").write_text(completion + "\n")
            completion_lines = completion.splitlines()
            has_one = any(
                "/retry 1 " in line
                and "failed · retry seed" in line
                and "retry seed ten" not in line
                for line in completion_lines
            )
            has_ten = any(
                "/retry 10 " in line and "failed · retry seed ten" in line
                for line in completion_lines
            )
            if not (has_one and has_ten):
                raise RuntimeError("slash completion did not show the retryable turn")
            os.write(master, b"\x1b")
            probe.pump(master, 0.2, transcript)
            os.write(master, b"/retry 1\r")
            probe.pump(master, 0.8, transcript)
            picker = screen_text()
            (evidence / "picker-screen.txt").write_text(picker + "\n")
            if not all(
                value in picker
                for value in [
                    "turn 1 failed",
                    "retry seed",
                    "fixture failure",
                    "current mode default",
                    "claude/claude-sonnet-5-5",
                    "retry one turn",
                    "session through turn 0",
                ]
            ):
                raise RuntimeError("the retry picker did not show the failed seed and context")

            os.write(master, b"\r")
            probe.pump(master, 0.5, transcript)
            deadline = time.time() + timeout
            while time.time() < deadline:
                data = probe.read_available(master)
                if data:
                    transcript.extend(data)
                try:
                    with sqlite3.connect(home / "baton.sqlite") as poll:
                        rows = list(
                            poll.execute("SELECT turn, phase FROM turns ORDER BY turn")
                        )
                except sqlite3.OperationalError:
                    rows = []
                if len(rows) >= 3 and rows[-1][1] == "completed":
                    break
                time.sleep(0.2)
            probe.pump(master, 0.5, transcript)
            final = screen_text()
            (evidence / "retry-screen.txt").write_text(final + "\n")
            if "retried successfully" not in final:
                raise RuntimeError("the retry reply did not reach the transcript")

            os.write(master, b"/quit\r")
            probe.pump(master, 0.5, transcript)
            exit_code = probe.reap(pid, 5)
            reaped = True

            conn = sqlite3.connect(home / "baton.sqlite")
            turns = list(conn.execute("SELECT turn, phase, error FROM turns ORDER BY turn"))
            users = list(
                conn.execute(
                    "SELECT turn, text FROM messages WHERE role = 'user' ORDER BY turn, id"
                )
            )
            assistants = list(
                conn.execute(
                    "SELECT turn, text FROM messages WHERE role = 'assistant' ORDER BY turn, id"
                )
            )
            orchestration_runs = conn.execute(
                "SELECT COUNT(*) FROM orchestration_runs"
            ).fetchone()[0]
            orchestration_tasks = conn.execute(
                "SELECT COUNT(*) FROM orchestration_tasks"
            ).fetchone()[0]
            session_mode = conn.execute(
                "SELECT session_mode FROM threads WHERE id = ?",
                (thread_id,),
            ).fetchone()[0]
            conn.close()
            retry_args = args_log.read_text().splitlines()
            provider_stdin = stdin_log.read_text()
            resume_pair = any(
                retry_args[index : index + 2] == ["--resume", "fake-session"]
                for index in range(len(retry_args) - 1)
            )
            result = {
                "thread_id": thread_id,
                "picker_showed_failed_turn": True,
                "turns": turns,
                "user_messages": users,
                "assistant_messages": assistants,
                "retry_args": retry_args,
                "provider_stdin": provider_stdin,
                "orchestration_runs": orchestration_runs,
                "orchestration_tasks": orchestration_tasks,
                "session_mode": session_mode,
                "resume_pair": resume_pair,
                "exit_code": exit_code,
            }
            result["passed"] = (
                exit_code == 0
                and turns
                == [
                    (1, "failed", "fixture failure"),
                    (10, "failed", "fixture failure ten"),
                    (11, "completed", None),
                ]
                and users
                == [
                    (1, "retry seed"),
                    (10, "retry seed ten"),
                    (11, "retry seed"),
                ]
                and assistants == [(11, "retried successfully")]
                and resume_pair
                and "retry seed" in provider_stdin
                and orchestration_runs == 0
                and orchestration_tasks == 0
                and session_mode == "orchestrate"
            )
            (evidence / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
            return result
        finally:
            if not reaped:
                try:
                    done, _ = os.waitpid(pid, os.WNOHANG)
                    if done == 0:
                        os.kill(pid, signal.SIGKILL)
                except (ChildProcessError, OSError):
                    pass
    finally:
        shutil.rmtree(home, ignore_errors=True)
        shutil.rmtree(cwd, ignore_errors=True)
        shutil.rmtree(shim, ignore_errors=True)
        shutil.rmtree(claude_config, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=str(Path.home() / ".cargo/bin/baton"))
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()
    evidence = Path(args.evidence_dir) / f"retry-probe-{int(time.time())}"
    evidence.mkdir(parents=True, exist_ok=True)
    try:
        result = run(str(Path(args.binary).expanduser().resolve()), args.timeout, evidence)
    except (RuntimeError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"passed": False, "error": str(error), "evidence": str(evidence)}, indent=2))
        return 1
    print(json.dumps({**result, "evidence": str(evidence)}, indent=2))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
