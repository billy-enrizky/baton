#!/usr/bin/env python3
"""A scripted Codex app-server used by the demo recorder."""

import json
import os
import sys

if "app-server" not in sys.argv:
    print("codex 0.0.0")
    sys.exit(0)


def send(value):
    sys.stdout.write(json.dumps(value) + "\n")
    sys.stdout.flush()


def edit():
    target = os.environ.get("BATON_DEMO_FILE")
    if not target:
        return
    body = "".join(f"probe line {index:03d}\n" for index in range(1, 61))
    with open(target, "w", encoding="utf-8") as handle:
        handle.write(body)


for line in sys.stdin:
    try:
        frame = json.loads(line)
    except json.JSONDecodeError:
        continue
    method = frame.get("method")
    request_id = frame.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif method == "thread/start":
        send({"jsonrpc": "2.0", "id": request_id, "result": {"thread": {"id": "demo-thread", "path": None}}})
    elif method == "thread/resume":
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif method == "model/list":
        send({"jsonrpc": "2.0", "id": request_id, "result": {"data": [], "nextCursor": None}})
    elif method == "turn/start":
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"turn": {"id": "demo-turn", "status": "inProgress"}},
        })
        edit()
        send({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {
                "threadId": "demo-thread",
                "turn": {
                    "id": "demo-turn",
                    "status": "completed",
                    "items": [
                        {"type": "agentMessage", "id": "demo-msg", "text": "rewrote the file"}
                    ],
                },
            },
        })
    elif request_id is not None:
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
