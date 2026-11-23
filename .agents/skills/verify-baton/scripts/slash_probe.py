#!/usr/bin/env python3
"""Prove that slash arguments expose choices and complete in the real TUI."""

import argparse
import json
import os
import pty
import re
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from tui_probe import Screen, pump, reap, set_size, wait_for


def screen_text(transcript: bytearray, width: int, height: int) -> str:
    screen = Screen(width, height)
    screen.feed(bytes(transcript).decode("utf-8", "replace"))
    return "\n".join(screen.lines())


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

    home = tempfile.mkdtemp(prefix="baton-slash-home-")
    workdir = tempfile.mkdtemp(prefix="baton-slash-cwd-")
    (Path(workdir) / "src" / "parser").mkdir(parents=True)
    (Path(workdir) / "src" / "parser" / "mod.rs").write_text("pub fn parse() {}\n")
    (Path(workdir) / "README.md").write_text("# readme\n")
    editor_bin = Path(home) / "bin"
    editor_bin.mkdir()
    editor_capture = Path(home) / "editor-args.txt"
    editor_fail = Path(home) / "editor-fail"
    editor = editor_bin / "code"
    editor.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' \"$@\" > \"$BATON_EDITOR_CAPTURE\"\n"
        "if [ -f \"$BATON_EDITOR_FAIL\" ]; then exit 7; fi\n"
        "exit 0\n"
    )
    editor.chmod(0o755)
    subprocess.run(["git", "init", "-q"], cwd=workdir, check=True)
    evidence = Path(args.evidence_dir) / f"slash-probe-{int(time.time())}"
    evidence.mkdir(parents=True, exist_ok=True)
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(workdir)
        env = os.environ.copy()
        env["BATON_HOME"] = home
        env["TERM"] = "xterm-256color"
        env["BATON_EDITOR_CAPTURE"] = str(editor_capture)
        env["BATON_EDITOR_FAIL"] = str(editor_fail)
        env["EDITOR"] = "code --wait"
        env["PATH"] = f"{editor_bin}{os.pathsep}{env.get('PATH', '')}"
        env.pop("VISUAL", None)
        os.execvpe(str(binary), [str(binary)], env)

    transcript = bytearray()
    missing = []
    rejected = []
    exit_code = None
    try:
        set_size(master, args.width, args.height)
        if not wait_for(master, "Ask anything", args.timeout, transcript):
            raise RuntimeError("TUI did not show its composer")
        pump(master, 3.0, transcript)

        os.write(master, b"/find ")
        pump(master, 0.5, transcript)
        hint_screen = screen_text(transcript, args.width, args.height)
        missing.extend(
            value for value in ["/find <text>"] if value not in hint_screen
        )
        (evidence / "hint-screen.txt").write_text(hint_screen + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/mode ")
        pump(master, 0.8, transcript)
        choices = screen_text(transcript, args.width, args.height)
        required = [
            "/mode default",
            "/mode acceptEdits",
            "/mode bypassPermissions",
            "/mode plan",
            "tab/enter complete",
        ]
        missing.extend(value for value in required if value not in choices)
        (evidence / "choices-screen.txt").write_text(choices + "\n")

        os.write(master, b"\t")
        pump(master, 0.3, transcript)
        os.write(master, b"\r")
        pump(master, 0.8, transcript)
        result = screen_text(transcript, args.width, args.height)
        expected_result = "permission mode: default"
        if expected_result not in result:
            missing.append(expected_result)
        (evidence / "result-screen.txt").write_text(result + "\n")

        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)
        os.write(master, b"/model\r")
        pump(master, 1.5, transcript)
        model_picker = screen_text(transcript, args.width, args.height)
        model_required = ["models ·", "type to filter", "enter", "esc"]
        missing.extend(value for value in model_required if value not in model_picker)
        model_header = re.search(
            r"[a-z-]+ models · (\d+)/(\d+) · type to filter · enter · esc",
            model_picker,
        )
        if model_header is None or int(model_header.group(1)) < 2:
            missing.append("model picker count is at least 2")
        model_rows = re.findall(r"❯\s+(?:●\s+)?\S+", model_picker)
        if len(model_rows) < 2:
            missing.append("model picker has at least 2 option rows")
        (evidence / "model-picker-screen.txt").write_text(model_picker + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/files ")
        pump(master, 0.5, transcript)
        files_hint = screen_text(transcript, args.width, args.height)
        if "files [query]" not in files_hint:
            missing.append("/files <query> hint")
        (evidence / "files-hint-screen.txt").write_text(files_hint + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/files pmod rs:12\r")
        pump(master, 1.5, transcript)
        files_picker = screen_text(transcript, args.width, args.height)
        files_required = [
            'files matching "pmod rs"',
            "line 12",
            "src/parser/mod.rs",
            "enter insert",
            "ctrl+o open",
            "esc close",
        ]
        missing.extend(value for value in files_required if value not in files_picker)
        if "README.md" in files_picker:
            missing.append("files picker leaked an unmatched row")
        (evidence / "files-picker-screen.txt").write_text(files_picker + "\n")
        os.write(master, b"\r")
        pump(master, 1.0, transcript)
        files_result = screen_text(transcript, args.width, args.height)
        if "inserted src/parser/mod.rs:12" not in files_result:
            missing.append("files insert notice")
        (evidence / "files-result-screen.txt").write_text(files_result + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/files pmod rs:12\r")
        pump(master, 1.5, transcript)
        os.write(master, b"\x0f")
        if not wait_for(master, "opened src/parser/mod.rs:12", 3.0, transcript):
            missing.append("files ctrl+o opened notice")
        pump(master, 0.5, transcript)
        editor_result = screen_text(transcript, args.width, args.height)
        (evidence / "files-open-screen.txt").write_text(editor_result + "\n")
        if not editor_capture.is_file():
            missing.append("files ctrl+o editor process")
        else:
            editor_args = editor_capture.read_text().splitlines()
            (evidence / "files-open-args.txt").write_text(
                json.dumps(editor_args, indent=2) + "\n"
            )
            if "--goto" not in editor_args:
                missing.append("files ctrl+o --goto")
            if not editor_args or not editor_args[-1].endswith(
                "src/parser/mod.rs:12"
            ):
                missing.append("files ctrl+o line target")

        editor_fail.write_text("fail\n")
        os.write(master, b"/files pmod rs:12\r")
        pump(master, 1.5, transcript)
        os.write(master, b"\x0f")
        if not wait_for(master, "editor failed:", 3.0, transcript):
            missing.append("files ctrl+o editor failure notice")
        pump(master, 0.5, transcript)
        os.write(master, b"/status\r")
        pump(master, 1.0, transcript)
        after_failure = screen_text(transcript, args.width, args.height)
        if "adapters:" not in after_failure:
            missing.append("TUI accepted input after editor failure")
        (evidence / "files-open-failure-screen.txt").write_text(
            after_failure + "\n"
        )

        os.write(master, b"/files \r")
        pump(master, 1.5, transcript)
        recent_picker = screen_text(transcript, args.width, args.height)
        for value in ["files · 2 · enter insert", "README.md", "❯ src/parser/mod.rs"]:
            if value not in recent_picker:
                missing.append(f"recent file ranking: {value}")
        if 'files matching "pmod rs"' in recent_picker:
            missing.append("recent file ranking kept a stale filter")
        (evidence / "files-recent-screen.txt").write_text(recent_picker + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/grep ")
        pump(master, 0.5, transcript)
        grep_hint = screen_text(transcript, args.width, args.height)
        if "grep [--case] [--word] [--regex] <query>" not in grep_hint:
            missing.append("/grep usage hint")
        (evidence / "grep-hint-screen.txt").write_text(grep_hint + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/grep parse\r")
        pump(master, 1.5, transcript)
        grep_picker = screen_text(transcript, args.width, args.height)
        grep_required = [
            'grep "parse"',
            "src/parser/mod.rs:1",
            "ctrl+o open",
            "enter insert",
            "esc close",
        ]
        missing.extend(value for value in grep_required if value not in grep_picker)
        os.write(master, b"pa")
        pump(master, 0.4, transcript)
        grep_filtered = screen_text(transcript, args.width, args.height)
        if "src/parser/mod.rs:1" not in grep_filtered:
            missing.append("grep filtered row")
        (evidence / "grep-picker-screen.txt").write_text(grep_filtered + "\n")
        os.write(master, b"\r")
        pump(master, 1.0, transcript)
        grep_result = screen_text(transcript, args.width, args.height)
        if "inserted src/parser/mod.rs:1" not in grep_result:
            missing.append("grep insert notice")
        (evidence / "grep-result-screen.txt").write_text(grep_result + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        if editor_fail.exists():
            editor_fail.unlink()
        os.write(master, b"/grep parse\r")
        pump(master, 1.5, transcript)
        os.write(master, b"\x0f")
        if not wait_for(master, "opened src/parser/mod.rs:1", 3.0, transcript):
            missing.append("grep ctrl+o opened notice")
        pump(master, 0.5, transcript)
        grep_open_result = screen_text(transcript, args.width, args.height)
        (evidence / "grep-open-screen.txt").write_text(grep_open_result + "\n")
        if not editor_capture.is_file():
            missing.append("grep ctrl+o editor process")
        else:
            editor_args = editor_capture.read_text().splitlines()
            (evidence / "grep-open-args.txt").write_text(
                json.dumps(editor_args, indent=2) + "\n"
            )
            if "--goto" not in editor_args:
                missing.append("grep ctrl+o --goto")
            if not editor_args or not editor_args[-1].endswith(
                "src/parser/mod.rs:1"
            ):
                missing.append("grep ctrl+o line target")

        os.write(master, b"/pin\r")
        pump(master, 1.5, transcript)
        pin_picker = screen_text(transcript, args.width, args.height)
        pin_required = ["pin · ctrl+↑/↓ move · enter toggle · esc close", "New thread"]
        missing.extend(value for value in pin_required if value not in pin_picker)
        (evidence / "pin-picker-screen.txt").write_text(pin_picker + "\n")

        os.write(master, b"\r")
        pump(master, 1.0, transcript)
        pin_result = screen_text(transcript, args.width, args.height)
        if "pinned New thread" not in pin_result:
            missing.append("pinned notice")
        if "New thread  [new]  pinned" not in pin_result:
            missing.append("pinned picker row")
        (evidence / "pin-result-screen.txt").write_text(pin_result + "\n")

        first_thread = re.search(
            r"❯ ([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})  New thread",
            pin_result,
        )
        if first_thread is None:
            missing.append("pinned thread id")
        else:
            first_id = first_thread.group(1)
            os.write(master, b"\x1b")
            pump(master, 0.2, transcript)
            os.write(master, b"/new\r")
            pump(master, 1.0, transcript)
            os.write(master, b"/model\r")
            pump(master, 1.5, transcript)
            os.write(master, b"\x1b")
            pump(master, 0.2, transcript)

            os.write(master, b"/pin\r")
            pump(master, 1.5, transcript)
            os.write(master, b"\x1b[B")
            pump(master, 0.2, transcript)
            os.write(master, b"\r")
            pump(master, 1.0, transcript)
            second_result = screen_text(transcript, args.width, args.height)
            if "pin · ctrl+↑/↓ move · enter toggle · esc close" not in second_result:
                missing.append("second pin picker")
            if second_result.count("  pinned") < 2:
                missing.append("second pinned row")
            second_thread = re.search(
                r"❯ ([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})  New thread",
                second_result,
            )
            if second_thread is None:
                missing.append("second pinned thread id")
            else:
                second_id = second_thread.group(1)
                os.write(master, b"\x1b")
                pump(master, 0.2, transcript)
                os.write(master, f"/pin move {second_id} down\r".encode())
                pump(master, 1.0, transcript)
                pin_move = screen_text(transcript, args.width, args.height)
                if "moved New thread down" not in pin_move:
                    missing.append("pin move notice")
                if "pin · ctrl+↑/↓ move · enter toggle · esc close" not in pin_move:
                    missing.append("refreshed pin picker")
                order = re.findall(
                    r"(?:❯| )([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})  New thread",
                    pin_move,
                )
                if order[:2] != [first_id, second_id]:
                    missing.append("pin move order")
                (evidence / "pin-move-screen.txt").write_text(pin_move + "\n")

                os.write(master, b"\x1b")
                pump(master, 0.2, transcript)
                os.write(master, f"/pin move {second_id} down\r".encode())
                pump(master, 1.0, transcript)
                pin_boundary = screen_text(transcript, args.width, args.height)
                if "already last among pinned threads" not in pin_boundary:
                    missing.append("pin move boundary notice")
                (evidence / "pin-boundary-screen.txt").write_text(
                    pin_boundary + "\n"
                )

                os.write(master, b"/pin\r")
                pump(master, 1.5, transcript)
                os.write(master, b"\x1b[1;5B")
                pump(master, 1.0, transcript)
                ctrl_move = screen_text(transcript, args.width, args.height)
                if "moved New thread down" not in ctrl_move:
                    missing.append("ctrl pin move notice")
                if "pin · ctrl+↑/↓ move · enter toggle · esc close" not in ctrl_move:
                    missing.append("ctrl pin refreshed picker")
                ctrl_order = re.findall(
                    r"(?:❯| )([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})  New thread",
                    ctrl_move,
                )
                if ctrl_order[:2] != [second_id, first_id]:
                    missing.append("ctrl pin move order")
                selected = re.search(
                    r"❯ ([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})  New thread",
                    ctrl_move,
                )
                if selected is None or selected.group(1) != first_id:
                    missing.append("ctrl pin selected row")
                (evidence / "pin-ctrl-move-screen.txt").write_text(ctrl_move + "\n")

        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)
        os.write(master, b"/quit\r")
        pump(master, 0.5, transcript)
        exit_code = reap(pid, 5)
        passed = not missing and not rejected and exit_code == 0
        summary = {
            "passed": passed,
            "missing": missing,
            "rejected": rejected,
            "exit_code": exit_code,
            "evidence": str(evidence),
        }
        (evidence / "transcript.raw").write_bytes(bytes(transcript))
        (evidence / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps(summary, indent=2))
        return 0 if passed else 1
    except Exception as error:
        try:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        except OSError:
            pass
        (evidence / "transcript.raw").write_bytes(bytes(transcript))
        print(json.dumps({"passed": False, "error": str(error), "evidence": str(evidence)}, indent=2))
        return 1
    finally:
        try:
            os.close(master)
        except OSError:
            pass
        import shutil
        shutil.rmtree(home, ignore_errors=True)
        shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
