use fabro_static::EnvVars;
use serde_json::{Value, json};

pub fn build_context() -> Value {
    json!({
        "os": { "name": std::env::consts::OS },
        "device": { "type": std::env::consts::ARCH },
        "locale": current_locale(),
        "app": { "name": "fabro", "version": env!("CARGO_PKG_VERSION") }
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "Telemetry context captures the conventional LANG locale from process env."
)]
fn current_locale() -> String {
    let lang = std::env::var(EnvVars::LANG).unwrap_or_default();
    parse_locale(&lang)
}

fn parse_locale(lang: &str) -> String {
    if lang.is_empty() || lang == "C" || lang == "POSIX" {
        return "en-US".to_string();
    }

    // Strip encoding suffix (e.g. ".UTF-8")
    let without_encoding = lang.split('.').next().unwrap_or(lang);

    // Convert underscore to hyphen (e.g. "en_US" -> "en-US")
    without_encoding.replace('_', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_has_expected_keys() {
        let ctx = build_context();
        assert!(ctx.get("os").is_some());
        assert!(ctx["os"].get("name").is_some());
        assert!(ctx.get("device").is_some());
        assert!(ctx["device"].get("type").is_some());
        assert!(ctx.get("locale").is_some());
        assert!(ctx.get("app").is_some());
        assert_eq!(ctx["app"]["name"], "fabro");
        assert!(ctx["app"].get("version").is_some());
    }

    #[test]
    fn parse_locale_en_us_utf8() {
        assert_eq!(parse_locale("en_US.UTF-8"), "en-US");
    }

    #[test]
    fn parse_locale_c() {
        assert_eq!(parse_locale("C"), "en-US");
    }

    #[test]
    fn parse_locale_posix() {
        assert_eq!(parse_locale("POSIX"), "en-US");
    }

    #[test]
    fn parse_locale_empty() {
        assert_eq!(parse_locale(""), "en-US");
    }

    #[test]
    fn parse_locale_no_encoding() {
        assert_eq!(parse_locale("fr_FR"), "fr-FR");
    }

    #[test]
    fn parse_locale_with_encoding() {
        assert_eq!(parse_locale("de_DE.ISO-8859-1"), "de-DE");
    }
}
