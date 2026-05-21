use agent_client_protocol::schema::{
    ContentBlock, ContentChunk, SessionNotification, SessionUpdate,
};
use serde_json::json;

pub const SESSION_ID: &str = "sess-1";

pub fn agent_message_chunk(session_id: &str, text: &str) -> SessionNotification {
    SessionNotification::new(
        session_id.to_string(),
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::from(text.to_string()))),
    )
}

pub fn agent_message_chunk_json(session_id: &str, text: &str) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": agent_message_chunk(session_id, text),
    })
}

pub fn fake_acp_agent_script() -> &'static str {
    r#"
import json
import os
import signal
import sys
import time

methods = []
session_id = "sess-1"
prompt_count = 0

if os.environ.get("ACP_PID_RECORD"):
    with open(os.environ["ACP_PID_RECORD"], "w", encoding="utf-8") as record:
        record.write(str(os.getpid()))

if os.environ.get("ACP_ENV_RECORD"):
    keys = [
        key.strip()
        for key in os.environ.get(
            "ACP_ENV_RECORD_KEYS",
            "ANTHROPIC_API_KEY,OPENAI_API_KEY,GEMINI_API_KEY",
        ).split(",")
        if key.strip()
    ]
    snapshot = {key: os.environ[key] for key in keys if key in os.environ}
    with open(os.environ["ACP_ENV_RECORD"], "w", encoding="utf-8") as record:
        record.write(json.dumps(snapshot, sort_keys=True))

def handle_sigterm(signum, frame):
    if os.environ.get("ACP_LINGER_TERMINATED"):
        with open(os.environ["ACP_LINGER_TERMINATED"], "w", encoding="utf-8") as record:
            record.write("terminated\n")
    sys.exit(0)

signal.signal(signal.SIGTERM, handle_sigterm)

def send(message):
    print(json.dumps(message), flush=True)

def respond(message, result):
    send({"jsonrpc": "2.0", "id": message["id"], "result": result})

def record_methods():
    if os.environ.get("ACP_RECORD"):
        with open(os.environ["ACP_RECORD"], "w", encoding="utf-8") as record:
            record.write("\n".join(methods) + "\n")

