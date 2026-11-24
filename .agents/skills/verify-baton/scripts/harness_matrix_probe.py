#!/usr/bin/env python3
"""Drive every advertised harness and record a live pass or explicit block."""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

if sys.version_info < (3, 10):
    raise SystemExit(
        "Python 3.10 or newer is required; run with `uv run --no-project python`."
    )


PROMPTS = {
    "codex": "Reply with exactly CODEX_OK",
    "claude": "Reply with exactly CLAUDE_OK",
    "grok": "Reply with exactly GROK_OK",
    "opencode": "Reply with exactly OPENCODE_OK",
    "antigravity": "Reply with exactly AGY_OK",
}


def terminate_tree(process: subprocess.Popen) -> None:
    if os.name != "nt":
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except OSError:
            process.terminate()
    else:
        process.terminate()
    try:
        process.wait(timeout=2)
        return
    except subprocess.TimeoutExpired:
        pass
    if os.name != "nt":
        try:
            os.killpg(process.pid, signal.SIGKILL)
            return
        except OSError:
            pass
    process.kill()


def run_command(
    args: list[str],
    *,
    env: dict[str, str] | None = None,
    timeout: int,
    capture_output: bool = True,
    text: bool = True,
) -> subprocess.CompletedProcess:
    process = subprocess.Popen(
        args,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=text,
        start_new_session=os.name != "nt",
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout)
        return subprocess.CompletedProcess(args, process.returncode, stdout, stderr)
    except subprocess.TimeoutExpired:
        terminate_tree(process)
        try:
            stdout, stderr = process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            stdout, stderr = "", ""
        note = f"command timed out after {timeout}s"
        stderr = f"{stderr}\n{note}" if stderr else note
        return subprocess.CompletedProcess(args, 124, stdout, stderr)


def command_version(command: str) -> str:
    try:
        run = run_command(
            [command, "--version"],
            capture_output=True,
            text=True,
            timeout=30,
        )
        return (run.stdout or run.stderr).strip()
    except (OSError, subprocess.TimeoutExpired) as error:
        return f"unavailable: {error}"


def bounded(text: str, limit: int = 1000) -> str:
    return text[-limit:]


def redact(text: str, secret: str | None) -> str:
    if secret:
        return text.replace(secret, "[redacted]")
    return text


def row(
    provider: str,
    mode: str,
    version: str,
    status: str,
    *,
    turn_exit_code: int | None = None,
    seconds: float | None = None,
    text: str | None = None,
    error: str | None = None,
    processes: list | None = None,
    reason: str | None = None,
    model: str | None = None,
    endpoint: str | None = None,
    detail: str | None = None,
) -> dict:
    return {
        "provider": provider,
        "mode": mode,
        "version": version,
        "model": model,
        "endpoint": endpoint,
        "turn_exit_code": turn_exit_code,
        "seconds": seconds,
        "text": text,
        "error": error,
        "processes": processes,
        "status": status,
        "reason": reason,
        "detail": detail,
    }


