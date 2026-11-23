//! Content Security Policy generation.
//!
//! The policy is built once at server startup from the embedded SPA
//! `index.html` so any inline `<script>` hashes don't drift from the
//! template. Third-party sources are enumerated explicitly.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use fabro_types::GitHubHost;
use sha2::{Digest, Sha256};

pub(crate) const INSTALL_MODE_SCRIPT_BODY: &str = "window.__FABRO_MODE__ = \"install\";";

pub fn policy(host: &GitHubHost) -> String {
    let mut script_hashes = inline_script_hashes_from_embedded_index();
    script_hashes.push(script_hash(INSTALL_MODE_SCRIPT_BODY));
    build_policy_with_hashes(&script_hashes, host)
}

fn inline_script_hashes_from_embedded_index() -> Vec<String> {
    let Some(bytes) = fabro_spa::get("index.html") else {
        return Vec::new();
    };
    let Ok(text) = std::str::from_utf8(bytes.as_ref()) else {
        return Vec::new();
    };
    inline_script_hashes(text)
}

/// Extract the sha256 hash (as `sha256-<base64>` CSP source) of every
/// inline `<script>` block in the given HTML that has no `src`
/// attribute.
pub(crate) fn inline_script_hashes(html: &str) -> Vec<String> {
    let mut hashes = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = html[cursor..].find("<script") {
        let tag_start = cursor + offset;
        let rest = &html[tag_start..];
        let Some(open_end_rel) = rest.find('>') else {
            break;
        };
        let content_start = tag_start + open_end_rel + 1;
        let open_tag = &html[tag_start..content_start];
        // External scripts don't need a CSP hash — `script-src 'self'`
        // covers them when they live under `/assets/…`.
        if open_tag.contains(" src=") {
            cursor = content_start;
            continue;
        }
        let Some(close_rel) = html[content_start..].find("</script>") else {
            break;
        };
        let content_end = content_start + close_rel;
        let body = &html[content_start..content_end];
        hashes.push(script_hash(body));
        cursor = content_end;
    }
    hashes
}

fn script_hash(script: &str) -> String {
    let hash = Sha256::digest(script.as_bytes());
    format!("sha256-{}", STANDARD.encode(hash))
}

/// Avatars of a GitHub Enterprise host are served from the host itself
/// (`https://avatars.<host>`) or from its web origin, not from
/// githubusercontent.com.
fn github_img_sources(host: &GitHubHost) -> String {
    if host.is_github_com() {
        "https://avatars.githubusercontent.com".to_string()
    } else {
        format!("{} https://avatars.{}", host.web_url(), host.host())
    }
}

