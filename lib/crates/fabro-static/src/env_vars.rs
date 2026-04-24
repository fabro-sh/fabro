//! Fixed process environment variable names used or supported by Fabro.

/// Declares the environment variable names used throughout Fabro and its
/// crates.
pub struct EnvVars;

impl EnvVars {
    // Fabro core
    pub const FABRO_AUTH_FILE: &'static str = "FABRO_AUTH_FILE";
    pub const FABRO_BUILD_DATE: &'static str = "FABRO_BUILD_DATE";
    pub const FABRO_BUILD_PROFILE: &'static str = "FABRO_BUILD_PROFILE";
    pub const FABRO_BUILD_PROFILE_SUFFIX: &'static str = "FABRO_BUILD_PROFILE_SUFFIX";
    pub const FABRO_CONFIG: &'static str = "FABRO_CONFIG";
    pub const FABRO_DEBUG: &'static str = "FABRO_DEBUG";
    pub const FABRO_DEV_TOKEN: &'static str = "FABRO_DEV_TOKEN";
    pub const FABRO_ENABLE_FETCH_FEATURE_OCI_INTEGRATION: &'static str =
        "FABRO_ENABLE_FETCH_FEATURE_OCI_INTEGRATION";
    pub const FABRO_GIT_SHA: &'static str = "FABRO_GIT_SHA";
    pub const FABRO_HOME: &'static str = "FABRO_HOME";
    pub const FABRO_HTTP_PROXY_POLICY: &'static str = "FABRO_HTTP_PROXY_POLICY";
    pub const FABRO_JSON: &'static str = "FABRO_JSON";
    pub const FABRO_NO_UPGRADE_CHECK: &'static str = "FABRO_NO_UPGRADE_CHECK";
    pub const FABRO_QUIET: &'static str = "FABRO_QUIET";
    pub const FABRO_SERVER: &'static str = "FABRO_SERVER";
    pub const FABRO_SERVER_MAX_CONCURRENT_RUNS: &'static str = "FABRO_SERVER_MAX_CONCURRENT_RUNS";
    pub const FABRO_SLACK_APP_TOKEN: &'static str = "FABRO_SLACK_APP_TOKEN";
    pub const FABRO_SLACK_BOT_TOKEN: &'static str = "FABRO_SLACK_BOT_TOKEN";
    pub const FABRO_STORAGE_DIR: &'static str = "FABRO_STORAGE_DIR";
    pub const FABRO_STORAGE_ROOT: &'static str = "FABRO_STORAGE_ROOT";
    pub const FABRO_SUPPRESS_OPEN_BROWSER: &'static str = "FABRO_SUPPRESS_OPEN_BROWSER";
    pub const FABRO_TELEMETRY: &'static str = "FABRO_TELEMETRY";
    pub const FABRO_TEST_IN_MEMORY_STORE: &'static str = "FABRO_TEST_IN_MEMORY_STORE";
    pub const FABRO_TEST_MODE: &'static str = "FABRO_TEST_MODE";
    pub const FABRO_VERBOSE: &'static str = "FABRO_VERBOSE";
    pub const FABRO_WEB_URL: &'static str = "FABRO_WEB_URL";
    pub const FABRO_WORKER_TOKEN: &'static str = "FABRO_WORKER_TOKEN";

    // LLM providers and tool integrations
    pub const ANTHROPIC_API_KEY: &'static str = "ANTHROPIC_API_KEY";
    pub const ANTHROPIC_BASE_URL: &'static str = "ANTHROPIC_BASE_URL";
    pub const BRAVE_SEARCH_API_KEY: &'static str = "BRAVE_SEARCH_API_KEY";
    pub const CHATGPT_ACCOUNT_ID: &'static str = "CHATGPT_ACCOUNT_ID";
    pub const GEMINI_API_KEY: &'static str = "GEMINI_API_KEY";
    pub const GEMINI_BASE_URL: &'static str = "GEMINI_BASE_URL";
    pub const GOOGLE_API_KEY: &'static str = "GOOGLE_API_KEY";
    pub const GOPATH: &'static str = "GOPATH";
    pub const INCEPTION_API_KEY: &'static str = "INCEPTION_API_KEY";
    pub const KIMI_API_KEY: &'static str = "KIMI_API_KEY";
    pub const MINIMAX_API_KEY: &'static str = "MINIMAX_API_KEY";
    pub const OPENAI_API_KEY: &'static str = "OPENAI_API_KEY";
    pub const OPENAI_BASE_URL: &'static str = "OPENAI_BASE_URL";
    pub const OPENAI_COMPATIBLE_BASE_URL: &'static str = "OPENAI_COMPATIBLE_BASE_URL";
    pub const OPENAI_ORGANIZATION: &'static str = "OPENAI_ORGANIZATION";
    pub const OPENAI_PROJECT: &'static str = "OPENAI_PROJECT";
    pub const OPENAI_ORG_ID: &'static str = "OPENAI_ORG_ID";
    pub const OPENAI_PROJECT_ID: &'static str = "OPENAI_PROJECT_ID";
    pub const ZAI_API_KEY: &'static str = "ZAI_API_KEY";