def baton_turn(
    binary: str,
    provider: str,
    cwd: Path,
    env: dict[str, str],
    secret: str | None,
    model: str | None = None,
    timeout: int = 180,
) -> dict:
    args = [binary, "-C", str(cwd), "exec", "-p", provider, "--json"]
    if model:
        args.extend(["--model", model])
    args.append(PROMPTS[provider])
    started = time.time()
    run = run_command(
        args,
        env=env,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    try:
        payload = json.loads(run.stdout) if run.stdout.strip() else {}
    except json.JSONDecodeError:
        payload = {}
    if not isinstance(payload, dict):
        payload = {}
    stderr = redact(bounded(run.stderr), secret)
    text = payload.get("text")
    error = payload.get("error") or (stderr if run.returncode != 0 else None)
    if text is not None:
        text = bounded(redact(str(text), secret), 2000)
    if error is not None:
        error = bounded(redact(str(error), secret))
    return {
        "turn_exit_code": run.returncode,
        "seconds": round(time.time() - started, 1),
        "text": text,
        "error": error,
    }


def check_processes(binary: str, env: dict[str, str]) -> list:
    run = run_command(
        [binary, "ps", "--json"],
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    if run.returncode != 0:
        return ["unparseable"]
    try:
        payload = json.loads(run.stdout)
        if not isinstance(payload, dict):
            return ["unparseable"]
        processes = payload.get("processes")
        return processes if isinstance(processes, list) else ["unparseable"]
    except json.JSONDecodeError:
        return ["unparseable"]


def expected_turn(turn: dict, expected: str, processes: list) -> bool:
    return (
        turn["turn_exit_code"] == 0
        and turn["text"] == expected
        and turn["error"] is None
        and processes == []
    )


def rate_limited(*values) -> bool:
    text = " ".join(str(value) for value in values if value).lower()
    return any(
        marker in text
        for marker in (
            "ratelimiterror",
            "rate limit",
            "quota exceeded",
            "too many requests",
            "error 429",
            "code\": 429",
        )
    )


def write_row(evidence: Path, provider: str, value: dict) -> dict:
    (evidence / f"{provider}.json").write_text(json.dumps(value, indent=2) + "\n")
    return value


def secret_leaks(evidence: Path, secret: str) -> list[str]:
    leaks = []
    for path in evidence.rglob("*"):
        if not path.is_file():
            continue
        try:
            if secret in path.read_text(errors="ignore"):
                leaks.append(str(path.relative_to(evidence)))
        except OSError:
            continue
    return leaks


def scrub_evidence(evidence: Path, secret: str) -> None:
    for path in evidence.rglob("*"):
        if not path.is_file():
            continue
        try:
            text = path.read_text(errors="ignore")
        except OSError:
            continue
        if secret in text:
            path.write_text(text.replace(secret, "[redacted]"))


def run_codex(
    binary: str,
    key_env: str,
    base_url: str,
    model: str,
    evidence: Path,
    secret: str,
) -> dict:
    endpoint = f"{base_url.rstrip('/')}/v1"
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True, prefix="baton-codex-matrix-") as root:
        root_path = Path(root)
        home = root_path / "home"
        cwd = root_path / "cwd"
        codex_home = root_path / "codex"
        home.mkdir()
        cwd.mkdir()
        codex_home.mkdir()
        (codex_home / "config.toml").write_text(
            "\n".join(
                [
                    f'model = "{model}"',
                    'model_provider = "ica"',
                    'approval_policy = "never"',
                    'sandbox_mode = "read-only"',
                    "check_for_update_on_startup = false",
                    "",
                    "[model_providers.ica]",
                    'name = "ICA"',
                    'wire_api = "responses"',
                    f'base_url = "{endpoint}"',
                    'query_params = { api-version = "2025-03-01-preview" }',
                    f'env_key = "{key_env}"',
                    "request_max_retries = 1",
                    "stream_max_retries = 1",
                    "stream_idle_timeout_ms = 300000",
                    "",
                ]
            )
        )
        env = os.environ.copy()
        env.update({"BATON_HOME": str(home), "CODEX_HOME": str(codex_home)})
        turn = baton_turn(binary, "codex", cwd, env, secret, timeout=180)
        processes = check_processes(binary, env)
        status = "passed" if expected_turn(turn, "CODEX_OK", processes) else "failed"
        if status == "failed" and rate_limited(turn["text"], turn["error"]):
            status = "blocked"
        return write_row(
            evidence,
            "codex",
            row(
                "codex",
                "ica-responses",
                command_version("codex"),
                status,
                reason="provider rate limit exceeded" if status == "blocked" else None,
                model=model,
                endpoint=endpoint,
                processes=processes,
                **turn,
            ),
        )


def run_claude(binary: str, evidence: Path, secret: str, include_live: bool) -> dict:
    claude_env = os.environ.copy()
    for name in (
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
    ):
        claude_env.pop(name, None)
    status_run = run_command(
        ["claude", "auth", "status"],
        env=claude_env,
        capture_output=True,
        text=True,
        timeout=30,
    )
    version = command_version("claude")
    raw_status = redact(bounded(status_run.stdout), secret)
    try:
        auth = json.loads(status_run.stdout)
    except json.JSONDecodeError:
        return write_row(
            evidence,
            "claude",
            row(
                "claude",
                "native-subscription",
                version,
                "failed",
                error=raw_status or redact(bounded(status_run.stderr), secret),
                reason="claude auth status did not return JSON",
            ),
        )
    if status_run.returncode != 0 or not isinstance(auth.get("loggedIn"), bool):
        return write_row(
            evidence,
            "claude",
            row(
                "claude",
                "native-subscription",
                version,
                "failed",
                error=raw_status,
                reason="claude auth status failed or had an unexpected shape",
            ),
        )
    if not auth["loggedIn"]:
        return write_row(
            evidence,
            "claude",
            row(
                "claude",
                "native-subscription",
                version,
                "blocked",
                detail=str(auth.get("authMethod") or ""),
                reason="claude auth login required",
            ),
        )
    if not include_live:
        return write_row(
            evidence,
            "claude",
            row(
                "claude",
                "native-subscription",
                version,
                "skipped",
                detail=str(auth.get("authMethod") or ""),
                reason="live Claude turn requires --include-claude",
            ),
        )
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True, prefix="baton-claude-matrix-") as root:
        root_path = Path(root)
        home = root_path / "home"
        cwd = root_path / "cwd"
        home.mkdir()
        cwd.mkdir()
        env = claude_env.copy()
        env["BATON_HOME"] = str(home)
        claude_model = None
        doctor = run_command(
            [binary, "-C", str(cwd), "doctor", "--json"],
            env=env,
            capture_output=True,
            text=True,
            timeout=60,
        )
        try:
            doctor_payload = json.loads(doctor.stdout)
            for provider in doctor_payload.get("providers", []):
                if provider.get("provider") == "claude":
                    claude_model = provider.get("model")
                    break
        except (json.JSONDecodeError, AttributeError):
            pass
        turn = baton_turn(binary, "claude", cwd, env, secret, timeout=180)
        processes = check_processes(binary, env)
        error = str(turn["error"] or "")
        if expected_turn(turn, "CLAUDE_OK", processes):
            status = "passed"
            reason = None
        elif (
            "credit balance" in error.lower()
            or "usage limit" in error.lower()
            or rate_limited(turn["text"], turn["error"])
        ):
            status = "blocked"
            reason = error.splitlines()[0] if error else "provider rate limit exceeded"
        else:
            status = "failed"
            reason = None
        return write_row(
            evidence,
            "claude",
            row(
                "claude",
                "native-subscription",
                version,
                status,
                processes=processes,
                model=claude_model,
                endpoint="native-subscription",
                reason=reason,
                detail=str(auth.get("authMethod") or ""),
                **turn,
            ),
        )


