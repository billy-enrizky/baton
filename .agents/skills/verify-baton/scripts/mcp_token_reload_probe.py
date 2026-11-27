#!/usr/bin/env python3
"""Live MCP token reload probe over HTTP.

Starts `baton mcp --http` on loopback, sends SIGHUP, and proves that a valid
replacement token takes effect while an invalid replacement keeps the old one.
It needs no provider turn or external account.
"""

import argparse
import http.client
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def post(port: int, token: str, method: str, params=None):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    frame = {"jsonrpc": "2.0", "id": 1, "method": method}
    if params is not None:
        frame["params"] = params
    connection.request(
        "POST",
        "/mcp",
        json.dumps(frame).encode(),
        {"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
    )
    response = connection.getresponse()
    data = response.read()
    connection.close()
    try:
        body = json.loads(data)
    except ValueError:
        body = {}
    return response.status, body


def wait_health(port: int) -> None:
    deadline = time.time() + 10
    while True:
        try:
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=1)
            connection.request("GET", "/health")
            response = connection.getresponse()
            response.read()
            connection.close()
            if response.status == 200:
                return
        except OSError:
            pass
        if time.time() >= deadline:
            raise RuntimeError("server did not become ready")
        time.sleep(0.05)


def wait_line(lines: list[str], text: str) -> None:
    deadline = time.time() + 10
    while time.time() < deadline:
        if any(text in line for line in lines):
            return
        time.sleep(0.05)
    raise RuntimeError(f"missing stderr line {text!r}: {lines}")


def run(binary: str, evidence: Path) -> dict:
    home = tempfile.mkdtemp(prefix="baton-mcp-reload-home-")
    cwd = tempfile.mkdtemp(prefix="baton-mcp-reload-cwd-")
    env = os.environ.copy()
    env["BATON_HOME"] = home
    try:
        token_path = Path(home) / "token"
        first = "a" * 64
        second = "b" * 64
        token_path.write_text(first + "\n")
        token_path.chmod(0o600)
        port = free_port()
        process = subprocess.Popen(
            [binary, "mcp", "--http", f"127.0.0.1:{port}", "--token-file", str(token_path)],
            cwd=cwd,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        lines: list[str] = []

        def read_stderr() -> None:
            for line in process.stderr:
                lines.append(line)

        threading.Thread(target=read_stderr, daemon=True).start()
        try:
            wait_health(port)
            first_status, first_init = post(
                port, first, "initialize", {"protocolVersion": "2025-06-18"}
            )
            token_path.write_text(second + "\n")
            process.send_signal(signal.SIGHUP)
            wait_line(lines, "reloaded the MCP bearer token")
            old_status, _ = post(port, first, "initialize", {"protocolVersion": "2025-06-18"})
            new_status, new_init = post(
                port, second, "initialize", {"protocolVersion": "2025-06-18"}
            )
            token_path.write_text("short\n")
            process.send_signal(signal.SIGHUP)
            wait_line(lines, "MCP token reload failed")
            kept_status, _ = post(
                port, second, "initialize", {"protocolVersion": "2025-06-18"}
            )
            result = {
                "first_status": first_status,
                "first_protocol": first_init.get("result", {}).get("protocolVersion"),
                "old_status_after_reload": old_status,
                "new_status_after_reload": new_status,
                "new_protocol": new_init.get("result", {}).get("protocolVersion"),
                "kept_status_after_invalid_reload": kept_status,
                "stderr_has_reload": any("reloaded the MCP bearer token" in line for line in lines),
                "stderr_has_failure": any("MCP token reload failed" in line for line in lines),
            }
            passed = (
                result["first_status"] == 200
                and result["first_protocol"] == "2025-06-18"
                and result["old_status_after_reload"] == 401
                and result["new_status_after_reload"] == 200
                and result["new_protocol"] == "2025-06-18"
                and result["kept_status_after_invalid_reload"] == 200
                and result["stderr_has_reload"]
                and result["stderr_has_failure"]
            )
        finally:
            process.send_signal(signal.SIGINT)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)
            result["server_exit"] = process.returncode
            passed = passed and process.returncode == 0
        result["passed"] = passed
        (evidence / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
        return result
    finally:
        shutil.rmtree(home, ignore_errors=True)
        shutil.rmtree(cwd, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=str(Path.home() / ".cargo/bin/baton"))
    parser.add_argument("--evidence-dir", default="local_docs/verification/evidence")
    args = parser.parse_args()
    evidence = Path(args.evidence_dir) / f"mcp-token-reload-probe-{int(time.time())}"
    evidence.mkdir(parents=True, exist_ok=True)
    try:
        result = run(os.path.expanduser(args.binary), evidence)
    except (RuntimeError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"passed": False, "error": str(error), "evidence": str(evidence)}, indent=2))
        return 1
    print(json.dumps({**result, "evidence": str(evidence)}, indent=2))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