def first_prompt_text(message):
    prompt = message.get("params", {}).get("prompt", [])
    if not prompt:
        return ""
    first = prompt[0]
    if isinstance(first, str):
        return first
    return first.get("text", "")

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    methods.append(method)

    if method == "initialize":
        if os.environ.get("ACP_MODE") == "slow_initialize":
            time.sleep(60)
        respond(message, {"protocolVersion": 1, "agentCapabilities": {}})
    elif method == "session/new":
        if os.environ.get("ACP_SESSION_NEW_PARAMS"):
            with open(os.environ["ACP_SESSION_NEW_PARAMS"], "w", encoding="utf-8") as record:
                record.write(json.dumps(message.get("params", {}), separators=(",", ":")))
        respond(message, {"sessionId": session_id})
    elif method == "session/prompt":
        prompt_count += 1
        if os.environ.get("ACP_PROMPT_RECORD"):
            with open(os.environ["ACP_PROMPT_RECORD"], "w", encoding="utf-8") as record:
                record.write(json.dumps(message.get("params", {})))
        mode = os.environ.get("ACP_MODE", "normal")
        if mode == "timeout":
            time.sleep(60)
        if mode == "malformed":
            print("malformed json", file=sys.stderr, flush=True)
            print("{not-json", flush=True)
            break
        if mode == "early_exit":
            print("early boom", file=sys.stderr, flush=True)
            sys.exit(2)
        if mode == "write_file":
            path = os.environ.get("ACP_WRITE_PATH", "hello.txt")
            parent = os.path.dirname(path)
            if parent:
                os.makedirs(parent, exist_ok=True)
            with open(path, "w", encoding="utf-8") as file:
                file.write(os.environ.get("ACP_WRITE_CONTENT", "hello from sandbox\n"))
            if os.environ.get("ACP_WRITE_MTIME_EPOCH"):
                mtime = float(os.environ["ACP_WRITE_MTIME_EPOCH"])
                os.utime(path, (mtime, mtime))
        if mode == "cancel":
            send({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": "waiting for cancellation"}
                    }
                }
            })
            for cancel_line in sys.stdin:
                cancel_message = json.loads(cancel_line)
                if cancel_message.get("method") == "session/cancel":
                    with open(os.environ["ACP_CANCEL_RECORD"], "w", encoding="utf-8") as record:
                        record.write("session/cancel\n")
                    respond(message, {"stopReason": "cancelled"})
                    sys.exit(0)
        if mode == "ignore_cancel":
            send({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": "waiting for ignored cancellation"}
                    }
                }
            })
            for control_line in sys.stdin:
                control_message = json.loads(control_line)
                methods.append(control_message.get("method"))
                if control_message.get("method") == "session/cancel":
                    if os.environ.get("ACP_CANCEL_RECORD"):
                        with open(os.environ["ACP_CANCEL_RECORD"], "w", encoding="utf-8") as record:
                            record.write("session/cancel\n")
                    record_methods()
                    time.sleep(60)
        if mode == "permission":
            send({
                "jsonrpc": "2.0",
                "id": "permission-1",
                "method": "session/request_permission",
                "params": {
                    "sessionId": session_id,
                    "toolCall": {"toolCallId": "tool-1"},
                    "options": [
                        {"optionId": "reject", "name": "Reject", "kind": "reject_once"},
                        {"optionId": "once", "name": "Allow once", "kind": "allow_once"},
                        {"optionId": "always", "name": "Allow always", "kind": "allow_always"}
                    ]
                }
            })
            permission_response = json.loads(sys.stdin.readline())
            with open(os.environ["ACP_PERMISSION"], "w", encoding="utf-8") as permission:
                permission.write(json.dumps(permission_response.get("result", {}), separators=(",", ":")))
        if mode == "interrupt_steer":
            if prompt_count == 1:
                send({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": session_id,
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": "interrupted "}
                        }
                    }
                })
                for control_line in sys.stdin:
                    control_message = json.loads(control_line)
                    methods.append(control_message.get("method"))
                    if control_message.get("method") == "session/cancel":
                        if os.environ.get("ACP_CANCEL_RECORD"):
                            with open(os.environ["ACP_CANCEL_RECORD"], "w", encoding="utf-8") as record:
                                record.write("session/cancel\n")
                        respond(message, {"stopReason": "cancelled"})
                        break
                continue
            if os.environ.get("ACP_STEER_PROMPT_RECORD"):
                with open(os.environ["ACP_STEER_PROMPT_RECORD"], "w", encoding="utf-8") as record:
                    record.write(json.dumps(message.get("params", {}), separators=(",", ":")))
            send({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": "steered:" + first_prompt_text(message)}
                    }
                }
            })
            record_methods()
            respond(message, {"stopReason": "end_turn"})
            break
        if mode == "steer":
            if prompt_count == 1:
                send({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": session_id,
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": "initial "}
                        }
                    }
                })
                respond(message, {"stopReason": "end_turn"})
                continue
            if os.environ.get("ACP_STEER_PROMPT_RECORD"):
                with open(os.environ["ACP_STEER_PROMPT_RECORD"], "w", encoding="utf-8") as record:
                    record.write(json.dumps(message.get("params", {}), separators=(",", ":")))
            send({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": "steered:" + first_prompt_text(message)}
                    }
                }
            })
            record_methods()
            respond(message, {"stopReason": "end_turn"})
            break
        for text in ["hello ", "from acp"]:
            send({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": text}
                    }
                }
            })
        record_methods()
        respond(message, {"stopReason": os.environ.get("ACP_STOP_REASON", "end_turn")})
        if mode == "linger_after_response":
            while True:
                time.sleep(1)
        break
    else:
        send({
            "jsonrpc": "2.0",
            "id": message.get("id"),
            "error": {"code": -32601, "message": "method not found"}
        })
"#
}
