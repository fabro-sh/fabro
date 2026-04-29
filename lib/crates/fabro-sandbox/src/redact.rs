pub(crate) fn redact_auth_url(
    text: &str,
    auth_url: Option<&fabro_redact::DisplaySafeUrl>,
) -> String {
    let Some(auth_url) = auth_url else {
        return text.to_string();
    };
    text.replace(&auth_url.raw_string(), &auth_url.redacted_string())
}
