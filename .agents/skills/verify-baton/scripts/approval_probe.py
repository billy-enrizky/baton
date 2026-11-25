#!/usr/bin/env python3
"""Drive a live ACP permission request through the TUI and prove the choice is sent."""

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
import os
import sys

state_path = os.environ.get("BATON_APPROVAL_STATE")
if "acp" not in sys.argv:
    print("agy 1.0.0")
    sys.exit(0)


def send(value):
    sys.stdout.write(json.dumps(value) + "\\n")
    sys.stdout.flush()


def record(value):
    if state_path:
        with open(state_path, "a", encoding="utf-8") as handle:
            handle.write(json.dumps(value) + "\\n")
            handle.flush()


prompt_id = None
turn_count = 0
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
        send({"jsonrpc": "2.0", "id": request_id, "result": {"sessionId": "acp-approval-1"}})
    elif method in {
        "session/resume",
        "session/load",
        "session/set_model",
        "session/set_mode",
        "session/set_config_option",
    }:
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif method == "session/prompt":
        turn_count += 1
        prompt_id = request_id
        if turn_count == 1:
            send({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "acp-approval-1",
                    "update": {
                        "sessionUpdate": "tool_call",
                        "toolCallId": "tc-approval",
                        "title": "bash touch approval.txt",
                        "status": "in_progress",
                    },
                },
            })
            send({
                "jsonrpc": "2.0",
                "id": 9001,
                "method": "session/request_permission",
                "params": {
                    "sessionId": "acp-approval-1",
                    "toolCall": {"toolCallId": "tc-approval", "title": "bash touch approval.txt"},
                    "options": [
                        {"optionId": "opt-allow", "name": "Allow once", "kind": "allow_once"},
                        {"optionId": "opt-always", "name": "Always allow", "kind": "allow_always"},
                        {"optionId": "opt-reject", "name": "Reject", "kind": "reject_once"},
                    ],
                },
            })
        else:
            send({
                "jsonrpc": "2.0",
                "id": 9002,
                "method": "session/request_permission",
                "params": {
                    "sessionId": "acp-approval-1",
                    "toolCall": {
                        "toolCallId": "interaction_9960062f",
                        "title": "Which database should the service use?",
                    },
                    "options": [
                        {"optionId": "opt-postgres", "name": "Postgres", "kind": "allow_once"},
                        {"optionId": "opt-sqlite", "name": "SQLite", "kind": "allow_once"},
                    ],
                },
            })
    elif request_id in {9001, 9002}:
        record(frame)
        text = "approval handled" if request_id == 9001 else "question handled"
        send({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "acp-approval-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": text},
                },
            },
        })
        send({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "acp-approval-1",
                "update": {"sessionUpdate": "state_update", "state": "idle", "stopReason": "end_turn"},
            },
        })
        if prompt_id is not None:
            send({"jsonrpc": "2.0", "id": prompt_id, "result": {"stopReason": "end_turn"}})
            prompt_id = None
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

    root = Path(tempfile.mkdtemp(prefix="baton-approval-"))
    home = root / "home"
    workdir = root / "work"
    fake_bin = root / "bin"
    evidence = Path(args.evidence_dir) / f"approval-probe-{int(time.time())}"
    home.mkdir(parents=True, exist_ok=True)
    workdir.mkdir(parents=True, exist_ok=True)
    evidence.mkdir(parents=True, exist_ok=True)
    write_fake_agy(fake_bin)
    (home / "config.json").write_text(
        json.dumps({"defaultProvider": "antigravity", "defaultMode": "single"}) + "\n"
    )
    state_path = root / "responses.jsonl"

    env = os.environ.copy()
    env["BATON_HOME"] = str(home)
    env["BATON_APPROVAL_STATE"] = str(state_path)
    env["PATH"] = f"{fake_bin}:{env.get('PATH', '')}"
    env["TERM"] = "xterm-256color"

    pid, master = pty.fork()
    if pid == 0:
        os.chdir(workdir)
        os.execvpe(str(binary), [str(binary)], env)

    set_size(master, args.width, args.height)
    transcript = bytearray()
    exit_code = None
    passed = False
    error = None
    selected = None
    question_selected = None
    try:
        if not wait_for_screen(
            master, "Ask anything", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("TUI did not show the composer")
        pump(master, 1.5, transcript)
        os.write(master, b"run a command\r")
        if not wait_for_screen(
            master, "approval needed", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("TUI did not show the approval request")
        if not wait_for_screen(
            master, "Allow once", 8.0, args.width, args.height, transcript
        ):
            raise RuntimeError("approval overlay did not list the provider choices")
        pump(master, 0.3, transcript)
        before = Screen(args.width, args.height)
        before.feed(bytes(transcript).decode("utf-8", "replace"))
        before_text = "\n".join(before.lines())
        (evidence / "screen-approval.txt").write_text(before_text + "\n")

        os.write(master, b"\x1b[B\x1b[B\r")
        if not wait_for_screen(
            master, "approval handled", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the fake ACP agent did not receive a completed turn")
        pump(master, 0.5, transcript)
        after = Screen(args.width, args.height)
        after.feed(bytes(transcript).decode("utf-8", "replace"))
        after_text = "\n".join(after.lines())
        (evidence / "screen-after.txt").write_text(after_text + "\n")

        os.write(master, b"ask which database\r")
        if not wait_for_screen(
            master,
            "Which database should the service use?",
            args.timeout,
            args.width,
            args.height,
            transcript,
        ):
            raise RuntimeError("TUI did not show the Antigravity question")
        if not wait_for_screen(
            master, "SQLite", 8.0, args.width, args.height, transcript
        ):
            raise RuntimeError("question overlay did not list the provider options")
        pump(master, 0.3, transcript)
        question = Screen(args.width, args.height)
        question.feed(bytes(transcript).decode("utf-8", "replace"))
        question_text = "\n".join(question.lines())
        (evidence / "screen-question.txt").write_text(question_text + "\n")

        os.write(master, b"\x1b[B\r")
        if not wait_for_screen(
            master, "question handled", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the fake ACP agent did not receive the question answer")
        pump(master, 0.5, transcript)
        question_after = Screen(args.width, args.height)
        question_after.feed(bytes(transcript).decode("utf-8", "replace"))
        question_after_text = "\n".join(question_after.lines())
        (evidence / "screen-question-after.txt").write_text(
            question_after_text + "\n"
        )

        if state_path.is_file():
            for line in state_path.read_text().splitlines():
                try:
                    frame = json.loads(line)
                except json.JSONDecodeError:
                    continue
                outcome = frame.get("result", {}).get("outcome", {})
                if frame.get("id") == 9001 and outcome.get("outcome") == "selected":
                    selected = outcome.get("optionId")
                if frame.get("id") == 9002 and outcome.get("outcome") == "selected":
                    question_selected = outcome.get("optionId")
        choices = ["Allow once", "Always allow", "Reject"]
        missing = [choice for choice in choices if choice not in before_text]
        question_choices = ["Which database should the service use?", "Postgres", "SQLite"]
        missing.extend(
            choice for choice in question_choices if choice not in question_text
        )
        passed = (
            not missing
            and selected == "opt-reject"
            and question_selected == "opt-sqlite"
            and "approval handled" in after_text
            and "question handled" in question_after_text
        )
        if missing:
            error = f"missing choices: {missing}"
        elif selected != "opt-reject":
            error = f"selected option was {selected!r}, expected 'opt-reject'"
        elif question_selected != "opt-sqlite":
            error = (
                f"question option was {question_selected!r}, expected 'opt-sqlite'"
            )
        elif "approval handled" not in after_text:
            error = "the completed turn is missing from the final screen"
        elif "question handled" not in question_after_text:
            error = "the answered question is missing from the final screen"
    except Exception as caught:  # noqa: BLE001
        error = str(caught)
    finally:
        try:
            os.write(master, b"/quit\r" if passed else b"\x03\x03")
            pump(master, 0.5, transcript)
        except OSError:
            pass
        exit_code = reap(pid, 5)
        (evidence / "transcript.raw").write_bytes(bytes(transcript))
        if state_path.is_file():
            shutil.copyfile(state_path, evidence / "responses.jsonl")
        summary = {
            "passed": passed and exit_code == 0 and error is None,
            "binary": binary_identity(binary),
            "selected": selected,
            "question_selected": question_selected,
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
