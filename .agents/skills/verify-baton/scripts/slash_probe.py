#!/usr/bin/env python3
"""Prove that slash arguments expose choices and complete in the real TUI."""

from __future__ import annotations

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
from tui_probe import Screen, binary_identity, pump, reap, set_size


def screen_text(transcript: bytearray, width: int, height: int) -> str:
    screen = Screen(width, height)
    screen.feed(bytes(transcript).decode("utf-8", "replace"))
    return "\n".join(screen.lines())


def wait_for_screen(
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


def select_memory_row(
    master: int,
    transcript: bytearray,
    width: int,
    height: int,
    label: str,
    tries: int = 30,
) -> bool:
    os.write(master, b"\x1b[H")
    pump(master, 0.1, transcript)
    needle = f"❯ {label}  "
    for _ in range(tries):
        text = screen_text(transcript, width, height)
        if any(needle in line for line in text.splitlines()):
            return True
        os.write(master, b"\x1b[B")
        pump(master, 0.08, transcript)
    return False


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
    gh = editor_bin / "gh"
    gh.write_text(
        "#!/bin/sh\n"
        "set -eu\n"
        "if [ \"$1\" = \"repo\" ] && [ \"$2\" = \"view\" ]; then\n"
        "  printf '%s\\n' '{\"defaultBranchRef\":{\"name\":\"main\"},\"nameWithOwner\":\"example/repo\"}'\n"
        "  exit 0\n"
        "fi\n"
        "if [ \"$1\" = \"pr\" ] && [ \"$2\" = \"view\" ]; then\n"
        "  if [ \"${3:-}\" = \"--json\" ]; then\n"
        "    cat <<'JSON'\n"
        '{"number":42,"title":"Add checks","url":"https://github.com/example/repo/pull/42","state":"OPEN","mergeable":"MERGEABLE","headRefName":"feat/checks","isDraft":false}\n'
        "JSON\n"
        "    exit 0\n"
        "  fi\n"
        "  if [ \"${4:-}\" != \"--repo\" ] || [ \"${5:-}\" != \"example/repo\" ]; then\n"
        "    printf 'missing --repo example/repo\\n' >&2\n"
        "    exit 1\n"
        "  fi\n"
        "  printf 'no pull requests found for branch\\n' >&2\n"
        "  exit 1\n"
        "fi\n"
        "if [ \"$1\" = \"pr\" ] && [ \"$2\" = \"checks\" ]; then\n"
        "  cat <<'JSON'\n"
        '[{"name":"Test","state":"SUCCESS","bucket":"pass","description":"","link":"https://example/check"},{"name":"Lint","state":"PENDING","bucket":"pending","description":"waiting","link":""}]\n'
        "JSON\n"
        "  exit 0\n"
        "fi\n"
        "if [ \"$1\" = \"pr\" ] && [ \"$2\" = \"create\" ]; then\n"
        "  if [ \"${3:-}\" != \"--repo\" ] || [ \"${4:-}\" != \"example/repo\" ]; then\n"
        "    printf 'missing --repo example/repo\\n' >&2\n"
        "    exit 1\n"
        "  fi\n"
        "  printf 'https://github.com/example/repo/pull/43\\n'\n"
        "  exit 0\n"
        "fi\n"
        "exit 1\n"
    )
    gh.chmod(0o755)
    # The login drive must never reach a real harness login, which can open a
    # browser. This fake records the argv and exits clean.
    fake_codex = editor_bin / "codex"
    fake_codex.write_text(
        "#!/bin/sh\n"
        "printf 'fake-codex-login %s\\n' \"$*\"\n"
        "exit 0\n"
    )
    fake_codex.chmod(0o755)
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=workdir, check=True)
    no_hooks = Path(home) / "no-hooks"
    no_hooks.mkdir()
    subprocess.run(
        ["git", "config", "core.hooksPath", str(no_hooks)], cwd=workdir, check=True
    )
    subprocess.run(
        ["git", "config", "commit.gpgsign", "false"], cwd=workdir, check=True
    )
    subprocess.run(
        ["git", "config", "push.autoSetupRemote", "false"],
        cwd=workdir,
        check=True,
    )
    subprocess.run(
        ["git", "config", "status.showUntrackedFiles", "all"],
        cwd=workdir,
        check=True,
    )
    empty_excludes = Path(home) / "empty-excludes"
    empty_excludes.write_text("")
    subprocess.run(
        ["git", "config", "core.excludesFile", str(empty_excludes)],
        cwd=workdir,
        check=True,
    )
    subprocess.run(
        ["git", "config", "user.email", "probe@example.com"], cwd=workdir, check=True
    )
    subprocess.run(["git", "config", "user.name", "probe"], cwd=workdir, check=True)
    remote_path = Path(home) / "remote.git"
    remote_path.mkdir()
    subprocess.run(
        ["git", "init", "--bare", "-q", "-b", "main"], cwd=remote_path, check=True
    )
    subprocess.run(
        ["git", "remote", "add", "origin", str(remote_path)], cwd=workdir, check=True
    )
    (Path(workdir) / ".git" / "info" / "exclude").write_text(".env\n")
    (Path(workdir) / ".env").write_text("IGNORED_MARKER=present\n")
    claude_config = Path(home) / "claude-config"
    codex_config = Path(home) / "codex-config"
    (claude_config / "skills" / "claude-demo").mkdir(parents=True)
    (claude_config / "skills" / "claude-demo" / "SKILL.md").write_text(
        "---\nname: claude-demo\n---\n"
    )
    (codex_config / "skills" / "codex-demo").mkdir(parents=True)
    (codex_config / "skills" / "codex-demo" / "SKILL.md").write_text(
        "---\nname: codex-demo\n---\n"
    )
    (Path(home) / "roles.json").write_text(
        json.dumps(
            {
                "roles": {
                    role: {
                        "provider": "codex",
                        "model": "rits/deepseek-v4.1-flash",
                    }
                    for role in ["orchestrator", "planner", "implementor", "reviewer"]
                }
            }
        )
        + "\n"
    )
    (Path(home) / "config.json").write_text(
        json.dumps(
            {
                "defaultProvider": "claude",
                "codex": {"apiKeyEnv": "CODEX_ICA_API_KEY"},
                "grok": {"mode": "subscription", "apiKeyEnv": "XAI_API_KEY"},
            }
        )
        + "\n"
    )
    (claude_config / "CLAUDE.md").write_text("# user memory\n")
    (claude_config / "rules").mkdir(parents=True)
    (claude_config / "rules" / "one.md").write_text("# user rule\n")
    (Path(workdir) / "CLAUDE.md").write_text("# project memory\n")
    (Path(workdir) / "AGENTS.md").write_text("# shared agents memory\n")
    (codex_config / "AGENTS.md").write_text("# codex memory\n")
    auto_memory = claude_config / "projects" / "probe-project" / "memory"
    auto_memory.mkdir(parents=True)
    (auto_memory / "MEMORY.md").write_text("# Memory\n\n- probe index\n")
    (auto_memory / "topic.md").write_text("# Topic\n\n- probe topic\n")
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
        env["BATON_GH_BIN"] = str(gh)
        env["PATH"] = f"{editor_bin}{os.pathsep}{env.get('PATH', '')}"
        env["CLAUDE_CONFIG_DIR"] = str(claude_config)
        env["CODEX_HOME"] = str(codex_config)
        env["CLAUDE_CODE_PROJECT_DIR_NAME"] = "probe-project"
        env["CODEX_ICA_API_KEY"] = "baton-probe-key"
        env.pop("XAI_API_KEY", None)
        for name in (
            "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
            "CLAUDE_CODE_REMOTE",
            "CLAUDE_CODE_REMOTE_MEMORY_DIR",
            "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE",
        ):
            env.pop(name, None)
        env.pop("VISUAL", None)
        os.execvpe(str(binary), [str(binary)], env)

    transcript = bytearray()
    missing = []
    rejected = []
    exit_code = None
    try:
        set_size(master, args.width, args.height)
        if not wait_for_screen(master, "Ask anything", args.timeout, transcript, args.width, args.height):
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
        for value in ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-4-5"]:
            if value not in model_picker:
                missing.append(f"model picker known catalog: {value}")
        model_header = re.search(
            r"[a-z-]+ models · (\d+)/(\d+) · type to filter · enter · esc",
            model_picker,
        )
        if model_header is None or int(model_header.group(2)) < 2:
            missing.append("model picker count is at least 2")
        # Count option rows by the model names they show, not by the cursor
        # glyph. Counting cursors accepted a picker that painted one on every
        # row, which is exactly the defect this has to catch.
        catalog = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-4-5"]
        option_rows = [name for name in catalog if name in model_picker]
        if len(option_rows) < 2:
            missing.append("model picker has at least 2 option rows")
        if model_picker.count("❯") != 1:
            missing.append("model picker must paint one cursor")
        (evidence / "model-picker-screen.txt").write_text(model_picker + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/skills\r")
        pump(master, 0.8, transcript)
        skills_picker = screen_text(transcript, args.width, args.height)
        skills_required = ["skills (2)", "enter insert", "claude-demo (claude)"]
        missing.extend(value for value in skills_required if value not in skills_picker)
        if "codex-demo (codex)" not in skills_picker:
            missing.append("codex skill row")
        (evidence / "skills-picker-screen.txt").write_text(skills_picker + "\n")
        os.write(master, b"\x1b[B")
        pump(master, 0.2, transcript)
        os.write(master, b"\r")
        pump(master, 0.8, transcript)
        skills_result = screen_text(transcript, args.width, args.height)
        if "inserted $codex-demo (codex)" not in skills_result:
            missing.append("codex skill insertion notice")
        if "switch to codex before sending" not in skills_result:
            missing.append("skills source warning")
        if "/orchestrate off before sending" not in skills_result:
            missing.append("skills orchestrate warning")
        if not any(
            line.strip().startswith("│$codex-demo ")
            for line in skills_result.splitlines()
        ):
            missing.append("codex skill in composer")
        (evidence / "skills-result-screen.txt").write_text(skills_result + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/roles\r")
        pump(master, 0.8, transcript)
        roles_picker = screen_text(transcript, args.width, args.height)
        roles_required = [
            "roles (4)",
            "orchestrator · codex/rits/deepseek-v4.1-flash · config user",
            "planner · codex/rits/deepseek-v4.1-flash",
            "implementor · codex/rits/deepseek-v4.1-flash",
            "reviewer · codex/rits/deepseek-v4.1-flash",
            "enter details",
            "esc close",
        ]
        missing.extend(value for value in roles_required if value not in roles_picker)
        if "config:" in roles_picker or "prompt:" in roles_picker:
            missing.append("roles detail fields leaked into picker rows")
        (evidence / "roles-picker-screen.txt").write_text(roles_picker + "\n")
        os.write(master, b"\x1b[B")
        pump(master, 0.2, transcript)
        os.write(master, b"\x1b[B")
        pump(master, 0.2, transcript)
        os.write(master, b"\r")
        pump(master, 0.5, transcript)
        roles_detail = screen_text(transcript, args.width, args.height)
        for value in [
            "implementor · codex/rits/deepseek-v4.1-flash",
            "config:",
            "prompt:",
        ]:
            if value not in roles_detail:
                missing.append(f"roles detail: {value}")
        (evidence / "roles-detail-screen.txt").write_text(roles_detail + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/auth antigravity ")
        pump(master, 0.8, transcript)
        auth_completion = screen_text(transcript, args.width, args.height)
        for value in [
            "/auth antigravity subscription",
            "/auth antigravity api",
            "antigravity billing",
        ]:
            if value not in auth_completion:
                missing.append(f"auth completion: {value}")
        (evidence / "auth-completion-screen.txt").write_text(auth_completion + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/auth\r")
        pump(master, 0.8, transcript)
        auth_picker = screen_text(transcript, args.width, args.height)
        for value in [
            "billing mode · 1/12",
            "claude subscription",
            "codex api · key CODEX_ICA_API_KEY",
            "antigravity api",
            "enter apply",
            "esc close",
        ]:
            if value not in auth_picker:
                missing.append(f"auth picker: {value}")
        (evidence / "auth-picker-screen.txt").write_text(auth_picker + "\n")
        os.write(master, b"\x1b[A")
        pump(master, 0.3, transcript)
        auth_last = screen_text(transcript, args.width, args.height)
        for value in ["billing mode · 12/12", "antigravity api"]:
            if value not in auth_last:
                missing.append(f"auth picker scroll: {value}")
        (evidence / "auth-last-row-screen.txt").write_text(auth_last + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/auth \r")
        if not wait_for_screen(
            master,
            "billing mode · 1/12",
            3.0,
            transcript,
            args.width,
            args.height,
        ):
            missing.append("auth trailing-space Enter opens the picker")
        (evidence / "auth-trailing-space-screen.txt").write_text(
            screen_text(transcript, args.width, args.height) + "\n"
        )
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/model \r")
        if not wait_for_screen(
            master,
            "models ·",
            3.0,
            transcript,
            args.width,
            args.height,
        ):
            missing.append("model trailing-space Enter opens the picker")
        (evidence / "model-trailing-space-screen.txt").write_text(
            screen_text(transcript, args.width, args.height) + "\n"
        )
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/auth codex api\r")
        pump(master, 0.6, transcript)
        auth_set = screen_text(transcript, args.width, args.height)
        if "codex billing set to api" not in auth_set:
            missing.append("auth codex api notice")
        config_path = Path(home) / "config.json"
        config = json.loads(config_path.read_text())
        if config.get("codex", {}).get("mode") != "api":
            missing.append("auth persisted codex api mode")
        (evidence / "auth-set-screen.txt").write_text(auth_set + "\n")

        os.write(master, b"/auth codex\r")
        pump(master, 0.6, transcript)
        auth_query = screen_text(transcript, args.width, args.height)
        for value in ["codex billing", "● api", "subscription"]:
            if value not in auth_query:
                missing.append(f"auth scoped picker: {value}")
        queried = json.loads(config_path.read_text())
        if queried.get("codex", {}).get("mode") != "api":
            missing.append("auth query mutated the codex billing mode")
        (evidence / "auth-query-screen.txt").write_text(auth_query + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/auth\r")
        pump(master, 0.6, transcript)
        for _ in range(7):
            os.write(master, b"\x1b[B")
            pump(master, 0.1, transcript)
        auth_grok = screen_text(transcript, args.width, args.height)
        for value in ["billing mode · 8/12", "grok api · missing XAI_API_KEY"]:
            if value not in auth_grok:
                missing.append(f"auth missing key row: {value}")
        (evidence / "auth-grok-row-screen.txt").write_text(auth_grok + "\n")
        os.write(master, b"\r")
        pump(master, 0.5, transcript)
        auth_missing = screen_text(transcript, args.width, args.height)
        for value in ["no API key for grok", "billing mode · 8/12"]:
            if value not in auth_missing:
                missing.append(f"auth missing key result: {value}")
        (evidence / "auth-missing-key-screen.txt").write_text(auth_missing + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/auth codex subscription\r")
        pump(master, 0.6, transcript)
        if "codex billing set to subscription" not in screen_text(
            transcript, args.width, args.height
        ):
            missing.append("auth codex subscription notice")

        os.write(master, b"/login\r")
        pump(master, 0.8, transcript)
        login_picker = screen_text(transcript, args.width, args.height)
        for value in [
            "sign in ·",
            "claude · run claude auth login",
            "codex · run codex login",
            "antigravity · no CLI login",
            "enter run",
        ]:
            if value not in login_picker:
                missing.append(f"login picker: {value}")
        if login_picker.count("❯") != 1:
            missing.append("login picker must paint one cursor")
        (evidence / "login-picker-screen.txt").write_text(login_picker + "\n")
        os.write(master, b"\x1b[B")
        pump(master, 0.2, transcript)
        os.write(master, b"\r")
        pump(master, 1.2, transcript)
        login_result = screen_text(transcript, args.width, args.height)
        if "codex login finished" not in login_result:
            missing.append("login completion notice")
        raw = bytes(transcript).decode("utf-8", "replace")
        if "fake-codex-login login" not in raw:
            missing.append("login ran the harness command with inherited stdio")
        (evidence / "login-result-screen.txt").write_text(login_result + "\n")

        os.write(master, b"/settings\r")
        pump(master, 0.8, transcript)
        settings_picker = screen_text(transcript, args.width, args.height)
        settings_required = [
            "claude + global",
            "default session mode: orchestrate",
            "notifications bell: off",
            "options: [orchestrate] · single",
            "permission mode · read-only",
            "enter apply",
            "esc",
        ]
        missing.extend(
            value for value in settings_required if value not in settings_picker
        )
        (evidence / "settings-picker-screen.txt").write_text(settings_picker + "\n")

        os.write(master, b"\x1b[C")
        pump(master, 0.2, transcript)
        os.write(master, b"\r")
        pump(master, 0.5, transcript)
        settings_mode = screen_text(transcript, args.width, args.height)
        if "default session mode set to single" not in settings_mode:
            missing.append("settings default mode notice")

        os.write(master, b"\x1b[B")
        pump(master, 0.2, transcript)
        settings_bell = screen_text(transcript, args.width, args.height)
        if "options: [off] · on" not in settings_bell:
            missing.append("settings bell option row")
        os.write(master, b"\x1b[C")
        pump(master, 0.2, transcript)
        os.write(master, b"\r")
        pump(master, 0.5, transcript)
        settings_result = screen_text(transcript, args.width, args.height)
        if "notifications bell on" not in settings_result:
            missing.append("settings bell notice")
        (evidence / "settings-result-screen.txt").write_text(settings_result + "\n")

        os.write(master, b"\x1b[B")
        pump(master, 0.2, transcript)
        settings_harness = screen_text(transcript, args.width, args.height)
        if "options: [default] · acceptEdits · bypassPermissions · plan" not in settings_harness:
            missing.append("settings harness option row")
        if "permission mode · read-only" not in settings_harness:
            missing.append("settings harness read-only row")
        (evidence / "settings-harness-options-screen.txt").write_text(
            settings_harness + "\n"
        )
        config_path = Path(home) / "config.json"
        if not config_path.is_file():
            missing.append("settings config file")
        else:
            config = json.loads(config_path.read_text())
            (evidence / "settings-config.json").write_text(
                json.dumps(config, indent=2) + "\n"
            )
            if config.get("defaultMode") != "single":
                missing.append("settings persisted default mode")
            if config.get("notifications", {}).get("bell") is not True:
                missing.append("settings persisted bell")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        if editor_capture.exists():
            editor_capture.unlink()
        os.write(master, b"/memory\r")
        pump(master, 0.8, transcript)
        memory_picker = screen_text(transcript, args.width, args.height)
        memory_required = [
            "Memory ·",
            "Claude Code",
            "User instructions",
            "one.md",
            "Project instructions",
            "Local instructions",
            "Codex",
            "Auto memory",
            "Auto-memory",
            "Auto-memory index",
            "topic.md",
            "Open auto-memory folder",
            "←→ toggle",
            "enter open",
            "esc close",
        ]
        missing.extend(value for value in memory_required if value not in memory_picker)
        (evidence / "memory-picker-screen.txt").write_text(memory_picker + "\n")
        if not select_memory_row(
            master, transcript, args.width, args.height, "Auto-memory"
        ):
            missing.append("auto-memory row selection")
        os.write(master, b"\r")
        pump(master, 0.5, transcript)
        memory_toggle = screen_text(transcript, args.width, args.height)
        if "auto-memory user setting off" not in memory_toggle:
            missing.append("auto-memory toggle notice")
        claude_settings = claude_config / "settings.json"
        if not claude_settings.is_file():
            missing.append("Claude settings file")
        else:
            settings = json.loads(claude_settings.read_text())
            (evidence / "claude-memory-settings.json").write_text(
                json.dumps(settings, indent=2) + "\n"
            )
            if settings.get("autoMemoryEnabled") is not False:
                missing.append("auto-memory persisted setting")
        (evidence / "memory-toggle-screen.txt").write_text(memory_toggle + "\n")
        if not select_memory_row(
            master, transcript, args.width, args.height, "Project instructions"
        ):
            missing.append("project instructions row selection")
        os.write(master, b"\r")
        if not wait_for_screen(master, "opened ", 5.0, transcript, args.width, args.height):
            missing.append("memory editor opened notice")
        pump(master, 0.5, transcript)
        memory_open = screen_text(transcript, args.width, args.height)
        (evidence / "memory-open-screen.txt").write_text(memory_open + "\n")
        if not editor_capture.is_file():
            missing.append("memory editor process")
        else:
            editor_args = editor_capture.read_text().splitlines()
            (evidence / "memory-open-args.txt").write_text(
                json.dumps(editor_args, indent=2) + "\n"
            )
            if not editor_args or editor_args[-1] != str(
                (Path(workdir).resolve() / "CLAUDE.md")
            ):
                missing.append("memory editor target")

        os.write(master, b"/git\r")
        if not wait_for_screen(master, "Git ·", 10.0, transcript, args.width, args.height):
            missing.append("git picker")
        pump(master, 0.2, transcript)
        git_picker = screen_text(transcript, args.width, args.height)
        git_required = [
            "Git ·",
            "Status",
            "Commit all changes",
            "Commit staged changes",
            "Push current branch",
            "Checks",
            "Cancel",
        ]
        missing.extend(value for value in git_required if value not in git_picker)
        (evidence / "git-picker-screen.txt").write_text(git_picker + "\n")
        os.write(master, b"\x1b[B")
        if not wait_for_screen(master, "❯ Commit all changes", 5.0, transcript, args.width, args.height):
            missing.append("git commit row selection")
        os.write(master, b"\r")
        if not wait_for_screen(master, "message: baton:", 10.0, transcript, args.width, args.height):
            missing.append("git confirm screen")
        pump(master, 0.2, transcript)
        git_confirm = screen_text(transcript, args.width, args.height)
        git_confirm_required = [
            "Commit ·",
            "message: baton:",
            "enter confirm",
            "src/parser/mod.rs",
            "README.md",
        ]
        missing.extend(
            value for value in git_confirm_required if value not in git_confirm
        )
        (evidence / "git-confirm-screen.txt").write_text(git_confirm + "\n")
        os.write(master, b"\r")
        if not wait_for_screen(master, "committed ", 20.0, transcript, args.width, args.height):
            missing.append("git commit notice")
        pump(master, 0.4, transcript)
        (evidence / "git-committed-screen.txt").write_text(
            screen_text(transcript, args.width, args.height) + "\n"
        )
        committed = subprocess.run(
            ["git", "-C", workdir, "log", "-1", "--format=%s"],
            capture_output=True,
            text=True,
        )
        if committed.returncode != 0:
            missing.append("git commit missing")
        else:
            committed_subject = committed.stdout.strip()
            (evidence / "git-log-subject.txt").write_text(committed_subject + "\n")
            if committed_subject != "baton: update 4 files":
                missing.append("git generated commit message")
            committed_names = subprocess.run(
                ["git", "-C", workdir, "show", "--name-only", "--format=", "HEAD"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout
            (evidence / "git-committed-files.txt").write_text(committed_names)
            expected_names = sorted(
                ["AGENTS.md", "CLAUDE.md", "README.md", "src/parser/mod.rs"]
            )
            if sorted(committed_names.splitlines()) != expected_names:
                missing.append("git committed file set")
            if ".env" in committed_names.splitlines():
                missing.append("git committed an ignored file")

        os.write(master, b"/git push\r")
        if not wait_for_screen(master, "Git push", 10.0, transcript, args.width, args.height):
            missing.append("git push picker")
        pump(master, 0.2, transcript)
        push_picker = screen_text(transcript, args.width, args.height)
        push_required = ["Git push", "origin/main", "Cancel"]
        missing.extend(value for value in push_required if value not in push_picker)
        if not any("❯ origin/main" in line for line in push_picker.splitlines()):
            missing.append("git push target selection")
        (evidence / "git-push-picker-screen.txt").write_text(push_picker + "\n")
        os.write(master, b"\r")
        if not wait_for_screen(master, "Push ·", 10.0, transcript, args.width, args.height):
            missing.append("git push confirm")
        pump(master, 0.2, transcript)
        push_confirm = screen_text(transcript, args.width, args.height)
        push_confirm_required = ["Push ·", "branch main -> origin/main", "never force"]
        missing.extend(
            value for value in push_confirm_required if value not in push_confirm
        )
        (evidence / "git-push-confirm-screen.txt").write_text(push_confirm + "\n")
        os.write(master, b"\r")
        if not wait_for_screen(master, "pushed main to origin/main", 20.0, transcript, args.width, args.height):
            missing.append("git push notice")
        pump(master, 0.3, transcript)
        (evidence / "git-pushed-screen.txt").write_text(
            screen_text(transcript, args.width, args.height) + "\n"
        )
        local_head = subprocess.run(
            ["git", "-C", workdir, "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
        )
        remote_head = subprocess.run(
            ["git", "-C", str(remote_path), "rev-parse", "refs/heads/main"],
            capture_output=True,
            text=True,
        )
        if local_head.returncode != 0 or remote_head.returncode != 0:
            missing.append("git push remote missing")
        elif local_head.stdout.strip() != remote_head.stdout.strip():
            missing.append("git push remote head")

        os.write(master, b"/git\r")
        if not wait_for_screen(master, "Git ·", 10.0, transcript, args.width, args.height):
            missing.append("git checks picker")
        pump(master, 0.2, transcript)
        for _ in range(4):
            os.write(master, b"\x1b[B")
            pump(master, 0.15, transcript)
        os.write(master, b"\r")
        pump(master, 1.0, transcript)
        git_checks = screen_text(transcript, args.width, args.height)
        if "Git checks · #42" not in git_checks:
            missing.append("git checks screen")
        for value in ["Add checks", "Test · SUCCESS", "Lint · PENDING"]:
            if value not in git_checks:
                missing.append(f"git checks row: {value}")
        (evidence / "git-checks-screen.txt").write_text(git_checks + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/git checks\r")
        if not wait_for_screen(
            master,
            "Git checks · #42",
            10.0,
            transcript,
            args.width,
            args.height,
        ):
            missing.append("direct /git checks command")
        direct_checks = screen_text(transcript, args.width, args.height)
        for value in ["Add checks", "Test · SUCCESS"]:
            if value not in direct_checks:
                missing.append(f"direct /git checks row: {value}")
        (evidence / "git-checks-direct-screen.txt").write_text(direct_checks + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/git status\r")
        if not wait_for_screen(
            master,
            "Git status ·",
            10.0,
            transcript,
            args.width,
            args.height,
        ):
            missing.append("direct /git status command")
        git_status = screen_text(transcript, args.width, args.height)
        if "esc close" not in git_status:
            missing.append("direct /git status close hint")
        (evidence / "git-status-direct-screen.txt").write_text(git_status + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        subprocess.run(
            ["git", "-C", workdir, "checkout", "-q", "-b", "feat/pr"], check=True
        )
        (Path(workdir) / "feature.txt").write_text("feature\n")
        subprocess.run(
            ["git", "-C", workdir, "add", "feature.txt"], check=True
        )
        subprocess.run(
            ["git", "-C", workdir, "commit", "-q", "-m", "Add pull request"],
            check=True,
        )
        subprocess.run(
            ["git", "-C", workdir, "push", "-q", "origin", "feat/pr"], check=True
        )
        os.write(master, b"/git pr\r")
        if not wait_for_screen(master, "Pull request ·", 10.0, transcript, args.width, args.height):
            missing.append("git pull request confirmation")
        pump(master, 0.2, transcript)
        git_pr = screen_text(transcript, args.width, args.height)
        for value in [
            "Pull request · example/repo · feat/pr → origin/main",
            "title: Add pull request",
            "- Add pull request",
            "enter create",
        ]:
            if value not in git_pr:
                missing.append(f"git pull request row: {value}")
        (evidence / "git-pr-confirm-screen.txt").write_text(git_pr + "\n")
        os.write(master, b"\r")
        if not wait_for_screen(
            master,
            "opened pull request #43",
            30.0,
            transcript,
            args.width,
            args.height,
        ):
            missing.append("git pull request notice")
        pump(master, 0.3, transcript)
        git_pr_result = screen_text(transcript, args.width, args.height)
        if "https://github.com/example/repo/pull/43" not in git_pr_result:
            missing.append("git pull request URL")
        (evidence / "git-pr-result-screen.txt").write_text(git_pr_result + "\n")
        subprocess.run(["git", "-C", workdir, "checkout", "-q", "main"], check=True)

        os.write(master, b"/files ")
        pump(master, 0.5, transcript)
        files_hint = screen_text(transcript, args.width, args.height)
        if "files [--ignored] [query]" not in files_hint:
            missing.append("/files [--ignored] [query] hint")
        (evidence / "files-hint-screen.txt").write_text(files_hint + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/files --")
        pump(master, 0.5, transcript)
        files_flags = screen_text(transcript, args.width, args.height)
        if "/files --ignored" not in files_flags:
            missing.append("/files --ignored completion")
        (evidence / "files-flags-screen.txt").write_text(files_flags + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/files --ignored env\r")
        pump(master, 1.5, transcript)
        ignored_picker = screen_text(transcript, args.width, args.height)
        ignored_required = [
            'files --ignored matching "env"',
            "ignored files included",
        ]
        missing.extend(value for value in ignored_required if value not in ignored_picker)
        if not any("❯ .env" in line for line in ignored_picker.splitlines()):
            missing.append("ignored .env row")
        (evidence / "files-ignored-screen.txt").write_text(ignored_picker + "\n")
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
        if not wait_for_screen(master, "opened src/parser/mod.rs:12", 3.0, transcript, args.width, args.height):
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
        if not wait_for_screen(master, "editor failed:", 3.0, transcript, args.width, args.height):
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
        for value in ["files · 4 · enter insert", "README.md", "❯ src/parser/mod.rs"]:
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
        if "grep [--case|--case-sensitive] [--word] [--regex] <query>" not in grep_hint:
            missing.append("/grep usage hint")
        (evidence / "grep-hint-screen.txt").write_text(grep_hint + "\n")
        os.write(master, b"\x1b")
        pump(master, 0.2, transcript)

        os.write(master, b"/grep --")
        pump(master, 0.5, transcript)
        grep_flags = screen_text(transcript, args.width, args.height)
        for flag in [
            "/grep --case",
            "/grep --case-sensitive",
            "/grep --word",
            "/grep --regex",
        ]:
            if flag not in grep_flags:
                missing.append(f"{flag} completion")
        (evidence / "grep-flags-screen.txt").write_text(grep_flags + "\n")
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
        if not wait_for_screen(master, "opened src/parser/mod.rs:1", 3.0, transcript, args.width, args.height):
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
            "binary": binary_identity(binary),
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
