#!/usr/bin/env python3
"""Live Grok turn through baton with the ICA OpenAI-compatible endpoint."""
import argparse, json, os, shutil, signal, subprocess, sys, tempfile, time
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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=str(Path.home() / ".cargo/bin/baton"))
    parser.add_argument("--key-env", default="CODEX_ICA_API_KEY")
    parser.add_argument("--base-url", default="https://api.servicesessentials.ibm.com/v1")
    parser.add_argument("--model", default="gemini-3.7-flash")
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()
    binary = os.path.expanduser(args.binary)
    if shutil.which("grok") is None:
        print(json.dumps({"passed": False, "error": "grok is not installed"}))
        return 2
    key = os.environ.get(args.key_env)
    if not key:
        print(json.dumps({"passed": False, "error": f"{args.key_env} is not set"}))
        return 2
    home = Path(tempfile.mkdtemp(prefix="baton-grok-home-"))
    cwd = Path(tempfile.mkdtemp(prefix="baton-grok-cwd-"))
    (home / "config.json").write_text(json.dumps({
        "grok": {"mode": "api", "apiKeyEnv": args.key_env, "apiBaseUrl": args.base_url},
        "defaultModels": {"grok": args.model},
    }, indent=2))
    env = os.environ.copy(); env["BATON_HOME"] = str(home)
    evidence = Path(args.evidence_dir) / f"grok-probe-{int(time.time())}"
    evidence.mkdir(parents=True, exist_ok=True)
    try:
        started = time.time()
        run = run_baton([binary, "-C", str(cwd), "exec", "-p", "grok", "--json", "Reply with exactly GROK_OK"], env, 180)
        payload = json.loads(run.stdout) if run.stdout.strip() else {}
        safe_stdout = run.stdout.replace(key, "[redacted]")
        def safe(value):
            return value.replace(key, "[redacted]") if value else value
        ps = subprocess.run([binary, "ps", "--json"], env=env, capture_output=True, text=True, timeout=30)
        try:
            process_payload = json.loads(ps.stdout)
            processes = process_payload.get("processes") if ps.returncode == 0 and isinstance(process_payload, dict) else ["unparseable"]
            if not isinstance(processes, list):
                processes = ["unparseable"]
        except json.JSONDecodeError:
            processes = ["unparseable"]
        version = subprocess.run(["grok", "--version"], capture_output=True, text=True).stdout.strip()
        result = {
            "grok_version": version,
            "exit_code": run.returncode,
            "seconds": round(time.time() - started, 1),
            "ok": payload.get("ok"),
            "phase": payload.get("phase"),
            "text": safe(payload.get("text")),
            "error": safe(payload.get("error")),
            "stderr": safe(run.stderr[-400:]),
            "processes": processes,
        }
        result["passed"] = run.returncode == 0 and payload.get("ok") is True and payload.get("text") == "GROK_OK" and payload.get("error") is None and processes == []
        (evidence / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
        (evidence / "stdout.json").write_text(safe_stdout)
        print(json.dumps({**result, "evidence": str(evidence)}, indent=2))
        return 0 if result["passed"] else 1
    finally:
        shutil.rmtree(home, ignore_errors=True); shutil.rmtree(cwd, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
