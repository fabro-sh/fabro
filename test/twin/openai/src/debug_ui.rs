use std::fmt::Write;

use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router};

use crate::logs::RequestLog;
use crate::state::{AppState, DebugSnapshot, NamespaceSnapshot, ScenarioSnapshot};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/__debug", get(debug_page))
        .route("/__debug/state.json", get(debug_state_json))
}

async fn debug_page(State(state): State<AppState>) -> impl IntoResponse {
    let snapshot = state.debug_snapshot();
    Html(render_html(&snapshot))
}

async fn debug_state_json(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.debug_snapshot())
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}...", &s[..max_len])
    } else {
        s.to_owned()
    }
}

fn render_html(snapshot: &DebugSnapshot) -> String {
    let mut html = String::with_capacity(8192);

    // Count totals for summary
    let ns_count = snapshot.namespaces.len();
    let sc_count: usize = snapshot
        .namespaces
        .iter()
        .map(|ns| ns.scenarios.len())
        .sum();
    let rq_count: usize = snapshot
        .namespaces
        .iter()
        .map(|ns| ns.request_logs.len())
        .sum();

    html.push_str(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>twin-openai // debug</title>
  <style>"#,
    );

    html.push_str(CSS);

    html.push_str(
        r#"</style>
</head>
<body>
  <header>
    <h1>twin-openai <span class="dim">//</span> debug</h1>
    <div class="status">
      <span class="live-dot"></span>
      <span class="live-label">live</span>
      <button onclick="refresh()">refresh now</button>
    </div>
  </header>

  <div class="summary" id="summary">
"#,
    );

    let _ = write!(
        html,
        r#"    <span id="ns-count">{ns_count} namespaces</span>
    <span class="sep">|</span>
    <span id="sc-count">{sc_count} queued scenarios</span>
    <span class="sep">|</span>
    <span id="rq-count">{rq_count} logged requests</span>
"#
    );

    html.push_str("  </div>\n\n  <div id=\"content\">\n");

    render_content(&mut html, &snapshot.namespaces);

    html.push_str("  </div>\n\n  <footer>auto-refreshing every 2s</footer>\n\n  <script>\n");
    html.push_str(JS);
    html.push_str("\n  </script>\n</body>\n</html>");

    html
}

fn render_content(html: &mut String, namespaces: &[NamespaceSnapshot]) {
    if namespaces.is_empty() {
        html.push_str("    <p class=\"empty\">(no active namespaces)</p>\n");
        return;
    }

    for ns in namespaces {
        let _ = write!(
            html,
            "    <section class=\"namespace\">\n      <h2 class=\"namespace-header\">{}</h2>\n",
            escape_html(&ns.key)
        );

        // Queued scenarios
        html.push_str("      <h3>queued scenarios</h3>\n");
        render_scenarios_table(html, &ns.scenarios);

        // Request log
        html.push_str("      <h3>request log</h3>\n");
        render_requests_table(html, &ns.request_logs);

        html.push_str("    </section>\n");
    }
}

fn render_scenarios_table(html: &mut String, scenarios: &[ScenarioSnapshot]) {
    if scenarios.is_empty() {
        html.push_str("      <p class=\"empty\">(no queued scenarios)</p>\n");
        return;
    }

    html.push_str(
        "      <table>\n        <thead><tr>\n          <th>#</th><th>endpoint</th><th>model</th><th>stream</th><th>input_contains</th><th>script</th>\n        </tr></thead>\n        <tbody>\n",
    );

    for (i, s) in scenarios.iter().enumerate() {
        let model = s
            .model
            .as_deref()
            .map_or_else(|| "--".to_owned(), escape_html);
        let stream = match s.stream {
            Some(v) => format!("{v}"),
            None => "--".to_owned(),
        };
        let input_contains = s
            .input_contains
            .as_deref()
            .map_or_else(|| "--".to_owned(), escape_html);

        let _ = writeln!(
            html,
            "          <tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            i + 1,
            escape_html(&s.endpoint),
            model,
            stream,
            input_contains,
            escape_html(&s.script_kind),
        );
    }

    html.push_str("        </tbody>\n      </table>\n");
}

