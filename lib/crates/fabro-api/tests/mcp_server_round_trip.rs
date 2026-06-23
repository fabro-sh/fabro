use fabro_api::types::{
    CreateMcpServerRequest as ApiCreateMcpServerRequest, McpHttpProtocol as ApiMcpHttpProtocol,
    McpServer as ApiMcpServer, McpTransport as ApiMcpTransport,
    ReplaceMcpServerRequest as ApiReplaceMcpServerRequest,
};
use fabro_types::settings::McpTransport;
use fabro_types::settings::run::McpHttpProtocol;
use fabro_types::{McpServerDefinition, McpServerDraft, McpServerReplace};
use serde_json::json;

// Compile-time witnesses that the generated API types resolve to the same types
// as the `fabro-types` domain types via `with_replacement(...)`. If progenitor
// stops reusing the domain type, these functions stop type-checking and the
// build fails. This is what keeps the spec's signed integer formats (`i64` for
// the `u64` timeouts, `i32` for the `u16` sandbox port) from leaking into the
// public client.
const _: fn(ApiMcpServer) -> McpServerDefinition = |value| value;
const _: fn(ApiCreateMcpServerRequest) -> McpServerDraft = |value| value;
const _: fn(ApiReplaceMcpServerRequest) -> McpServerReplace = |value| value;
const _: fn(ApiMcpTransport) -> McpTransport = |value| value;
const _: fn(ApiMcpHttpProtocol) -> McpHttpProtocol = |value| value;

#[test]
fn mcp_server_response_round_trips_http_transport_json_shape() {
    let value = json!({
        "id": "sentry",
        "revision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "name": "Sentry",
        "description": "Production Sentry MCP server.",
        "transport": {
            "type": "http",
            "protocol": "streamable_http",
            "url": "https://sentry.example.com/mcp",
            "headers": {
                "X-Org": "fabro"
            }
        },
        "startup_timeout_secs": 10,
        "tool_timeout_secs": 60
    });

    let api: ApiMcpServer = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}

#[test]
fn mcp_server_response_round_trips_null_description() {
    let value = json!({
        "id": "local",
        "revision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "name": "Local",
        "description": null,
        "transport": {
            "type": "stdio",
            "command": ["npx", "@modelcontextprotocol/server-filesystem"],
            "env": {}
        },
        "startup_timeout_secs": 10,
        "tool_timeout_secs": 60
    });

    let api: ApiMcpServer = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(api).unwrap(), value);
}

#[test]
fn create_mcp_server_request_round_trips_sandbox_transport_json_shape() {
    // The sandbox transport carries a `port`, whose spec format is `int32`.
    // Reusing the domain type pins it to `u16`, so this round-trip also guards
    // against the signed-width leak.
    let value = json!({
        "id": "sandbox-mcp",
        "name": "Sandbox MCP",
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
        "name": "Sentry v2",
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
