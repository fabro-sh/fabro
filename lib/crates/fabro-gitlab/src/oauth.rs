use serde::{Deserialize, Serialize};
use url::Url;

use crate::repository::GitLabBaseUrl;

#[derive(Debug, Clone, Deserialize)]
pub struct GitLabUser {
    pub id:       u64,
    pub username: String,
    pub name:     Option<String>,
    pub email:    Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitLabGroup {
    pub id:        u64,
    pub full_path: String,
}

#[derive(Serialize)]
pub struct TokenExchangeRequest<'a> {
    pub client_id:     &'a str,
    pub client_secret: &'a str,
    pub code:          &'a str,
    pub grant_type:    &'static str,
    pub redirect_uri:  &'a str,
}

#[derive(Deserialize)]
pub struct TokenExchangeResponse {
    pub access_token: String,
    pub token_type:   String,
    pub scope:        Option<String>,
}

#[must_use]
pub fn authorize_url(
    base: &GitLabBaseUrl,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
) -> Url {
    let mut url = base.oauth_url("authorize");
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", "read_user read_api")
        .append_pair("state", state);
    url
}

#[must_use]
pub fn token_url(base: &GitLabBaseUrl) -> Url {
    base.oauth_url("token")
}

#[must_use]
pub fn user_url(base: &GitLabBaseUrl) -> Url {
    base.api_url("user")
}

#[must_use]
pub fn groups_url(base: &GitLabBaseUrl, page: u32) -> Url {
    let mut url = base.api_url("groups");
    url.query_pairs_mut()
        .append_pair("page", &page.to_string())
        .append_pair("per_page", "100");
    url
}

#[must_use]
pub fn group_member_url(base: &GitLabBaseUrl, group_path: &str, user_id: u64) -> Url {
    base.api_url(&format!(
        "groups/{}/members/all/{user_id}",
        crate::repository::encode_path_segment(group_path)
    ))
}
