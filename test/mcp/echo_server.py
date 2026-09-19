#!/usr/bin/env python3
"""A one-tool MCP server over stdio for the server's scenario tests.

Speaks JSON-RPC 2.0, one message per line on stdin and stdout, as the MCP
specification describes for the stdio transport. It exposes ``echo(message)``,
which answers the message, so a test can see the server's tool reach an
agent session's tool list. Dependency-free.
"""

import json
import sys

SERVER_INFO = {"name": "fabro-test-echo", "version": "1.0.0"}
PROTOCOL_VERSION = "2025-03-26"

TOOLS = [
    {
        "name": "echo",
        "description": "Echo back the message",
        "inputSchema": {
            "type": "object",
            "properties": {"message": {"type": "string"}},
            "required": ["message"],
        },
    }
]


def handle(request):
    """Answer one request, or ``None`` for a notification."""
    method = request.get("method")
    request_id = request.get("id")
    params = request.get("params") or {}
    if method == "initialize":
        return {
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "protocolVersion": params.get("protocolVersion", PROTOCOL_VERSION),
                "capabilities": {"tools": {}},
                "serverInfo": SERVER_INFO,
            },
        }
    if method == "ping":
        return {"jsonrpc": "2.0", "id": request_id, "result": {}}
    if method == "tools/list":
        return {"jsonrpc": "2.0", "id": request_id, "result": {"tools": TOOLS}}
    if method == "tools/call":
        message = (params.get("arguments") or {}).get("message", "")
        return {
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"content": [{"type": "text", "text": message}]},
        }
    if request_id is None:
        return None
    return {
        "jsonrpc": "2.0",
        "id": request_id,
        "error": {"code": -32601, "message": f"Method not found: {method}"},
    }


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError:
            continue
        response = handle(request)
        if response is not None:
            sys.stdout.write(json.dumps(response) + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
