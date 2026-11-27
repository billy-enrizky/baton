#!/usr/bin/env python3
"""Live MCP HTTPS probe with a temporary self-signed certificate.

Seeds one empty thread with `baton new`, starts `baton mcp --http` with a
0600 token and a PEM certificate/key, then sends HTTPS JSON-RPC. It needs
`openssl` and no provider turn or external account.
"""

import argparse
import http.client
import json
import os
import shutil
import signal
import socket
import ssl
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def request(context: ssl.SSLContext, port: int, token: str, method: str, params=None):
    connection = http.client.HTTPSConnection("127.0.0.1", port, context=context, timeout=10)
    frame = {"jsonrpc": "2.0", "id": 1, "method": method}
    if params is not None:
        frame["params"] = params
    body = json.dumps(frame).encode()
    connection.request(
        "POST",
        "/mcp",
        body,
        {"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
    )
    response = connection.getresponse()
    data = response.read()
    connection.close()
    return response.status, json.loads(data)


def run(binary: str, evidence: Path) -> dict:
    home = tempfile.mkdtemp(prefix="baton-mcp-https-home-")
    cwd = tempfile.mkdtemp(prefix="baton-mcp-https-cwd-")
    env = os.environ.copy()
    env["BATON_HOME"] = home
    try:
        seed = subprocess.run(
            [binary, "-C", cwd, "new", "https seed"], env=env, capture_output=True, text=True
        )
        if seed.returncode != 0:
            raise RuntimeError("baton new failed: " + (seed.stderr.strip() or seed.stdout.strip()))
        token_path = Path(home) / "token"
        token = "a" * 64
        token_path.write_text(token + "\n")
        token_path.chmod(0o600)
        cert_path = Path(home) / "cert.pem"
        key_path = Path(home) / "key.pem"
        generated = subprocess.run(
            [
                "openssl",
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                str(key_path),
                "-out",
                str(cert_path),
                "-days",
                "1",
                "-subj",
                "/CN=localhost",
                "-addext",
                "subjectAltName=DNS:localhost,IP:127.0.0.1",
                "-addext",
                "extendedKeyUsage=serverAuth",
                "-addext",
                "keyUsage=digitalSignature,keyEncipherment",
            ],
            capture_output=True,
            text=True,
        )
        if generated.returncode != 0:
            raise RuntimeError("openssl failed: " + generated.stderr.strip())
        key_path.chmod(0o600)
        port = free_port()
        process = subprocess.Popen(
            [
                binary,
                "mcp",
                "--http",
                f"127.0.0.1:{port}",
                "--token-file",
                str(token_path),
                "--tls-cert",
                str(cert_path),
                "--tls-key",
                str(key_path),
            ],
            cwd=cwd,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        context = ssl.create_default_context(cafile=str(cert_path))
        try:
            deadline = time.time() + 10
            health = None
            while True:
                try:
                    connection = http.client.HTTPSConnection(
                        "127.0.0.1", port, context=context, timeout=1
                    )
                    connection.request("GET", "/health")
                    response = connection.getresponse()
                    health = (response.status, json.loads(response.read()))
                    connection.close()
                    break
                except (OSError, ssl.SSLError, ValueError):
                    if time.time() >= deadline:
                        raise RuntimeError("HTTPS server did not become ready")
                    time.sleep(0.05)
            initialize_status, initialized = request(
                context, port, token, "initialize", {"protocolVersion": "2025-06-18"}
            )
            tools_status, tools = request(context, port, token, "tools/list")
            status_status, status = request(
                context, port, token, "tools/call", {"name": "baton_status", "arguments": {}}
            )
            tools_names = [tool["name"] for tool in tools["result"]["tools"]]
            result = {
                "health": health,
                "initialize_status": initialize_status,
                "protocol_version": initialized["result"]["protocolVersion"],
                "tools_status": tools_status,
                "tool_count": len(tools_names),
                "required_tools_present": {
                    "baton_status",
                    "baton_queue",
                    "baton_fork",
                }.issubset(tools_names),
                "status_status": status_status,
                "status_error": status["result"].get("isError"),
                "status_provider": json.loads(status["result"]["content"][0]["text"])["provider"],
            }
            passed = (
                health[0] == 200
                and result["initialize_status"] == 200
                and result["protocol_version"] == "2025-06-18"
                and result["tools_status"] == 200
                and result["tool_count"] == 15
                and result["required_tools_present"]
                and result["status_status"] == 200
                and not result["status_error"]
                and result["status_provider"] == "codex"
            )
        finally:
            process.send_signal(signal.SIGINT)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)
            stderr = process.stderr.read()
            result["server_exit"] = process.returncode
            result["server_stderr"] = stderr[-400:]
            passed = passed and process.returncode == 0 and "https://127.0.0.1" in stderr
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
    evidence = Path(args.evidence_dir) / f"mcp-https-probe-{int(time.time())}"
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