def run_focused_probe(
    provider: str,
    script: Path,
    binary: str,
    key_env: str,
    base_url: str,
    model: str,
    evidence: Path,
    secret: str,
) -> dict:
    started = time.time()
    run = run_command(
        [
            sys.executable,
            str(script),
            "--binary",
            binary,
            "--key-env",
            key_env,
            "--base-url",
            base_url,
            "--model",
            model,
            "--evidence-dir",
            str(evidence),
        ],
        capture_output=True,
        text=True,
        timeout=240,
    )
    seconds = round(time.time() - started, 1)
    try:
        payload = json.loads(run.stdout)
    except json.JSONDecodeError:
        payload = {}
    if not isinstance(payload, dict):
        payload = {}
    status = "passed" if run.returncode == 0 and payload.get("passed") is True else "failed"
    if status == "failed" and rate_limited(
        payload.get("text"), payload.get("error"), run.stderr
    ):
        status = "blocked"
    version = payload.get(
        f"{provider}_version", command_version("agy" if provider == "antigravity" else provider)
    )
    return write_row(
        evidence,
        provider,
        row(
            provider,
            "ica-openai-compatible" if provider == "grok" else "ica-acp",
            version,
            status,
            reason="provider rate limit exceeded" if status == "blocked" else None,
            turn_exit_code=payload.get("exit_code"),
            seconds=payload.get("seconds", seconds),
            text=bounded(redact(str(payload.get("text")), secret), 2000)
            if payload.get("text") is not None
            else None,
            error=bounded(redact(str(payload.get("error")), secret))
            if payload.get("error") is not None
            else redact(bounded(run.stderr), secret) or None,
            processes=payload.get("processes"),
            model=model,
            endpoint=base_url,
        ),
    )


