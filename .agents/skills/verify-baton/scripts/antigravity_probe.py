#!/usr/bin/env python3
"""Live Antigravity ACP turn through baton with a custom Gemini endpoint.

Requires `agy` on PATH and a key in the source variable (default
CODEX_ICA_API_KEY). Uses a throwaway BATON_HOME and spends one small Gemini
turn. Prints one JSON summary and exits 0 only when the turn completes.
"""

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def run_baton(args, env, timeout):
    process = subprocess.Popen(
        args,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout)
        return subprocess.CompletedProcess(args, process.returncode, stdout, stderr)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            stdout, stderr = process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            stdout, stderr = process.communicate()
        note = f"command timed out after {timeout}s"
        stderr = f"{stderr}\n{note}" if stderr else note
        return subprocess.CompletedProcess(args, 124, stdout, stderr)


def run(binary: str, key_env: str, base_url: str, model: str, evidence: Path) -> dict:
    if shutil.which("agy") is None:
        raise SystemExit("agy is not installed; install Antigravity CLI and retry")
    key = os.environ.get(key_env)
    if not key:
        raise SystemExit(f"{key_env} is not set; export it before the probe")
    home = Path(tempfile.mkdtemp(prefix="baton-agy-home-"))
    cwd = Path(tempfile.mkdtemp(prefix="baton-agy-cwd-"))
    (home / "config.json").write_text(json.dumps({
        "antigravity": {
            "mode": "api",
            "apiKeyEnv": key_env,
            "apiBaseUrl": base_url,
        },
        "defaultModels": {"antigravity": model},
    }, indent=2))
    env = os.environ.copy()
    env["BATON_HOME"] = str(home)
    try:
        started = time.time()
        run_turn = run_baton(
            [binary, "-C", str(cwd), "exec", "-p", "antigravity", "--json", "Reply with exactly AGY_OK"],
            env,
            180,
        )
        seconds = round(time.time() - started, 1)
        try:
            payload = json.loads(run_turn.stdout)
        except json.JSONDecodeError:
            payload = {}
        def safe(value):
            return value.replace(key, "[redacted]") if value else value
        ps = subprocess.run(
            [binary, "ps", "--json"], env=env, capture_output=True, text=True, timeout=30
        )
        try:
            process_payload = json.loads(ps.stdout)
            processes = process_payload.get("processes") if ps.returncode == 0 and isinstance(process_payload, dict) else ["unparseable"]
            if not isinstance(processes, list):
                processes = ["unparseable"]
        except json.JSONDecodeError:
            processes = ["unparseable"]
        result = {
            "exit_code": run_turn.returncode,
            "seconds": seconds,
            "ok": payload.get("ok"),
            "phase": payload.get("phase"),
            "text": safe(payload.get("text")),
            "error": safe(payload.get("error")),
            "thread_id": payload.get("thread_id"),
            "stderr": safe(run_turn.stderr[-400:]),
            "processes": processes,
        }
        passed = (
            run_turn.returncode == 0
            and payload.get("ok") is True
            and payload.get("phase") == "completed"
            and payload.get("text") == "AGY_OK"
            and payload.get("error") is None
            and processes == []
        )
        result["passed"] = passed
        (evidence / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
        (evidence / "stdout.json").write_text(safe(run_turn.stdout))
        return result
    finally:
        shutil.rmtree(home, ignore_errors=True)
        shutil.rmtree(cwd, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=str(Path.home() / ".cargo/bin/baton"))
    parser.add_argument("--key-env", default="CODEX_ICA_API_KEY")
    parser.add_argument("--base-url", default="https://api.servicesessentials.ibm.com")
    parser.add_argument("--model", default="gemini-3.7-flash")
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()
    evidence = Path(args.evidence_dir) / f"antigravity-probe-{int(time.time())}"
    evidence.mkdir(parents=True, exist_ok=True)
    try:
        result = run(os.path.expanduser(args.binary), args.key_env, args.base_url, args.model, evidence)
    except (subprocess.TimeoutExpired, subprocess.CalledProcessError) as error:
        print(json.dumps({"passed": False, "error": str(error), "evidence": str(evidence)}, indent=2))
        return 1
    print(json.dumps({**result, "evidence": str(evidence)}, indent=2))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
