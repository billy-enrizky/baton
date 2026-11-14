#!/usr/bin/env python3
"""Cursor capability probe: vendor login, no custom endpoint, exact errors."""
import argparse, json, os, shutil, subprocess, sys, tempfile, time
from pathlib import Path


def run(binary: str, home: Path, cwd: Path, args: list[str]) -> subprocess.CompletedProcess:
    env = os.environ.copy(); env["BATON_HOME"] = str(home)
    return subprocess.run([binary, *args], env=env, capture_output=True, text=True, timeout=60)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=str(Path.home() / ".cargo/bin/baton"))
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()
    binary = os.path.expanduser(args.binary)
    if shutil.which("cursor-agent") is None:
        print(json.dumps({"passed": False, "error": "cursor-agent is not installed"}))
        return 2
    version = subprocess.run(["cursor-agent", "--version"], capture_output=True, text=True).stdout.strip()
    status = subprocess.run(["cursor-agent", "status"], capture_output=True, text=True).stdout.strip()
    evidence = Path(args.evidence_dir) / f"cursor-probe-{int(time.time())}"
    evidence.mkdir(parents=True, exist_ok=True)
    home = Path(tempfile.mkdtemp(prefix="baton-cursor-home-")); cwd = Path(tempfile.mkdtemp(prefix="baton-cursor-cwd-"))
    try:
        (home / "config.json").write_text(json.dumps({
            "cursor": {"mode": "subscription"}, "defaultModels": {"cursor": "composer-2.5"},
        }, indent=2))
        subscription = run(binary, home, cwd, ["-C", str(cwd), "exec", "-p", "cursor", "--json", "Reply with exactly CURSOR_OK"])
        logged_in = "not logged in" not in status.lower()
        login_hint_ok = logged_in or (
            subscription.returncode == 1 and "cursor-agent login" in subscription.stderr
        )
        (home / "config.json").write_text(json.dumps({
            "cursor": {"mode": "api", "apiKeyEnv": "CODEX_ICA_API_KEY", "apiBaseUrl": "https://api.servicesessentials.ibm.com/v1"},
        }, indent=2))
        custom = run(binary, home, cwd, ["-C", str(cwd), "doctor", "--json"])
        custom_rejected = custom.returncode == 1 and "Cursor does not support a custom endpoint" in custom.stderr
        result = {
            "cursor_version": version,
            "cursor_status": status,
            "subscription_exit": subscription.returncode,
            "subscription_stdout": subscription.stdout[:500],
            "subscription_stderr": subscription.stderr[-500:],
            "login_hint_ok": login_hint_ok,
            "custom_endpoint_exit": custom.returncode,
            "custom_endpoint_stderr": custom.stderr[-500:],
            "custom_endpoint_rejected": custom_rejected,
        }
        result["passed"] = login_hint_ok and custom_rejected
        (evidence / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps({**result, "evidence": str(evidence)}, indent=2))
        return 0 if result["passed"] else 1
    finally:
        shutil.rmtree(home, ignore_errors=True); shutil.rmtree(cwd, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
