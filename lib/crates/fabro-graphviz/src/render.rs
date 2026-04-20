use std::sync::LazyLock;

/// Dark mode CSS injected into SVG output (leading newline included for
/// insertion).
const DARK_MODE_STYLE: &str = r##"
<style>
  @media (prefers-color-scheme: dark) {
    text { fill: #e0e0e0 !important; }
    [stroke="#357f9e"] { stroke: #5bb8d8; }
    [stroke="#666666"] { stroke: #999999; }
    polygon[fill="#357f9e"] { fill: #5bb8d8; }
    polygon[fill="#666666"] { fill: #999999; }
  }
</style>"##;

/// DOT graph-level defaults injected after the first `{`.
const DOT_STYLE_DEFAULTS: &str = r##"
    bgcolor="transparent"
    node [color="#357f9e", fontname="Helvetica", fontsize=12, fontcolor="#1a1a1a"]
    edge [color="#666666", fontname="Helvetica", fontsize=10, fontcolor="#666666"]
"##;

static RANKDIR_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"rankdir\s*=\s*\w+").expect("hardcoded regex should compile")
});
static WHITE_BG_POLYGON_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r#"<polygon\b[^>]*fill="white"[^>]*stroke="none"[^>]*/>|<polygon\b[^>]*stroke="none"[^>]*fill="white"[^>]*/>"#,
    )
    .expect("hardcoded regex should compile")
});

/// Rewrite `rankdir=...` in DOT source.
#[must_use]
pub fn apply_direction<'a>(source: &'a str, direction: &str) -> std::borrow::Cow<'a, str> {
    let replacement = format!("rankdir={direction}");
    RANKDIR_RE.replace(source, replacement.as_str())
}

/// Inject DOT graph-level style defaults.
#[must_use]
pub fn inject_dot_style_defaults(source: &str) -> String {
    let Some(pos) = source.find('{') else {
        return source.to_string();
    };
    let (before, after) = source.split_at(pos + 1);
    format!("{before}{DOT_STYLE_DEFAULTS}{after}")
}

/// Post-process raw SVG output from Graphviz.
#[must_use]
pub fn postprocess_svg(raw: Vec<u8>) -> Vec<u8> {
    let mut svg = String::from_utf8(raw)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());

    svg = WHITE_BG_POLYGON_RE.replace_all(&svg, "").into_owned();

    if let Some(svg_close) = svg
        .find("<svg")
        .and_then(|start| svg[start..].find('>').map(|end| start + end))
    {
        svg.insert_str(svg_close + 1, DARK_MODE_STYLE);
    }

    svg.into_bytes()
}

/// Render styled DOT source into SVG via the vendored Graphviz library.
pub fn render_dot(source: &str) -> anyhow::Result<Vec<u8>> {
    let styled_source = inject_dot_style_defaults(source);
    let raw = graphviz_sys::render_dot_to_svg(&styled_source)
        .map_err(|e| anyhow::anyhow!("Graphviz rendering failed: {e}"))?;
    Ok(postprocess_svg(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_direction_rewrites_rankdir() {
        let source = "digraph { rankdir=LR a -> b }";
        let rewritten = apply_direction(source, "TB");
        assert!(rewritten.contains("rankdir=TB"));
    }

    #[test]
    fn inject_style_defaults_adds_graph_defaults() {
        let source = "digraph X { a -> b }";
        let styled = inject_dot_style_defaults(source);
        assert!(styled.contains("bgcolor=\"transparent\""));
        assert!(styled.contains("node [color=\"#357f9e\""));
    }

    #[test]
    fn postprocess_svg_removes_white_background() {
        let raw = br#"<svg><polygon fill="white" stroke="none" points="0,0"/><text>x</text></svg>"#
            .to_vec();
        let svg = String::from_utf8(postprocess_svg(raw)).unwrap();
        assert!(!svg.contains("fill=\"white\""));
        assert!(svg.contains("@media (prefers-color-scheme: dark)"));
    }

    #[test]
    fn render_dot_produces_svg() {
        let svg = render_dot("digraph { a -> b }").unwrap();
        assert!(String::from_utf8(svg).unwrap().contains("<svg"));
    }

    #[test]
    fn render_dot_complex_graph() {
        let source = r#"digraph {
            subgraph cluster_0 {
                label = "process #1";
                a0 -> a1 -> a2 -> a3;
            }
            subgraph cluster_1 {
                label = "process #2";
                b0 -> b1 -> b2 -> b3;
            }
            start -> a0;
            start -> b0;
            a1 -> b3;
            b2 -> a3;
            a3 -> end;
            b3 -> end;
        }"#;
        let svg = render_dot(source).unwrap();
        let svg_str = String::from_utf8(svg).unwrap();
        assert!(svg_str.contains("<svg"));
        assert!(svg_str.contains("process #1"));
        assert!(svg_str.contains("process #2"));
    }

    #[test]
    fn render_dot_invalid_source_returns_error() {
        let result = render_dot("not valid dot {{{");
        assert!(result.is_err());
    }
}
