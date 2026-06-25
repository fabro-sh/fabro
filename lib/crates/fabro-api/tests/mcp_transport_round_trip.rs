use std::any::TypeId;
use std::collections::HashMap;

use fabro_api::types::{McpHttpProtocol as ApiMcpHttpProtocol, McpTransport as ApiMcpTransport};
use fabro_types::settings::run::{McpHttpProtocol, McpTransport};

#[test]
fn mcp_transport_api_types_are_canonical() {
    assert_same_type::<ApiMcpHttpProtocol, McpHttpProtocol>();
    assert_same_type::<ApiMcpTransport, McpTransport>();
}

#[test]
fn mcp_transport_json_matches_runtime_types() {
    let transports = [
        McpTransport::Stdio {
            command: vec!["npx".to_string(), "server".to_string()],
            env:     HashMap::from([("NODE_ENV".to_string(), "production".to_string())]),
        },
        McpTransport::Http {
            protocol: McpHttpProtocol::StreamableHttp,
            url:      "https://mcp.example.com/mcp".to_string(),
            headers:  HashMap::from([(
                "Authorization".to_string(),
                "Bearer {{ secrets.MCP_TOKEN }}".to_string(),
            )]),
        },
        McpTransport::Sandbox {
            protocol: McpHttpProtocol::Sse,
            command:  vec!["playwright-mcp".to_string()],
            port:     3333,
            env:      HashMap::from([("NODE_ENV".to_string(), "test".to_string())]),
        },
    ];

    for transport in transports {
        let value = serde_json::to_value(&transport).unwrap();
        let api: ApiMcpTransport = serde_json::from_value(value.clone()).unwrap();

        assert_eq!(serde_json::to_value(api).unwrap(), value);
    }
}

#[test]
fn mcp_transport_http_protocol_defaults_to_streamable_http() {
    let value = serde_json::json!({
        "type": "http",
        "url": "https://mcp.example.com/mcp",
        "headers": {}
    });

    let api: ApiMcpTransport = serde_json::from_value(value).unwrap();

    assert!(matches!(api, McpTransport::Http {
        protocol: McpHttpProtocol::StreamableHttp,
        ..
    }));
}

fn assert_same_type<T: 'static, U: 'static>() {
    assert_eq!(TypeId::of::<T>(), TypeId::of::<U>());
}