def run_opencode(
    binary: str,
    key_env: str,
    base_url: str,
    model: str,
    evidence: Path,
    secret: str,
) -> dict:
    endpoint = f"{base_url.rstrip('/')}/v1"
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True, prefix="baton-opencode-matrix-") as root:
        root_path = Path(root)
        home = root_path / "home"
        cwd = root_path / "cwd"
        config = root_path / "config" / "opencode"
        data = root_path / "data"
        state = root_path / "state"
        cache = root_path / "cache"
        for path in (home, cwd, config, data, state, cache):
            path.mkdir(parents=True)
        (config / "opencode.json").write_text(
            json.dumps(
                {
                    "$schema": "https://opencode.ai/config.json",
                    "provider": {
                        "ica": {
                            "npm": "@ai-sdk/openai-compatible",
                            "name": "ICA",
                            "env": [key_env],
                            "options": {
                                "baseURL": endpoint,
                                "apiKey": f"{{env:{key_env}}}",
                            },
                            "models": {model: {"name": model}},
                        }
                    },
                    "model": f"ica/{model}",
                },
                indent=2,
            )
            + "\n"
        )
        env = os.environ.copy()
        env.update(
            {
                "BATON_HOME": str(home),
                "XDG_CONFIG_HOME": str(root_path / "config"),
                "XDG_DATA_HOME": str(data),
                "XDG_STATE_HOME": str(state),
                "XDG_CACHE_HOME": str(cache),
            }
        )
        turn = baton_turn(
            binary, "opencode", cwd, env, secret, model=f"ica/{model}", timeout=180
        )
        processes = check_processes(binary, env)
        status = "passed" if expected_turn(turn, "OPENCODE_OK", processes) else "failed"
        if status == "failed" and rate_limited(turn["text"], turn["error"]):
            status = "blocked"
        return write_row(
            evidence,
            "opencode",
            row(
                "opencode",
                "ica-openai-compatible-config",
                command_version("opencode"),
                status,
                reason="provider rate limit exceeded" if status == "blocked" else None,
                model=f"ica/{model}",
                endpoint=endpoint,
                processes=processes,
                **turn,
            ),
        )


