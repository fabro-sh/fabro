#!/usr/bin/env python3
"""Minimal MCP server for integration testing over stdio.

Speaks JSON-RPC 2.0 over stdin/stdout per the MCP specification.
Exposes a single tool: echo(message) -> message.
"""
import json
import os
import sys

SERVER_INFO = {
    "name": "test-echo-server",
    "version": "0.1.0",
}

TOOL = {
    "name": "echo",
    "description": "Echo back the message",
    "inputSchema": {
        "type": "object",
        "properties": {
            "message": {"type": "string", "description": "Message to echo"}
        },
        "required": ["message"],
    },
}


def handle_request(req):
    method = req.get("method")
    req_id = req.get("id")
    params = req.get("params", {})

    if method == "initialize":
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": "2025-03-26",
                "capabilities": {"tools": {}},
                "serverInfo": SERVER_INFO,
            },
        }

    if method == "tools/list":
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {"tools": [TOOL]},
        }

    if method == "tools/call":
        tool_name = params.get("name")
        arguments = params.get("arguments", {})
        if tool_name == "echo":
            msg = arguments.get("message", "")
            if msg == "__cwd__":
                msg = os.getcwd()
            elif msg.startswith("__env:") and msg.endswith("__"):
                key = msg[len("__env:") : -len("__")]
                msg = os.environ.get(key, "")
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "content": [{"type": "text", "text": msg}],
                },
            }
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "content": [{"type": "text", "text": f"unknown tool: {tool_name}"}],
                "isError": True,
            },
        }

    # Notifications (no id) — just ignore
    if req_id is None:
        return None

    return {
        "jsonrpc": "2.0",
        "id": req_id,
        "error": {"code": -32601, "message": f"Method not found: {method}"},
    }


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            continue

        resp = handle_request(req)
        if resp is not None:
            sys.stdout.write(json.dumps(resp) + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