fn render_requests_table(html: &mut String, logs: &[RequestLog]) {
    if logs.is_empty() {
        html.push_str("      <p class=\"empty\">(no requests logged)</p>\n");
        return;
    }

    html.push_str(
        "      <table>\n        <thead><tr>\n          <th>#</th><th>endpoint</th><th>model</th><th>stream</th><th>input text</th><th>metadata</th>\n        </tr></thead>\n        <tbody>\n",
    );

    for (i, r) in logs.iter().enumerate() {
        let meta = serde_json::to_string(&r.metadata).unwrap_or_else(|_| "{}".to_owned());
        let meta_display = if meta == "{}" {
            "--".to_owned()
        } else {
            escape_html(&truncate(&meta, 80))
        };

        let _ = writeln!(
            html,
            "          <tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            i + 1,
            escape_html(&r.endpoint),
            escape_html(&r.model),
            r.stream,
            escape_html(&truncate(&r.input_text, 120)),
            meta_display,
        );
    }

    html.push_str("        </tbody>\n      </table>\n");
}

const CSS: &str = r#"
:root {
  --bg: #0a0a0a;
  --bg-row: #111;
  --bg-row-alt: #0d0d0d;
  --bg-hover: #1a1a0a;
  --text: #ccc;
  --text-bright: #00ff41;
  --text-heading: #ffb000;
  --text-dim: #666;
  --text-error: #ff4444;
  --border: #333;
  --border-bright: #555;
}

body {
  background: var(--bg);
  color: var(--text);
  font-family: "IBM Plex Mono", "Fira Code", "Cascadia Code", monospace;
  margin: 0;
  padding: 24px;
}

header {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

h1 {
  color: var(--text-heading);
  text-transform: uppercase;
  letter-spacing: 0.2em;
  font-size: 1.1rem;
  margin: 0;
}

h1 .dim {
  color: var(--text-dim);
}

h2.namespace-header {
  color: var(--text-heading);
  border-top: 2px solid var(--border-bright);
  padding-top: 16px;
  margin-top: 32px;
}

h3 {
  color: var(--text-dim);
  text-transform: uppercase;
  font-size: 0.75rem;
  letter-spacing: 0.15em;
}

table {
  width: 100%;
  border-collapse: collapse;
  border: 2px solid var(--border);
}

th {
  color: var(--text-heading);
  text-transform: uppercase;
  font-size: 0.7rem;
  letter-spacing: 0.1em;
  padding: 6px 10px;
  text-align: left;
  border-bottom: 2px solid var(--border);
}

td {
  color: var(--text-bright);
  padding: 5px 10px;
  font-size: 0.85rem;
  border-bottom: 1px solid var(--border);
}

tr:nth-child(even) {
  background: var(--bg-row-alt);
}

tr:hover td {
  background: var(--bg-hover);
  transition: background 0.15s;
}

.empty {
  color: var(--text-error);
  font-style: italic;
  padding: 8px 0;
}

.summary {
  color: var(--text-dim);
  margin: 12px 0 24px;
  font-size: 0.85rem;
}

.sep {
  margin: 0 8px;
  color: var(--border-bright);
}

footer {
  color: var(--text-dim);
  font-size: 0.75rem;
  margin-top: 40px;
  border-top: 1px solid var(--border);
  padding-top: 12px;
}

.status {
  display: flex;
  align-items: center;
}

.live-label {
  color: var(--text-bright);
  text-transform: uppercase;
  font-size: 0.75rem;
  letter-spacing: 0.1em;
}

@keyframes pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.3; }
}

