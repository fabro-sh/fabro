use fabro_static::EnvVars;
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

// ---------------------------------------------------------------------------
// Provider enum — compile-time safe provider identity
// ---------------------------------------------------------------------------

/// Known LLM provider variants.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum Provider {
    Anthropic,
    #[serde(rename = "openai", alias = "open_ai")]
    #[strum(to_string = "openai", serialize = "open_ai")]
    OpenAi,
    Gemini,
    Kimi,
    Zai,
    Minimax,
    #[strum(to_string = "inception", serialize = "inception_labs")]
    Inception,
    #[serde(rename = "openai_compatible", alias = "open_ai_compatible")]
    #[strum(to_string = "openai_compatible", serialize = "open_ai_compatible")]
    OpenAiCompatible,
}

impl Provider {
    /// All known provider variants, for use in guardrail tests and iteration.
    pub const ALL: &[Self] = &[
        Self::Anthropic,
        Self::OpenAi,
        Self::Gemini,
        Self::Kimi,
        Self::Zai,
        Self::Minimax,
        Self::Inception,
    ];

    /// Environment variable names that can provide the API key for this
    /// provider. Gemini accepts either `GEMINI_API_KEY` or
    /// `GOOGLE_API_KEY`.
    #[must_use]
    pub fn api_key_env_vars(self) -> &'static [&'static str] {
        match self {
            Self::Anthropic => &[EnvVars::ANTHROPIC_API_KEY],
            Self::OpenAi => &[EnvVars::OPENAI_API_KEY],
            Self::Gemini => &[EnvVars::GEMINI_API_KEY, EnvVars::GOOGLE_API_KEY],
            Self::Kimi => &[EnvVars::KIMI_API_KEY],
            Self::Zai => &[EnvVars::ZAI_API_KEY],
            Self::Minimax => &[EnvVars::MINIMAX_API_KEY],
            Self::Inception => &[EnvVars::INCEPTION_API_KEY],
            Self::OpenAiCompatible => &[],
        }
    }

    /// Returns `true` if at least one of the provider's API key env vars is
    /// set.
    #[must_use]
    #[expect(
        clippy::disallowed_methods,
        reason = "Provider discovery intentionally checks the process env for known API-key names."
    )]
    pub fn has_api_key(self) -> bool {
        self.api_key_env_vars()
            .iter()
            .any(|var| std::env::var(var).is_ok())
    }

    /// Pick the best default provider based on which API keys are available.
    ///
    /// Checks Anthropic → OpenAI → Gemini; falls back to Anthropic if none
    /// have a key configured.
    #[must_use]
    pub fn default_from_env() -> Self {
        Self::default_with(Self::has_api_key)
    }

    /// Pick the best default provider based on an explicit configured list.
    #[must_use]
    pub fn default_for_configured(configured: &[Self]) -> Self {
        Self::default_with(|p| configured.contains(&p))
    }

    /// Testable core of [`default_from_env`]: walks the precedence list and
    /// returns the first provider for which `is_configured` returns `true`.
    fn default_with(is_configured: impl Fn(Self) -> bool) -> Self {
        const PRECEDENCE: [Provider; 3] = [Provider::Anthropic, Provider::OpenAi, Provider::Gemini];
        PRECEDENCE
            .iter()
            .copied()
            .find(|&p| is_configured(p))
            .unwrap_or(Self::Anthropic)
    }

    /// Human-readable display name for the provider.
    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::OpenAi => "OpenAI",
            Self::Gemini => "Gemini",
            Self::Kimi => "Kimi",
            Self::Zai => "Zai",
            Self::Minimax => "Minimax",
            Self::Inception => "Inception",
            Self::OpenAiCompatible => "OpenAI Compatible",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kimi() {
        assert_eq!("kimi".parse::<Provider>().unwrap(), Provider::Kimi);
    }

    #[test]
    fn parse_zai() {
        assert_eq!("zai".parse::<Provider>().unwrap(), Provider::Zai);
    }

    #[test]
    fn parse_minimax() {
        assert_eq!("minimax".parse::<Provider>().unwrap(), Provider::Minimax);
    }

    #[test]
    fn kimi_as_str() {
        assert_eq!(Provider::Kimi.to_string(), "kimi");
        assert_eq!(<&'static str>::from(Provider::Kimi), "kimi");
    }

    #[test]
    fn zai_as_str() {
        assert_eq!(Provider::Zai.to_string(), "zai");
        assert_eq!(<&'static str>::from(Provider::Zai), "zai");
    }

    #[test]
    fn minimax_as_str() {
        assert_eq!(Provider::Minimax.to_string(), "minimax");
        assert_eq!(<&'static str>::from(Provider::Minimax), "minimax");
    }

    #[test]
    fn parse_inception() {
        assert_eq!(
            "inception".parse::<Provider>().unwrap(),
            Provider::Inception
        );
        assert_eq!(
            "inception_labs".parse::<Provider>().unwrap(),
            Provider::Inception
        );
    }

    #[test]
    fn inception_as_str() {
        assert_eq!(Provider::Inception.to_string(), "inception");
        assert_eq!(<&'static str>::from(Provider::Inception), "inception");
    }

    #[test]
    fn default_with_all_configured_prefers_anthropic() {
        assert_eq!(Provider::default_with(|_| true), Provider::Anthropic);
    }

    #[test]
    fn default_with_only_openai() {
        assert_eq!(
            Provider::default_with(|p| p == Provider::OpenAi),
            Provider::OpenAi
        );
    }

    #[test]
    fn default_for_configured_only_openai() {
        assert_eq!(
            Provider::default_for_configured(&[Provider::OpenAi]),
            Provider::OpenAi
        );
    }

    #[test]
    fn default_with_only_gemini() {
        assert_eq!(
            Provider::default_with(|p| p == Provider::Gemini),
            Provider::Gemini
        );
    }

    #[test]
    fn default_with_openai_and_gemini_prefers_openai() {
        assert_eq!(
            Provider::default_with(|p| p == Provider::OpenAi || p == Provider::Gemini),
            Provider::OpenAi,
        );
    }

    #[test]
    fn default_with_none_configured_falls_back_to_anthropic() {
        assert_eq!(Provider::default_with(|_| false), Provider::Anthropic);
    }

    #[test]
    fn default_with_only_kimi_falls_back_to_anthropic() {
        assert_eq!(
            Provider::default_with(|p| p == Provider::Kimi),
            Provider::Anthropic
        );
    }

    #[test]
    fn api_key_env_vars_anthropic() {
        assert_eq!(Provider::Anthropic.api_key_env_vars(), &[
            "ANTHROPIC_API_KEY"
        ]);
    }

    #[test]
    fn api_key_env_vars_openai() {
        assert_eq!(Provider::OpenAi.api_key_env_vars(), &["OPENAI_API_KEY"]);
    }

    #[test]
    fn api_key_env_vars_gemini_has_two() {
        let vars = Provider::Gemini.api_key_env_vars();
        assert_eq!(vars.len(), 2);
        assert_eq!(vars, &["GEMINI_API_KEY", "GOOGLE_API_KEY"]);
    }

    #[test]
    fn api_key_env_vars_kimi() {
        assert_eq!(Provider::Kimi.api_key_env_vars(), &["KIMI_API_KEY"]);
    }

    #[test]
    fn api_key_env_vars_zai() {
        assert_eq!(Provider::Zai.api_key_env_vars(), &["ZAI_API_KEY"]);
    }

    #[test]
    fn api_key_env_vars_minimax() {
        assert_eq!(Provider::Minimax.api_key_env_vars(), &["MINIMAX_API_KEY"]);
    }

    #[test]
    fn api_key_env_vars_inception() {
        assert_eq!(Provider::Inception.api_key_env_vars(), &[
            "INCEPTION_API_KEY"
        ]);
    }

    #[test]
    fn every_provider_has_at_least_one_env_var() {
        assert!(
            Provider::ALL
                .iter()
                .all(|p| !p.api_key_env_vars().is_empty())
        );
    }
}