    // GitHub, OAuth, and Slack
    pub const GH_TOKEN: &'static str = "GH_TOKEN";
    pub const GITHUB_APP_CLIENT_SECRET: &'static str = "GITHUB_APP_CLIENT_SECRET";
    pub const GITHUB_APP_PRIVATE_KEY: &'static str = "GITHUB_APP_PRIVATE_KEY";
    pub const GITHUB_APP_WEBHOOK_SECRET: &'static str = "GITHUB_APP_WEBHOOK_SECRET";
    pub const GITHUB_BASE_URL: &'static str = "GITHUB_BASE_URL";
    pub const GITHUB_TOKEN: &'static str = "GITHUB_TOKEN";
    pub const OAUTH_CALLBACK_PATH: &'static str = "OAUTH_CALLBACK_PATH";
    pub const OAUTH_CLIENT_ID: &'static str = "OAUTH_CLIENT_ID";
    pub const OAUTH_ISSUER: &'static str = "OAUTH_ISSUER";
    pub const OAUTH_PORT: &'static str = "OAUTH_PORT";
    pub const OAUTH_SCOPE: &'static str = "OAUTH_SCOPE";
    pub const SLACK_BASE_URL: &'static str = "SLACK_BASE_URL";

    // Server, sandbox, and cloud provider integration
    pub const AWS_ACCESS_KEY_ID: &'static str = "AWS_ACCESS_KEY_ID";
    pub const AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE: &'static str =
        "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE";
    pub const AWS_CONTAINER_CREDENTIALS_FULL_URI: &'static str =
        "AWS_CONTAINER_CREDENTIALS_FULL_URI";
    pub const AWS_CONTAINER_CREDENTIALS_RELATIVE_URI: &'static str =
        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI";
    pub const AWS_ENDPOINT: &'static str = "AWS_ENDPOINT";
    pub const AWS_ENDPOINT_URL_S3: &'static str = "AWS_ENDPOINT_URL_S3";
    pub const AWS_ENDPOINT_URL_STS: &'static str = "AWS_ENDPOINT_URL_STS";
    pub const AWS_IMDSV1_FALLBACK: &'static str = "AWS_IMDSV1_FALLBACK";
    pub const AWS_METADATA_ENDPOINT: &'static str = "AWS_METADATA_ENDPOINT";
    pub const AWS_ROLE_ARN: &'static str = "AWS_ROLE_ARN";
    pub const AWS_ROLE_SESSION_NAME: &'static str = "AWS_ROLE_SESSION_NAME";
    pub const AWS_SECRET_ACCESS_KEY: &'static str = "AWS_SECRET_ACCESS_KEY";
    pub const AWS_SESSION_TOKEN: &'static str = "AWS_SESSION_TOKEN";
    pub const AWS_WEB_IDENTITY_TOKEN_FILE: &'static str = "AWS_WEB_IDENTITY_TOKEN_FILE";
    pub const DAYTONA_API_KEY: &'static str = "DAYTONA_API_KEY";
    pub const DAYTONA_API_URL: &'static str = "DAYTONA_API_URL";
    pub const DAYTONA_SERVER_URL: &'static str = "DAYTONA_SERVER_URL";
    pub const SESSION_SECRET: &'static str = "SESSION_SECRET";

