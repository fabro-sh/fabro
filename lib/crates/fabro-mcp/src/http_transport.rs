#![expect(
    clippy::disallowed_types,
    reason = "MCP HTTP helpers parse operator-provided endpoint URLs; callers control what is logged"
)]

use std::collections::HashMap;

use anyhow::{Context as _, Result};
use fabro_http::{HeaderMap, HeaderName, HeaderValue, Url};

use crate::config::McpHttpProtocol;

pub fn sandbox_mcp_http_url(protocol: McpHttpProtocol, preview_url: &str) -> Result<String> {
    match protocol {
        McpHttpProtocol::StreamableHttp => Ok(preview_url.to_string()),
        McpHttpProtocol::Sse => {
            let mut url = Url::parse(preview_url).context("invalid sandbox MCP preview URL")?;
            let path = url.path().trim_end_matches('/');
            url.set_path(&format!("{path}/sse"));
            Ok(url.to_string())
        }
    }
}

pub(crate) fn headers_from_pairs(headers: &HashMap<String, String>) -> Result<HeaderMap> {
    let mut header_map = HeaderMap::new();
    for (key, value) in headers {
        let name = HeaderName::from_bytes(key.as_bytes())
            .with_context(|| format!("invalid header name '{key}'"))?;
        let val = HeaderValue::from_str(value)
            .with_context(|| format!("invalid header value for '{key}'"))?;
        header_map.insert(name, val);
    }
    Ok(header_map)
}
