pub(crate) fn redact_auth_url(
    text: &str,
    auth_url: Option<&fabro_redact::DisplaySafeUrl>,
) -> String {
    let Some(auth_url) = auth_url else {
        return text.to_string();
    };
    text.replace(&auth_url.raw_string(), &auth_url.redacted_string())
}

pub(crate) fn classify_credential_refresh_failure(inner: &str) -> &'static str {
    let lower = inner.to_ascii_lowercase();
    if lower.contains("set_url_exec_failed")
        || lower.contains("execute command")
        || lower.contains("failed to execute")
    {
        "set_url_exec_failed"
    } else if lower.contains("set_url_nonzero")
        || lower.contains("remote set-url")
        || lower.contains("set refreshed push credentials")
        || lower.contains("exit ")
    {
        "set_url_nonzero"
    } else if lower.contains("token_mint_failed")
        || lower.contains("github app")
        || lower.contains("installation")
        || lower.contains("private key")
        || lower.contains("token")
    {
        "token_mint_failed"
    } else {
        "unclassified"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_credential_refresh_failure_documents_known_branches() {
        assert_eq!(
            classify_credential_refresh_failure("GitHub App installation token request failed"),
            "token_mint_failed"
        );
        assert_eq!(
            classify_credential_refresh_failure("set_url_exec_failed: sdk echoed command argv"),
            "set_url_exec_failed"
        );
        assert_eq!(
            classify_credential_refresh_failure("git remote set-url origin failed with exit 128"),
            "set_url_nonzero"
        );
        assert_eq!(
            classify_credential_refresh_failure("new provider error"),
            "unclassified"
        );
    }
}