fn build_policy_with_hashes(script_hashes: &[String], host: &GitHubHost) -> String {
    let inline_script_sources = if script_hashes.is_empty() {
        String::new()
    } else {
        format!(
            " {}",
            script_hashes
                .iter()
                .map(|h| format!("'{h}'"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    // `'wasm-unsafe-eval'` lets `@viz-js/viz` instantiate the Graphviz
    // WASM module for graph rendering. `'unsafe-inline'` on style-src is
    // accepted as a pragmatic concession — React and UI utilities regularly
    // set inline `style=` attributes, and blocking those would break normal
    // interactions. The meaningful XSS protection still comes from the
    // script-src restrictions above. `ws:`/`wss:` keep the same-origin
    // terminal WebSocket working across browsers that do not treat `'self'`
    // as matching WebSocket schemes for connect-src. `frame-src https:`
    // allows signed sandbox VNC preview iframes from dynamic Daytona preview
    // hosts. The GitHub web origin in form-action allows the install-mode
    // GitHub App manifest POST handoff. Loopback resume targets keep CLI
    // OAuth working when the browser-visible origin and canonical origin use
    // different loopback hostnames on a dynamically chosen port.
    format!(
        "default-src 'self'; \
         script-src 'self'{inline_script_sources} 'wasm-unsafe-eval'; \
         style-src 'self' https://fonts.googleapis.com 'unsafe-inline'; \
         font-src 'self' https://fonts.gstatic.com; \
         img-src 'self' data: blob: {img_sources}; \
         connect-src 'self' ws: wss:; \
         worker-src 'self' blob:; \
         frame-src 'self' https:; \
         manifest-src 'self'; \
         frame-ancestors 'none'; \
         base-uri 'self'; \
         form-action 'self' {web_url} http://127.0.0.1:*/auth/cli/resume http://localhost:*/auth/cli/resume; \
         object-src 'none'",
        img_sources = github_img_sources(host),
        web_url = host.web_url(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_script_hashes_are_stable_for_known_body() {
        // sha256("hello") =
        // 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        // base64 of that digest:
        let html = "<script>hello</script>";
        let hashes = inline_script_hashes(html);
        assert_eq!(hashes.len(), 1);
        assert_eq!(
            hashes[0],
            "sha256-LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ="
        );
    }

    #[test]
    fn external_scripts_are_ignored() {
        let html = r#"<script src="/assets/app.js"></script>
<script>console.log('inline')</script>"#;
        let hashes = inline_script_hashes(html);
        assert_eq!(hashes.len(), 1, "only the inline script should be hashed");
    }

    #[test]
    fn inline_script_body_includes_exact_whitespace() {
        // Browsers hash the raw bytes between `<script>` and `</script>`
        // including leading/trailing whitespace. A missing newline would
        // make the header mismatch and break CSP.
        let a = inline_script_hashes("<script>  foo  </script>");
        let b = inline_script_hashes("<script>foo</script>");
        assert_ne!(a, b, "whitespace must be preserved in the hashed body");
    }

    #[test]
    fn policy_allows_install_mode_inline_bootstrap() {
        let policy = policy(&GitHubHost::default());
        let expected_hash = script_hash(INSTALL_MODE_SCRIPT_BODY);
        assert!(
            policy.contains(&format!("'{expected_hash}'")),
            "install-mode inline script hash should be present in CSP: {policy}"
        );
    }

    #[test]
    fn policy_allows_cli_loopback_resume_form_posts() {
        let policy = build_policy_with_hashes(&[], &GitHubHost::default());
        assert!(
            policy.contains("form-action 'self' https://github.com http://127.0.0.1:*/auth/cli/resume http://localhost:*/auth/cli/resume"),
            "CLI auth can submit the resume form to the canonical loopback origin on a dynamic port: {policy}"
        );
    }

    #[test]
    fn policy_is_constructed_with_expected_directives() {
        let policy = build_policy_with_hashes(&["sha256-abc".to_string()], &GitHubHost::default());
        assert!(policy.contains("default-src 'self'"));
        assert!(
            policy.contains("script-src 'self' 'sha256-abc' 'wasm-unsafe-eval'"),
            "script-src must carry inline hash and wasm-unsafe-eval"
        );
        assert!(policy.contains("font-src 'self' https://fonts.gstatic.com"));
        assert!(policy.contains("style-src 'self' https://fonts.googleapis.com 'unsafe-inline'"));
        assert!(policy.contains("connect-src 'self' ws: wss:"));
        assert!(
            policy.contains("frame-src 'self' https:"),
            "signed sandbox VNC previews are embedded from dynamic HTTPS origins"
        );
        assert!(
            policy.contains("form-action 'self' https://github.com"),
            "GitHub App manifest creation posts directly to github.com"
        );
        assert!(policy.contains("frame-ancestors 'none'"));
        assert!(policy.contains("object-src 'none'"));
    }

    #[test]
    fn github_com_policy_is_unchanged() {
        let policy = build_policy_with_hashes(&[], &GitHubHost::default());
        assert!(policy.contains(
            "img-src 'self' data: blob: https://avatars.githubusercontent.com; connect-src"
        ));
    }

    #[test]
    fn enterprise_policy_uses_the_configured_web_origin() {
        let host = GitHubHost::new("https://ghe.example.com", None).unwrap();
        let policy = build_policy_with_hashes(&[], &host);
        assert!(policy.contains(
            "img-src 'self' data: blob: https://ghe.example.com https://avatars.ghe.example.com; connect-src"
        ));
        assert!(policy.contains(
            "form-action 'self' https://ghe.example.com http://127.0.0.1:*/auth/cli/resume"
        ));
        assert!(!policy.contains("https://github.com"));
        assert!(!policy.contains("githubusercontent.com"));
    }

    #[test]
    fn policy_is_valid_without_inline_scripts() {
        let policy = build_policy_with_hashes(&[], &GitHubHost::default());
        assert!(
            policy.contains("script-src 'self' 'wasm-unsafe-eval'"),
            "empty hash list should not produce a stray space: {policy}"
        );
    }

    #[test]
    fn embedded_spa_index_builds_a_policy() {
        // Guards against the embedded asset going missing or becoming
        // unreadable in a way that would leave the CSP header blank.
        let policy = policy(&GitHubHost::default());
        assert!(
            policy.contains("script-src 'self'"),
            "embedded SPA policy should contain a script-src directive"
        );
        assert!(policy.contains("'wasm-unsafe-eval'"));
    }
}
