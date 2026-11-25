#!/usr/bin/env python3
"""Drive a Codex user-input question through the TUI and prove the answers are sent."""

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


FAKE_CODEX = '''#!/usr/bin/env python3
import json
import os
import sys

state_path = os.environ.get("BATON_QUESTION_STATE")
if "app-server" not in sys.argv:
    print("codex 0.0.0")
    sys.exit(0)


def send(value):
    sys.stdout.write(json.dumps(value) + "\\n")
    sys.stdout.flush()


def record(value):
    if state_path:
        with open(state_path, "a", encoding="utf-8") as handle:
            handle.write(json.dumps(value) + "\\n")
            handle.flush()


question_sent = False
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
        send({"jsonrpc": "2.0", "id": request_id, "result": {"thread": {"id": "native-question", "path": None}}})
    elif method == "thread/resume":
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif method == "model/list":
        send({"jsonrpc": "2.0", "id": request_id, "result": {"data": [], "nextCursor": None}})
    elif method == "turn/start":
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"turn": {"id": "turn-question", "status": "inProgress"}},
        })
        if not question_sent:
            question_sent = True
            send({
                "jsonrpc": "2.0",
                "id": 91,
                "method": "item/tool/requestUserInput",
                "params": {
                    "threadId": "native-question",
                    "turnId": "turn-question",
                    "itemId": "call-question",
                    "questions": [
                        {
                            "id": "schema_preference",
                            "header": "Fixture",
                            "question": "Strict schemas or UI flexibility?",
                            "isOther": True,
                            "isSecret": False,
                            "options": [
                                {
                                    "label": "Strict schemas",
                                    "description": "Keep fixture data tightly validated.",
                                },
                                {
                                    "label": "UI flexibility",
                                    "description": "Allow looser fixture shapes.",
                                },
                            ],
                        },
                        {
                            "id": "note",
                            "header": "Note",
                            "question": "Add a note?",
                            "isOther": True,
                            "isSecret": False,
                            "options": [],
                        },
                    ],
                },
            })
    elif request_id == 91:
        record(frame)
        send({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {
                "threadId": "native-question",
                "turn": {
                    "id": "turn-question",
                    "status": "completed",
                    "items": [
                        {
                            "type": "agentMessage",
                            "id": "msg-question",
                            "text": "question answered",
                            "phase": "final_answer",
                        }
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

    root = Path(tempfile.mkdtemp(prefix="baton-question-"))
    home = root / "home"
    workdir = root / "work"
    fake_bin = root / "bin"
    evidence = Path(args.evidence_dir) / f"question-probe-{int(time.time())}"
    home.mkdir(parents=True, exist_ok=True)
    workdir.mkdir(parents=True, exist_ok=True)
    evidence.mkdir(parents=True, exist_ok=True)
    write_fake_codex(fake_bin)
    (home / "config.json").write_text(
        json.dumps({"defaultProvider": "codex", "defaultMode": "single"}) + "\n"
    )
    state_path = root / "responses.jsonl"

    env = os.environ.copy()
    env["BATON_HOME"] = str(home)
    env["BATON_QUESTION_STATE"] = str(state_path)
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
    answers = None
    try:
        if not wait_for_screen(
            master, "Ask anything", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("TUI did not show the composer")
        pump(master, 1.5, transcript)
        os.write(master, b"ask me a question\r")
        if not wait_for_screen(
            master, "question 1/2", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("TUI did not show the first question")
        if not wait_for_screen(
            master, "Strict schemas", 8.0, args.width, args.height, transcript
        ):
            raise RuntimeError("the first question did not list its options")
        pump(master, 0.3, transcript)
        before = Screen(args.width, args.height)
        before.feed(bytes(transcript).decode("utf-8", "replace"))
        before_text = "\n".join(before.lines())
        (evidence / "screen-question-one.txt").write_text(before_text + "\n")

        os.write(master, b"\x1b[B\r")
        if not wait_for_screen(
            master, "question 2/2", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the TUI did not advance to the second question")
        os.write(master, b"\r")
        if not wait_for_screen(
            master, "answer:", 8.0, args.width, args.height, transcript
        ):
            raise RuntimeError("the second question did not open the custom answer")
        for character in b"custom note":
            os.write(master, bytes([character]))
        os.write(master, b"\r")
        if not wait_for_screen(
            master, "question answered", args.timeout, args.width, args.height, transcript
        ):
            raise RuntimeError("the fake Codex server did not receive the answers")
        pump(master, 0.5, transcript)
        after = Screen(args.width, args.height)
        after.feed(bytes(transcript).decode("utf-8", "replace"))
        after_text = "\n".join(after.lines())
        (evidence / "screen-after.txt").write_text(after_text + "\n")

        if state_path.is_file():
            for line in state_path.read_text().splitlines():
                try:
                    frame = json.loads(line)
                except json.JSONDecodeError:
                    continue
                result = frame.get("result", {})
                if "answers" in result:
                    answers = result["answers"]
        expected = {
            "schema_preference": ["UI flexibility"],
            "note": ["custom note"],
        }
        missing = [
            label
            for label in ["Strict schemas", "UI flexibility"]
            if label not in before_text
        ]
        passed = (
            answers is not None
            and answers.get("schema_preference", {}).get("answers") == expected["schema_preference"]
            and answers.get("note", {}).get("answers") == expected["note"]
            and "question answered" in after_text
            and not missing
        )
        if missing:
            error = f"missing choices: {missing}"
        elif answers is None:
            error = "the fake Codex server did not receive a question response"
        elif answers.get("schema_preference", {}).get("answers") != expected["schema_preference"]:
            error = f"first answer was {answers.get('schema_preference')!r}"
        elif answers.get("note", {}).get("answers") != expected["note"]:
            error = f"second answer was {answers.get('note')!r}"
        elif "question answered" not in after_text:
            error = "the completed turn is missing from the final screen"
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
            "answers": answers,
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