def run_cursor(binary: str, key_env: str, base_url: str, evidence: Path, secret: str) -> dict:
    started = time.time()
    status_run = run_command(
        ["cursor-agent", "status"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    status_text = redact((status_run.stdout + status_run.stderr).strip(), secret)
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True, prefix="baton-cursor-matrix-") as root:
        root_path = Path(root)
        home = root_path / "home"
        cwd = root_path / "cwd"
        home.mkdir()
        cwd.mkdir()
        (home / "config.json").write_text(
            json.dumps(
                {
                    "cursor": {
                        "mode": "api",
                        "apiKeyEnv": key_env,
                        "apiBaseUrl": f"{base_url.rstrip('/')}/v1",
                    }
                },
                indent=2,
            )
            + "\n"
        )
        env = os.environ.copy()
        env["BATON_HOME"] = str(home)
        doctor = run_command(
            [binary, "-C", str(cwd), "doctor", "--json"],
            env=env,
            capture_output=True,
            text=True,
            timeout=60,
        )
    error_text = redact(doctor.stderr, secret)
    custom_rejected = (
        doctor.returncode != 0
        and "Cursor does not support a custom endpoint" in error_text
    )
    status_ok = status_run.returncode == 0
    lowered_status = status_text.lower()
    logged_out = (
        "not logged in" in lowered_status
        or "not authenticated" in lowered_status
        or "unauthenticated" in lowered_status
    )
    logged_in = status_ok and not logged_out and any(
        marker in lowered_status
        for marker in ("logged in as", "authenticated as")
    )
    if not custom_rejected:
        status = "failed"
        reason = "Baton did not reject the Cursor custom endpoint"
    elif logged_out:
        status = "blocked"
        reason = "cursor-agent login required; Baton rejects a custom endpoint"
    elif logged_in:
        status = "capability_verified"
        reason = "logged in; live Cursor turn was not requested"
    else:
        status = "failed"
        reason = "unrecognized cursor-agent status output"
    return write_row(
        evidence,
        "cursor",
        row(
            "cursor",
            "native-acp-capability",
            command_version("cursor-agent"),
            status,
            seconds=round(time.time() - started, 1),
            error=None if custom_rejected else redact(bounded(error_text), secret),
            endpoint=f"{base_url.rstrip('/')}/v1",
            processes=None,
            reason=reason,
            detail=bounded(status_text),
        ),
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/baton")
    parser.add_argument("--key-env", default="CODEX_ICA_API_KEY")
    parser.add_argument("--base-url", default="https://api.servicesessentials.ibm.com")
    parser.add_argument("--model", default="gemini-3.7-flash")
    parser.add_argument(
        "--codex-model", default="rits/deepseek-ai/DeepSeek-V4.1-Flash-1017"
    )
    parser.add_argument("--include-claude", action="store_true")
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()

    binary = str(Path(args.binary).resolve())
    if not Path(binary).is_file():
        print(json.dumps({"passed": False, "error": f"missing binary: {binary}"}))
        return 2
    secret = os.environ.get(args.key_env)
    if not secret:
        print(json.dumps({"passed": False, "error": f"{args.key_env} is not set"}))
        return 2
    script_dir = Path(__file__).resolve().parent
    evidence = (
        Path(args.evidence_dir)
        / f"harness-matrix-{int(time.time())}-{uuid.uuid4().hex[:8]}"
    )
    evidence.mkdir(parents=True, exist_ok=True)
    providers = [
        {
            "provider": "codex",
            "mode": "ica-responses",
            "version_command": "codex",
            "model": args.codex_model,
            "endpoint": args.base_url.rstrip("/") + "/v1",
            "runner": lambda: run_codex(
                binary, args.key_env, args.base_url, args.codex_model, evidence, secret
            ),
        },
        {
            "provider": "claude",
            "mode": "native-subscription",
            "version_command": "claude",
            "model": None,
            "endpoint": None,
            "runner": lambda: run_claude(binary, evidence, secret, args.include_claude),
        },
        {
            "provider": "grok",
            "mode": "ica-openai-compatible",
            "version_command": "grok",
            "model": args.model,
            "endpoint": args.base_url.rstrip("/") + "/v1",
            "runner": lambda: run_focused_probe(
                "grok",
                script_dir / "grok_probe.py",
                binary,
                args.key_env,
                args.base_url.rstrip("/") + "/v1",
                args.model,
                evidence,
                secret,
            ),
        },
        {
            "provider": "opencode",
            "mode": "ica-openai-compatible-config",
            "version_command": "opencode",
            "model": f"ica/{args.model}",
            "endpoint": args.base_url.rstrip("/") + "/v1",
            "runner": lambda: run_opencode(
                binary, args.key_env, args.base_url, args.model, evidence, secret
            ),
        },
        {
            "provider": "antigravity",
            "mode": "ica-acp",
            "version_command": "agy",
            "model": args.model,
            "endpoint": args.base_url.rstrip("/"),
            "runner": lambda: run_focused_probe(
                "antigravity",
                script_dir / "antigravity_probe.py",
                binary,
                args.key_env,
                args.base_url.rstrip("/"),
                args.model,
                evidence,
                secret,
            ),
        },
        {
            "provider": "cursor",
            "mode": "native-acp-capability",
            "version_command": "cursor-agent",
            "model": None,
            "endpoint": args.base_url.rstrip("/") + "/v1",
            "runner": lambda: run_cursor(binary, args.key_env, args.base_url, evidence, secret),
        },
    ]
    results = []
    for spec in providers:
        provider = spec["provider"]
        started = time.time()
        try:
            results.append(spec["runner"]())
        except Exception as error:
            results.append(
                write_row(
                    evidence,
                    provider,
                    row(
                        provider,
                        spec["mode"],
                        command_version(spec["version_command"]),
                        "failed",
                        seconds=round(time.time() - started, 1),
                        error=bounded(redact(str(error), secret)),
                        reason="provider probe raised an exception",
                        model=spec["model"],
                        endpoint=spec["endpoint"],
                        processes=None,
                    ),
                )
            )
    leaks = secret_leaks(evidence, secret)
    if leaks:
        results.append(
            write_row(
                evidence,
                "security",
                row(
                    "security",
                    "evidence-scan",
                    "matrix",
                    "failed",
                    error="configured key appeared in evidence",
                    reason="redaction failed",
                    detail=", ".join(leaks),
                ),
            )
        )
    scrub_evidence(evidence, secret)
    failed = [value for value in results if value["status"] == "failed"]
    summary = {
        "schema_version": 1,
        "effective": {
            "key_env": args.key_env,
            "base_url": args.base_url,
            "model": args.model,
            "codex_model": args.codex_model,
            "include_claude": args.include_claude,
        },
        "passed": not failed,
        "results": results,
        "evidence": str(evidence),
    }
    serialized = json.dumps(summary, indent=2).replace(secret, "[redacted]")
    (evidence / "summary.json").write_text(serialized + "\n")
    print(serialized)
    return 0 if not failed else 1


if __name__ == "__main__":
    raise SystemExit(main())
