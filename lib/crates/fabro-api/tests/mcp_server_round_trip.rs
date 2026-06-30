use fabro_api::types::{
    CreateMcpServerRequest as ApiCreateMcpServerRequest, McpHttpProtocol as ApiMcpHttpProtocol,
    McpServer as ApiMcpServer, McpTransport as ApiMcpTransport,
    McpTransportView as ApiMcpTransportView, ReplaceMcpServerRequest as ApiReplaceMcpServerRequest,
};
use fabro_types::settings::McpTransport;
use fabro_types::settings::run::McpHttpProtocol;
use fabro_types::{McpServerDraft, McpServerReplace, McpServerView, McpTransportView};
use serde_json::json;

// Compile-time witnesses that the generated API types resolve to the same types
// as the `fabro-types` domain types via `with_replacement(...)`. If progenitor
// stops reusing the domain type, these functions stop type-checking and the
// build fails. This also keeps the spec's signed integer formats (`i64` for the
// `u64` timeouts, `i32` for the `u16` sandbox port) from leaking into the
// public client.
//
// Read responses (`McpServer`) reuse the value-omitting `McpServerView` /
// `McpTransportView`; write requests reuse the full-transport draft/replace
// types.
const _: fn(ApiMcpServer) -> McpServerView = |value| value;
const _: fn(ApiMcpTransportView) -> McpTransportView = |value| value;
const _: fn(ApiCreateMcpServerRequest) -> McpServerDraft = |value| value;
const _: fn(ApiReplaceMcpServerRequest) -> McpServerReplace = |value| value;
const _: fn(ApiMcpTransport) -> McpTransport = |value| value;
const _: fn(ApiMcpHttpProtocol) -> McpHttpProtocol = |value| value;

// The response JSON below is the exact shape the spec's `McpServer` schema
// declares: `display_name` (not the internal `name`), and a `McpTransportView`
// transport whose env/header *values* are replaced by `*_keys` name lists. If
// the handler ever serialized the internal `McpServerDefinition` instead, these
// round-trips would fail.
#[test]
fn mcp_server_response_round_trips_http_transport_view_shape() {
    let value = json!({
        "id": "sentry",
        "revision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "display_name": "Sentry",
        "description": "Production Sentry MCP server.",
        "transport": {
            "type": "http",
            "protocol": "streamable_http",
            "url": "https://sentry.example.com/mcp",
            "header_keys": ["X-Org"]
        },
        "startup_timeout_secs": 10,
        "tool_timeout_secs": 60
    });

    let api: ApiMcpServer = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}

#[test]
fn mcp_server_response_round_trips_stdio_view_and_null_description() {
    let value = json!({
        "id": "local",
        "revision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "display_name": "Local",
        "description": null,
        "transport": {
            "type": "stdio",
            "command": ["npx", "@modelcontextprotocol/server-filesystem"],
            "env_keys": ["HOME", "PATH"]
        },
        "startup_timeout_secs": 10,
        "tool_timeout_secs": 60
    });

    let api: ApiMcpServer = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}

#[test]
fn mcp_server_response_round_trips_sandbox_view_shape() {
    // The sandbox view carries a `port`, whose spec format is `int32`. Reusing
    // `McpTransportView` pins it to `u16`, so this round-trip also guards the
    // signed-width leak on the read side.
    let value = json!({
        "id": "sandbox-mcp",
        "revision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "display_name": "Sandbox MCP",
        "description": null,
        "transport": {
            "type": "sandbox",
            "protocol": "sse",
            "command": ["./serve"],
            "port": 8080,
            "env_keys": ["NODE_ENV"]
        },
        "startup_timeout_secs": 15,
        "tool_timeout_secs": 90
    });

    let api: ApiMcpServer = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}

// Write requests use `display_name` and the full `McpTransport`, since a client
// must be able to supply env/header values when creating or replacing.
#[test]
fn create_mcp_server_request_round_trips_sandbox_transport_json_shape() {
    let value = json!({
        "id": "sandbox-mcp",
        "display_name": "Sandbox MCP",
        "transport": {
            "type": "sandbox",
            "protocol": "sse",
            "command": ["./serve"],
            "port": 8080,
            "env": {
                "NODE_ENV": "production"
            }
        },
        "startup_timeout_secs": 15,
        "tool_timeout_secs": 90
    });

    let api: ApiCreateMcpServerRequest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}

#[test]
fn replace_mcp_server_request_round_trips_http_transport_json_shape() {
    let value = json!({
        "display_name": "Sentry v2",
        "description": "Updated.",
        "transport": {
            "type": "http",
            "protocol": "streamable_http",
            "url": "https://sentry.example.com/mcp/v2",
            "headers": {}
        },
        "startup_timeout_secs": 20,
        "tool_timeout_secs": 120
    });

    let api: ApiReplaceMcpServerRequest = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}