    // Platform and test harness
    pub const CARGO_BIN_EXE_FABRO: &'static str = "CARGO_BIN_EXE_fabro";
    pub const CARGO_CFG_TARGET_OS: &'static str = "CARGO_CFG_TARGET_OS";
    pub const CARGO_HOME: &'static str = "CARGO_HOME";
    pub const CARGO_MANIFEST_DIR: &'static str = "CARGO_MANIFEST_DIR";
    pub const CI: &'static str = "CI";
    pub const HOME: &'static str = "HOME";
    pub const KUBERNETES_SERVICE_HOST: &'static str = "KUBERNETES_SERVICE_HOST";
    pub const LANG: &'static str = "LANG";
    pub const LLVM_PROFILE_FILE: &'static str = "LLVM_PROFILE_FILE";
    pub const NEXTEST_PROFILE: &'static str = "NEXTEST_PROFILE";
    pub const NEXTEST_RUN_ID: &'static str = "NEXTEST_RUN_ID";
    pub const NO_COLOR: &'static str = "NO_COLOR";
    pub const NVM_DIR: &'static str = "NVM_DIR";
    pub const OUT_DIR: &'static str = "OUT_DIR";
    pub const PATH: &'static str = "PATH";
    pub const PATHEXT: &'static str = "PATHEXT";
    pub const PROFILE: &'static str = "PROFILE";
    pub const RAILWAY_ENVIRONMENT: &'static str = "RAILWAY_ENVIRONMENT";
    pub const RAILWAY_PUBLIC_DOMAIN: &'static str = "RAILWAY_PUBLIC_DOMAIN";
    pub const RUST_BACKTRACE: &'static str = "RUST_BACKTRACE";
    pub const RUST_LOG: &'static str = "RUST_LOG";
    pub const SHELL: &'static str = "SHELL";
    pub const TERM: &'static str = "TERM";
    pub const TMPDIR: &'static str = "TMPDIR";
    pub const TWIN_OPENAI_BIND_ADDR: &'static str = "TWIN_OPENAI_BIND_ADDR";
    pub const TWIN_OPENAI_ENABLE_ADMIN: &'static str = "TWIN_OPENAI_ENABLE_ADMIN";
    pub const TWIN_OPENAI_LIVE_BASE_URL: &'static str = "TWIN_OPENAI_LIVE_BASE_URL";
    pub const TWIN_OPENAI_LIVE_MODEL: &'static str = "TWIN_OPENAI_LIVE_MODEL";
    pub const TWIN_OPENAI_REQUIRE_AUTH: &'static str = "TWIN_OPENAI_REQUIRE_AUTH";
    pub const USER: &'static str = "USER";
    pub const ZDOTDIR: &'static str = "ZDOTDIR";
}

#[cfg(test)]
mod tests {
    use super::EnvVars;

    #[test]
    fn env_var_constants_match_their_names() {
        assert_eq!(EnvVars::FABRO_CONFIG, "FABRO_CONFIG");
    }

