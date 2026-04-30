//! Shared HTML shell for browser-facing auth pages.
//!
//! Both the web sign-in flow and the CLI auth flow render styled status pages
//! (errors, confirmations) directly from the server. This module provides the
//! single dark-themed shell those pages share so they stay visually consistent.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};

/// Wrap `body` in the standard Fabro auth page chrome (logo, panel, dark
/// atmosphere) and return it as an HTML response with `status`.
pub(crate) fn browser_shell(status: StatusCode, title: &str, body: &str) -> Response {
    let html = format!(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>{title} · Fabro</title>
    <link rel="icon" href="/images/favicon.svg" type="image/svg+xml">
    <style>
      :root {{
        color-scheme: dark;
        --page:       #0F1729;
        --overlay:    rgba(255, 255, 255, 0.04);
        --overlay-2:  rgba(255, 255, 255, 0.08);
        --line:       rgba(255, 255, 255, 0.08);
        --fg:         #ffffff;
        --fg-2:       #E8EDF3;
        --fg-3:       #A8B5C5;
        --teal-500:   #67B2D7;
        --teal-700:   #357F9E;
        --mint:       #5AC8A8;
        --coral:      #E86B6B;
        --on-primary: #0F1729;
        font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif, "Apple Color Emoji", "Segoe UI Emoji";
      }}
      * {{ box-sizing: border-box; }}
      body {{
        margin: 0;
        min-height: 100dvh;
        color: var(--fg-2);
        background-color: var(--page);
        background-image:
          radial-gradient(ellipse 120% 60% at 15% 5%, rgba(53, 127, 158, 0.14) 0%, transparent 50%),
          radial-gradient(ellipse 80% 50% at 85% 90%, rgba(90, 200, 168, 0.08) 0%, transparent 45%);
        background-attachment: fixed;
        display: flex;
        align-items: center;
        justify-content: center;
        padding: 3rem 1rem;
      }}
      main {{ width: 100%; max-width: 24rem; }}
      .brand {{
        display: flex;
        justify-content: center;
        margin-bottom: 1.75rem;
      }}
      .brand img {{ width: 3rem; height: 3rem; }}
      .panel {{
        background: rgba(37, 44, 61, 0.82);
        border: 1px solid var(--line);
        border-radius: 0.75rem;
        padding: 2rem;
        box-shadow: 0 20px 48px rgba(0, 0, 0, 0.35);
        backdrop-filter: blur(4px);
        -webkit-backdrop-filter: blur(4px);
      }}
      .stack > * + * {{ margin-top: 1.5rem; }}
      .eyebrow {{
        display: inline-flex;
        align-items: center;
        gap: 0.5rem;
        margin: 0;
        color: var(--mint);
        font-size: 0.75rem;
        font-weight: 600;
        letter-spacing: 0.06em;
        text-transform: uppercase;
      }}
      .eyebrow::before {{
        content: "";
        width: 0.375rem;
        height: 0.375rem;
        border-radius: 9999px;
        background: var(--mint);
        box-shadow: 0 0 0 3px rgba(90, 200, 168, 0.22);
      }}
      .eyebrow.error {{ color: var(--coral); }}
      .eyebrow.error::before {{
        background: var(--coral);
        box-shadow: 0 0 0 3px rgba(232, 107, 107, 0.22);
      }}
      h1 {{
        margin: 0.625rem 0 0;
        color: var(--fg);
        font-size: 1.5rem;
        line-height: 1.2;
        font-weight: 600;
        letter-spacing: -0.015em;
        text-wrap: balance;
      }}
      p {{
        margin: 0;
        color: var(--fg-3);
        font-size: 0.875rem;
        line-height: 1.6;
        text-wrap: pretty;
      }}
      code {{
        font-family: ui-monospace, "JetBrains Mono", Menlo, Consolas, monospace;
        font-size: 0.8125em;
        padding: 0.1em 0.35em;
        border-radius: 0.25rem;
        background: var(--overlay-2);
        color: var(--fg-2);
        white-space: nowrap;
      }}
      .identity {{
        display: flex;
        flex-direction: column;
        gap: 0.25rem;
        padding: 0.875rem 1rem;
        border-radius: 0.5rem;
        background: var(--overlay);
        border: 1px solid var(--line);
      }}
      .identity strong {{
        color: var(--fg);
        font-size: 0.9375rem;
        font-weight: 600;
      }}
      .identity-meta {{
        color: var(--fg-3);
        font-size: 0.8125rem;
        font-feature-settings: "tnum";
        word-break: break-word;
      }}
      .button {{
        display: inline-flex;
        align-items: center;
        justify-content: center;
        gap: 0.5rem;
        width: 100%;
        appearance: none;
        border: 0;
        border-radius: 0.5rem;
        background: var(--teal-500);
        color: var(--on-primary);
        padding: 0.625rem 1rem;
        font: inherit;
        font-size: 0.875rem;
        font-weight: 600;
        text-decoration: none;
        cursor: pointer;
        transition: background-color 120ms ease, color 120ms ease;
      }}
      .button:hover {{ background: var(--teal-700); color: var(--fg); }}
      .button:focus-visible {{
        outline: 2px solid var(--teal-500);
        outline-offset: 2px;
      }}
      form {{ margin: 0; }}
      @media (prefers-reduced-motion: reduce) {{
        .button {{ transition: none; }}
      }}
    </style>
  </head>
  <body>
    <main>
      <div class="brand">
        <img src="/images/logo.svg" alt="Fabro" width="48" height="48">
      </div>
      <div class="panel">
        <div class="stack">
          {body}
        </div>
      </div>
    </main>
  </body>
</html>"#,
    );
    let mut response = (status, Html(html)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
