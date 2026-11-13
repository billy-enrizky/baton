#!/usr/bin/env python3
"""Live MCP stdio probe: initialize, list tools, and call the read-only tools.

Seeds one failed OpenCode turn (the 401 records a native id without model
tokens), then talks to `baton mcp` over newline-delimited JSON-RPC. Uses a
throwaway BATON_HOME and never touches the real store.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


class Rpc:
    def __init__(self, process: subprocess.Popen):
        self.process = process
        self.next_id = 0

    def call(self, method: str, params=None) -> dict:
        self.next_id += 1
        frame = {"jsonrpc": "2.0", "id": self.next_id, "method": method}
        if params is not None:
            frame["params"] = params
        self.process.stdin.write(json.dumps(frame) + "\n")
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        if not line:
            raise RuntimeError(f"baton mcp closed before answering {method}")
        response = json.loads(line)
        if response.get("id") != self.next_id:
            raise RuntimeError(f"response id mismatch for {method}: {response}")
        return response

    def tool(self, name: str, arguments=None) -> tuple[bool, dict]:
        response = self.call("tools/call", {"name": name, "arguments": arguments or {}})
        if "error" in response:
            raise RuntimeError(f"tool {name} returned a protocol error: {response['error']}")
        result = response["result"]
        text = result["content"][0]["text"]
        return bool(result.get("isError")), json.loads(text)


def run(binary: str, evidence: Path) -> dict:
    if shutil.which("opencode") is None:
        raise SystemExit("opencode is not installed; the MCP probe needs its 401 seed")
    home = tempfile.mkdtemp(prefix="baton-mcp-home-")
    cwd = tempfile.mkdtemp(prefix="baton-mcp-cwd-")
    env = os.environ.copy()
    env["BATON_HOME"] = home
    try:
        seed = subprocess.run(
            [binary, "-C", cwd, "exec", "-p", "opencode", "--json", "mcp seed"],
            env=env,
            capture_output=True,
            text=True,
        )
        seed_json = json.loads(seed.stdout)
        thread_id = seed_json["thread_id"]
        if seed_json.get("phase") != "failed" or "401" not in (seed_json.get("error") or ""):
            raise RuntimeError("the OpenCode seed did not fail with the expected 401")

        process = subprocess.Popen(
            [binary, "mcp"],
            cwd=cwd,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        rpc = Rpc(process)
        initialized = rpc.call("initialize", {"protocolVersion": "2025-06-18"})
        server = initialized["result"]["serverInfo"]
        listed = rpc.call("tools/list")
        tools = [tool["name"] for tool in listed["result"]["tools"]]
        required = {
            "baton_status", "baton_threads", "baton_turns", "baton_queue",
            "baton_messages", "baton_search", "baton_latest_diff", "baton_doctor",
            "baton_processes", "baton_orchestration_runs",
            "baton_orchestration_report", "baton_fork",
        }
        status_error, status = rpc.tool("baton_status")
        turns_error, turns = rpc.tool("baton_turns")
        threads_error, threads = rpc.tool("baton_threads")
        doctor_error, doctor = rpc.tool("baton_doctor")
        unknown_error, unknown = rpc.tool("baton_not_a_tool")

        process.stdin.close()
        exit_code = process.wait(timeout=15)
        stderr = process.stderr.read()

        result = {
            "protocol_version": initialized["result"]["protocolVersion"],
            "server": server,
            "tool_count": len(tools),
            "required_tools": sorted(required.difference(tools)),
            "status_error": status_error,
            "status": {
                "schema_version": status.get("schema_version"),
                "thread_id": status.get("thread_id"),
                "provider": status.get("provider"),
                "phase": (status.get("latest_turn") or {}).get("phase"),
                "queue_count": status.get("queue_count"),
            },
            "turns_error": turns_error,
            "turn_count": turns.get("count"),
            "provider_turn_id": (turns.get("turns") or [{}])[0].get("provider_turn_id"),
            "threads_error": threads_error,
            "thread_count": threads.get("count"),
            "doctor_error": doctor_error,
            "doctor_ready": doctor.get("ready"),
            "doctor_next_action": doctor.get("next_action"),
            "unknown_error": unknown_error,
            "unknown_text": unknown,
            "exit_code": exit_code,
            "stderr": stderr[-300:],
        }
        passed = (
            result["protocol_version"] == "2025-06-18"
            and server.get("name") == "baton"
            and not result["required_tools"]
            and result["tool_count"] == len(required)
            and not status_error
            and status.get("thread_id") == thread_id
            and status.get("provider") == "opencode"
            and (status.get("latest_turn") or {}).get("phase") == "failed"
            and status.get("queue_count") == 0
            and not turns_error
            and turns.get("count") == 1
            and str((turns.get("turns") or [{}])[0].get("provider_turn_id") or "").startswith("msg_")
            and not threads_error
            and threads.get("count") == 1
            and not doctor_error
            and doctor.get("next_action")
            and "opencode" in (doctor.get("ready") or [])
            and unknown_error
            and "unknown baton tool" in json.dumps(unknown)
            and exit_code == 0
        )
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
    import time
    evidence = Path(args.evidence_dir) / f"mcp-probe-{int(time.time())}"
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
