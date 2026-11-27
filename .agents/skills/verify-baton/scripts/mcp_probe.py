#!/usr/bin/env python3
"""Live MCP stdio probe: initialize, list tools, and call the read-only tools.

Seeds one empty thread with `baton new`, then talks to `baton mcp` over
newline-delimited JSON-RPC. It needs no provider turn or external account.
Uses a throwaway BATON_HOME and never touches the real store.
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
    home = tempfile.mkdtemp(prefix="baton-mcp-home-")
    cwd = tempfile.mkdtemp(prefix="baton-mcp-cwd-")
    env = os.environ.copy()
    env["BATON_HOME"] = home
    try:
        seed = subprocess.run(
            [binary, "-C", cwd, "new", "mcp seed"],
            env=env,
            capture_output=True,
            text=True,
        )
        if seed.returncode != 0:
            raise RuntimeError(
                "baton new failed: " + (seed.stderr.strip() or seed.stdout.strip())
            )
        listed = subprocess.run(
            [binary, "-C", cwd, "threads", "--json"],
            env=env,
            capture_output=True,
            text=True,
        )
        if listed.returncode != 0:
            raise RuntimeError(
                "baton threads failed: " + (listed.stderr.strip() or listed.stdout.strip())
            )
        threads_json = json.loads(listed.stdout)
        if not threads_json:
            raise RuntimeError("baton new created no thread")
        seed_thread = threads_json[0]
        thread_id = seed_thread["thread_id"]
        provider = seed_thread["provider"]

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
            "baton_status", "baton_threads", "baton_turns", "baton_agents", "baton_queue",
            "baton_messages", "baton_search", "baton_latest_diff", "baton_doctor",
            "baton_processes", "baton_orchestration_runs",
            "baton_orchestration_report", "baton_fork", "baton_activity",
            "baton_memories",
        }
        status_error, status = rpc.tool("baton_status")
        turns_error, turns = rpc.tool("baton_turns")
        agents_error, agents = rpc.tool("baton_agents")
        threads_error, threads = rpc.tool("baton_threads")
        doctor_error, doctor = rpc.tool("baton_doctor")
        unknown_error, unknown = rpc.tool("baton_not_a_tool")

        process.stdin.close()
        exit_code = process.wait(timeout=15)
        stderr = process.stderr.read()

        read_only_process = subprocess.Popen(
            [binary, "mcp", "--read-only"],
            cwd=cwd,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        read_only = Rpc(read_only_process)
        read_only_init = read_only.call("initialize", {"protocolVersion": "2025-06-18"})
        read_only_tools = [
            tool["name"] for tool in read_only.call("tools/list")["result"]["tools"]
        ]
        fork_response = read_only.call(
            "tools/call", {"name": "baton_fork", "arguments": {}}
        )
        queue_clear_response = read_only.call(
            "tools/call",
            {"name": "baton_queue", "arguments": {"clear": True, "thread": thread_id}},
        )
        queue_read_error, queue_read = read_only.tool(
            "baton_queue", {"thread": thread_id}
        )
        read_only_process.stdin.close()
        read_only_exit = read_only_process.wait(timeout=15)
        read_only_stderr = read_only_process.stderr.read()

        allow_process = subprocess.Popen(
            [
                binary,
                "mcp",
                "--allow-tool",
                "baton_status",
                "--allow-tool",
                "baton_queue",
            ],
            cwd=cwd,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        allow = Rpc(allow_process)
        allow_init = allow.call("initialize", {"protocolVersion": "2025-06-18"})
        allow_tools = [tool["name"] for tool in allow.call("tools/list")["result"]["tools"]]
        allow_status_error, allow_status = allow.tool("baton_status")
        allow_turns_response = allow.call(
            "tools/call", {"name": "baton_turns", "arguments": {}}
        )
        allow_fork_response = allow.call(
            "tools/call", {"name": "baton_fork", "arguments": {}}
        )
        allow_queue_error, allow_queue = allow.tool(
            "baton_queue", {"thread": thread_id}
        )
        allow_process.stdin.close()
        allow_exit = allow_process.wait(timeout=15)
        allow_stderr = allow_process.stderr.read()

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
            "agents_error": agents_error,
            "agents_count": agents.get("count"),
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
            "read_only_tool_count": len(read_only_tools),
            "read_only_fork_hidden": "baton_fork" not in read_only_tools,
            "read_only_queue_listed": "baton_queue" in read_only_tools,
            "read_only_instructions": read_only_init["result"]["instructions"],
            "read_only_fork_error": (fork_response.get("error") or {}).get("code"),
            "read_only_queue_clear_error": (queue_clear_response.get("error") or {}).get("code"),
            "read_only_queue_list_error": queue_read_error,
            "read_only_queue_count": queue_read.get("count"),
            "read_only_exit_code": read_only_exit,
            "read_only_stderr": read_only_stderr[-300:],
            "allow_tool_count": len(allow_tools),
            "allow_tools": allow_tools,
            "allow_instructions": allow_init["result"]["instructions"],
            "allow_status_error": allow_status_error,
            "allow_status_provider": allow_status.get("provider"),
            "allow_turns_error": (allow_turns_response.get("error") or {}).get("code"),
            "allow_fork_error": (allow_fork_response.get("error") or {}).get("code"),
            "allow_queue_list_error": allow_queue_error,
            "allow_queue_count": allow_queue.get("count"),
            "allow_exit_code": allow_exit,
            "allow_stderr": allow_stderr[-300:],
        }
        passed = (
            result["protocol_version"] == "2025-06-18"
            and server.get("name") == "baton"
            and not result["required_tools"]
            and result["tool_count"] == len(required)
            and not status_error
            and status.get("thread_id") == thread_id
            and status.get("provider") == provider
            and (status.get("latest_turn") or {}).get("phase") is None
            and status.get("queue_count") == 0
            and not agents_error
            and agents.get("count") == 0
            and not turns_error
            and turns.get("count") == 0
            and not threads_error
            and threads.get("count") == 1
            and not doctor_error
            and doctor.get("next_action")
            and unknown_error
            and "unknown baton tool" in json.dumps(unknown)
            and exit_code == 0
            and result["read_only_tool_count"] == len(required) - 1
            and result["read_only_fork_hidden"]
            and result["read_only_queue_listed"]
            and "read-only" in result["read_only_instructions"]
            and result["read_only_fork_error"] == -32003
            and result["read_only_queue_clear_error"] == -32003
            and not result["read_only_queue_list_error"]
            and result["read_only_queue_count"] == 0
            and read_only_exit == 0
            and not read_only_stderr
            and result["allow_tool_count"] == 2
            and result["allow_tools"] == ["baton_status", "baton_queue"]
            and "baton_status" in result["allow_instructions"]
            and "baton_queue" in result["allow_instructions"]
            and not result["allow_status_error"]
            and result["allow_status_provider"] == provider
            and result["allow_turns_error"] == -32003
            and result["allow_fork_error"] == -32003
            and not result["allow_queue_list_error"]
            and result["allow_queue_count"] == 0
            and allow_exit == 0
            and not allow_stderr
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