.live-dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  background: var(--text-bright);
  border-radius: 50%;
  animation: pulse 2s ease-in-out infinite;
  margin-right: 6px;
  vertical-align: middle;
}

button {
  background: transparent;
  color: var(--text-dim);
  border: 1px solid var(--border);
  padding: 2px 10px;
  font-family: inherit;
  font-size: 0.75rem;
  cursor: pointer;
  text-transform: uppercase;
  letter-spacing: 0.1em;
  margin-left: 12px;
}

button:hover {
  color: var(--text-bright);
  border-color: var(--text-bright);
}
"#;

const JS: &str = r#"
async function refresh() {
  try {
    const res = await fetch('/__debug/state.json');
    const data = await res.json();
    document.getElementById('content').innerHTML = renderState(data);
    updateSummary(data);
  } catch(e) { /* silent -- next interval will retry */ }
}

if (new URLSearchParams(window.location.search).get('refresh') !== '0') {
  setInterval(refresh, 2000);
}

function renderState(data) {
  if (data.namespaces.length === 0) {
    return '<p class="empty">(no active namespaces)</p>';
  }
  return data.namespaces.map(function(ns) {
    return '<section class="namespace">'
      + '<h2 class="namespace-header">' + esc(ns.key) + '</h2>'
      + '<h3>queued scenarios</h3>'
      + renderScenariosTable(ns.scenarios)
      + '<h3>request log</h3>'
      + renderRequestsTable(ns.request_logs)
      + '</section>';
  }).join('');
}

function renderScenariosTable(scenarios) {
  if (scenarios.length === 0) return '<p class="empty">(no queued scenarios)</p>';
  var rows = scenarios.map(function(s, i) {
    return '<tr>'
      + '<td>' + (i+1) + '</td>'
      + '<td>' + esc(s.endpoint) + '</td>'
      + '<td>' + esc(s.model || '--') + '</td>'
      + '<td>' + (s.stream === null ? '--' : s.stream) + '</td>'
      + '<td>' + esc(s.input_contains || '--') + '</td>'
      + '<td>' + esc(s.script_kind) + '</td>'
      + '</tr>';
  }).join('');
  return '<table><thead><tr>'
    + '<th>#</th><th>endpoint</th><th>model</th><th>stream</th><th>input_contains</th><th>script</th>'
    + '</tr></thead><tbody>' + rows + '</tbody></table>';
}

function renderRequestsTable(logs) {
  if (logs.length === 0) return '<p class="empty">(no requests logged)</p>';
  var rows = logs.map(function(r, i) {
    var meta = JSON.stringify(r.metadata);
    if (meta === '{}') meta = '--';
    return '<tr>'
      + '<td>' + (i+1) + '</td>'
      + '<td>' + esc(r.endpoint) + '</td>'
      + '<td>' + esc(r.model) + '</td>'
      + '<td>' + r.stream + '</td>'
      + '<td>' + esc(trunc(r.input_text, 120)) + '</td>'
      + '<td>' + esc(trunc(meta, 80)) + '</td>'
      + '</tr>';
  }).join('');
  return '<table><thead><tr>'
    + '<th>#</th><th>endpoint</th><th>model</th><th>stream</th><th>input text</th><th>metadata</th>'
    + '</tr></thead><tbody>' + rows + '</tbody></table>';
}

function updateSummary(data) {
  var sc = 0, rq = 0;
  data.namespaces.forEach(function(ns) {
    sc += ns.scenarios.length;
    rq += ns.request_logs.length;
  });
  document.getElementById('ns-count').textContent = data.namespaces.length + ' namespaces';
  document.getElementById('sc-count').textContent = sc + ' queued scenarios';
  document.getElementById('rq-count').textContent = rq + ' logged requests';
}

function esc(s) {
  return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;').replace(/'/g,'&#39;');
}

function trunc(s, n) {
  return s.length > n ? s.slice(0, n) + '...' : s;
}
"#;
