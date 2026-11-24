#!/usr/bin/env python3
"""Run every slash command in an isolated TUI and assert its stable surface."""

import argparse
import json
import os
import pty
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from tui_probe import Screen, binary_identity, pump, set_size, wait_for

COMMANDS = [
    ("/help", ["Type what you want to do"]),
    ("/help all", ["/auth", "/turns"]),
    ("/status", ["thread:", "adapters:"]),
    ("/usage", ["no usage yet"]),
    ("/context", ["context:"]),
    ("/turns", ["no turn records yet"]),
    ("/queue", ["queue: empty"]),
    ("/activity", ["no turns yet"]),
    ("/commands", ["no tool calls recorded yet"]),
    ("/find zzz", ["no matches for"]),
    ("/switch", ["switch harness", "antigravity"]),
    ("/model", ["models ·", "enter", "esc"]),
    ("/effort", ["reasoning effort", "xhigh", "max"]),
    ("/mode", ["permission mode", "acceptEdits", "plan"]),
    ("/auth", ["billing mode", "1/12", "enter apply"]),
    ("/roles", ["roles (4)", "orchestrator", "reviewer"]),
    ("/settings", ["+ global", "default session mode", "enter apply"]),
    ("/mcp", ["MCP servers", "add server", "sync to all harnesses"]),
    ("/skills", ["skills (2)", "claude-demo", "codex-demo"]),
    ("/memory", ["Memory ·", "Auto-memory"]),
    ("/threads", ["threads ·", "New thread"]),
    ("/pin", ["pin ·", "New thread"]),
    ("/inbox", ["inbox empty"]),
    ("/open", ["threads ·"]),
    ("/runs", ["no active orchestration runs"]),
    ("/checkpoints", ["no checkpoints yet"]),
    ("/review", ["no turn diff yet"]),
    ("/diff", ["no checkpoints yet"]),
    ("/rewind", ["nothing to rewind to yet"]),
    ("/retry", ["retry: no failed or unverified turns"]),
    ("/goal", ["usage: /goal"]),
    ("/multi", ["usage: /multi"]),
    ("/orchestrate", ["usage: /orchestrate"]),
    ("/orchestrate off", ["single harness mode"]),
    ("/orchestrate on", ["orchestrate mode"]),
    (
        "/git",
        [
            "Git · whole repository",
            "Commit all changes",
            "Push current branch",
            "Checks",
            "Open pull request",
        ],
    ),
    ("/compact", ["not enough conversation to compact"]),
    ("/init", ["created "]),
    ("/export", ["nothing to export"]),
    ("/copy", ["no assistant replies to copy yet"]),
    ("/clear", []),
    ("/new", ["new thread, unsaved until you send a message"]),
    ("/fork", ["nothing to fork yet"]),
    ("/worktree", ["source checkout has uncommitted changes"]),
    ("/exit", []),
]

FORBIDDEN = [
    "thread 'main' panicked",
    "panicked at",
    "unknown command",
    "unknown /git action",
    "cannot fork: thread not found",
]


def screen_text(transcript: bytearray, width: int, height: int) -> str:
    screen = Screen(width, height)
    screen.feed(bytes(transcript).decode("utf-8", "replace"))
    return "\n".join(screen.lines())


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/baton")
    parser.add_argument("--width", type=int, default=120)
    parser.add_argument("--height", type=int, default=40)
    parser.add_argument("--timeout", type=float, default=15.0)
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.is_file():
        print(f"missing binary: {binary}", file=sys.stderr)
        return 2

    home = tempfile.mkdtemp(prefix="baton-sweep-home-")
    workdir = tempfile.mkdtemp(prefix="baton-sweep-cwd-")
    claude_config = Path(home) / "claude-config"
    codex_config = Path(home) / "codex-config"
    for root, name in ((claude_config, "claude-demo"), (codex_config, "codex-demo")):
        skill = root / "skills" / name
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_text(f"---\nname: {name}\n---\n")
    (Path(home) / "config.json").write_text(
        json.dumps({"defaultProvider": "claude"}) + "\n"
    )
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=workdir, check=True)
    (Path(workdir) / "README.md").write_text("# sweep\n")
    subprocess.run(
        ["git", "config", "user.email", "sweep@example.com"], cwd=workdir, check=True
    )
    subprocess.run(["git", "config", "user.name", "sweep"], cwd=workdir, check=True)
    subprocess.run(
        ["git", "config", "commit.gpgsign", "false"], cwd=workdir, check=True
    )
    subprocess.run(["git", "add", "."], cwd=workdir, check=True)
    subprocess.run(["git", "commit", "-q", "-m", "init"], cwd=workdir, check=True)

    evidence = Path(args.evidence_dir) / f"slash-sweep-{int(time.time())}"
    evidence.mkdir(parents=True, exist_ok=True)
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(workdir)
        env = os.environ.copy()
        env["BATON_HOME"] = home
        env["TERM"] = "xterm-256color"
        env["CLAUDE_CONFIG_DIR"] = str(claude_config)
        env["CODEX_HOME"] = str(codex_config)
        env["BATON_CLIPBOARD_COMMAND"] = "cat"
        env["VISUAL"] = "/bin/true"
        env["EDITOR"] = "/bin/true"
        os.execvpe(str(binary), [str(binary)], env)

    transcript = bytearray()
    missing: list[str] = []
    forbidden: list[str] = []
    exit_code = None
    try:
        set_size(master, args.width, args.height)
        if not wait_for(master, "Ask anything", args.timeout, transcript):
            raise RuntimeError("TUI did not show its composer")
        pump(master, 3.0, transcript)
        for index, (command, required) in enumerate(COMMANDS):
            os.write(master, command.encode())
            pump(master, 0.6, transcript)
            os.write(master, b"\r")
            pump(master, 0.9, transcript)
            screen = screen_text(transcript, args.width, args.height)
            name = command.strip("/").replace(" ", "_") or "root"
            (evidence / f"{index:02d}-{name}.txt").write_text(screen + "\n")
            for value in required:
                if value not in screen:
                    missing.append(f"{command}: {value}")
            for value in FORBIDDEN:
                if value in screen:
                    forbidden.append(f"{command}: {value}")
            if command == "/exit":
                break
            os.write(master, b"\x1b")
            pump(master, 0.4, transcript)
        pump(master, 1.0, transcript)
        deadline = time.time() + 3.0
        waited = 0
        status = 0
        while time.time() < deadline:
            waited, status = os.waitpid(pid, os.WNOHANG)
            if waited:
                break
            time.sleep(0.1)
        if not waited:
            missing.append("/exit: TUI did not exit")
        else:
            exit_code = os.waitstatus_to_exitcode(status)
    except Exception as error:  # noqa: BLE001
        missing.append(f"sweep error: {error}")
    finally:
        try:
            os.close(master)
        except OSError:
            pass
        shutil.rmtree(home, ignore_errors=True)
        shutil.rmtree(workdir, ignore_errors=True)

    summary = {
        "passed": not missing and not forbidden and exit_code == 0,
        "binary": binary_identity(binary),
        "commands": len(COMMANDS),
        "missing": missing,
        "forbidden": forbidden,
        "exit_code": exit_code,
        "evidence": str(evidence),
    }
    (evidence / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