    #[test]
    fn env_var_constants_are_non_empty_and_single_tokens() {
        let values = [
            EnvVars::FABRO_AUTH_FILE,
            EnvVars::FABRO_BUILD_DATE,
            EnvVars::FABRO_BUILD_PROFILE,
            EnvVars::FABRO_BUILD_PROFILE_SUFFIX,
            EnvVars::FABRO_CONFIG,
            EnvVars::FABRO_DEBUG,
            EnvVars::FABRO_DEV_TOKEN,
            EnvVars::FABRO_ENABLE_FETCH_FEATURE_OCI_INTEGRATION,
            EnvVars::FABRO_GIT_SHA,
            EnvVars::FABRO_HOME,
            EnvVars::FABRO_HTTP_PROXY_POLICY,
            EnvVars::FABRO_JSON,
            EnvVars::FABRO_NO_UPGRADE_CHECK,
            EnvVars::FABRO_QUIET,
            EnvVars::FABRO_SERVER,
            EnvVars::FABRO_SERVER_MAX_CONCURRENT_RUNS,
            EnvVars::FABRO_SLACK_APP_TOKEN,
            EnvVars::FABRO_SLACK_BOT_TOKEN,
            EnvVars::FABRO_STORAGE_DIR,
            EnvVars::FABRO_STORAGE_ROOT,
            EnvVars::FABRO_SUPPRESS_OPEN_BROWSER,
            EnvVars::FABRO_TELEMETRY,
            EnvVars::FABRO_TEST_IN_MEMORY_STORE,
            EnvVars::FABRO_TEST_MODE,
            EnvVars::FABRO_VERBOSE,
            EnvVars::FABRO_WEB_URL,
            EnvVars::FABRO_WORKER_TOKEN,
            EnvVars::ANTHROPIC_API_KEY,
            EnvVars::ANTHROPIC_BASE_URL,
            EnvVars::BRAVE_SEARCH_API_KEY,
            EnvVars::CHATGPT_ACCOUNT_ID,
            EnvVars::GEMINI_API_KEY,
            EnvVars::GEMINI_BASE_URL,
            EnvVars::GOOGLE_API_KEY,
            EnvVars::GOPATH,
            EnvVars::INCEPTION_API_KEY,
            EnvVars::KIMI_API_KEY,
            EnvVars::MINIMAX_API_KEY,
            EnvVars::OPENAI_API_KEY,
            EnvVars::OPENAI_BASE_URL,
            EnvVars::OPENAI_COMPATIBLE_BASE_URL,
            EnvVars::OPENAI_ORGANIZATION,
            EnvVars::OPENAI_PROJECT,
            EnvVars::OPENAI_ORG_ID,
            EnvVars::OPENAI_PROJECT_ID,
            EnvVars::ZAI_API_KEY,
            EnvVars::GH_TOKEN,
            EnvVars::GITHUB_APP_CLIENT_SECRET,
            EnvVars::GITHUB_APP_PRIVATE_KEY,
            EnvVars::GITHUB_APP_WEBHOOK_SECRET,
            EnvVars::GITHUB_BASE_URL,
            EnvVars::GITHUB_TOKEN,
            EnvVars::OAUTH_CALLBACK_PATH,
            EnvVars::OAUTH_CLIENT_ID,
            EnvVars::OAUTH_ISSUER,
            EnvVars::OAUTH_PORT,
            EnvVars::OAUTH_SCOPE,
            EnvVars::SLACK_BASE_URL,
            EnvVars::AWS_ACCESS_KEY_ID,
            EnvVars::AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE,
            EnvVars::AWS_CONTAINER_CREDENTIALS_FULL_URI,
            EnvVars::AWS_CONTAINER_CREDENTIALS_RELATIVE_URI,
            EnvVars::AWS_ENDPOINT,
            EnvVars::AWS_ENDPOINT_URL_S3,
            EnvVars::AWS_ENDPOINT_URL_STS,
            EnvVars::AWS_IMDSV1_FALLBACK,
            EnvVars::AWS_METADATA_ENDPOINT,
            EnvVars::AWS_ROLE_ARN,
            EnvVars::AWS_ROLE_SESSION_NAME,
            EnvVars::AWS_SECRET_ACCESS_KEY,
            EnvVars::AWS_SESSION_TOKEN,
            EnvVars::AWS_WEB_IDENTITY_TOKEN_FILE,
            EnvVars::DAYTONA_API_KEY,
            EnvVars::DAYTONA_API_URL,
            EnvVars::DAYTONA_SERVER_URL,
            EnvVars::SESSION_SECRET,
            EnvVars::CARGO_BIN_EXE_FABRO,
            EnvVars::CARGO_CFG_TARGET_OS,
            EnvVars::CARGO_HOME,
            EnvVars::CARGO_MANIFEST_DIR,
            EnvVars::CI,
            EnvVars::HOME,
            EnvVars::KUBERNETES_SERVICE_HOST,
            EnvVars::LANG,
            EnvVars::LLVM_PROFILE_FILE,
            EnvVars::NEXTEST_PROFILE,
            EnvVars::NEXTEST_RUN_ID,
            EnvVars::NO_COLOR,
            EnvVars::NVM_DIR,
            EnvVars::OUT_DIR,
            EnvVars::PATH,
            EnvVars::PATHEXT,
            EnvVars::PROFILE,
            EnvVars::RAILWAY_ENVIRONMENT,
            EnvVars::RAILWAY_PUBLIC_DOMAIN,
            EnvVars::RUST_BACKTRACE,
            EnvVars::RUST_LOG,
            EnvVars::SHELL,
            EnvVars::TERM,
            EnvVars::TMPDIR,
            EnvVars::TWIN_OPENAI_BIND_ADDR,
            EnvVars::TWIN_OPENAI_ENABLE_ADMIN,
            EnvVars::TWIN_OPENAI_LIVE_BASE_URL,
            EnvVars::TWIN_OPENAI_LIVE_MODEL,
            EnvVars::TWIN_OPENAI_REQUIRE_AUTH,
            EnvVars::USER,
            EnvVars::ZDOTDIR,
        ];

        for value in values {
            assert!(!value.is_empty());
            assert!(!value.chars().any(char::is_whitespace));
        }
    }
}
