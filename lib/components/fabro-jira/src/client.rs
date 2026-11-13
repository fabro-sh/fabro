use std::time::Duration;

use fabro_http::{HttpClient, HttpClientBuildError, HttpError};
use serde::Deserialize;
use serde_json::json;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SEARCH_PAGE_SIZE: &str = "50";
const COMMENT_PAGE_SIZE: usize = 100;
const ISSUE_FIELDS: &str = "summary,description,status,issuetype,priority,labels,comment";

#[derive(Debug, thiserror::Error)]
pub enum JiraError {
    #[error("Jira request failed")]
    Http(#[source] HttpError),
    #[error("Jira returned HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("Jira response could not be parsed")]
    Parse(#[source] serde_json::Error),
    #[error("Jira issue {key} has no transition named {status_name:?}; available: {available:?}")]
    TransitionNotFound {
        key:         String,
        status_name: String,
        available:   Vec<String>,
    },
}

impl JiraError {
    #[must_use]
    pub fn is_auth(&self) -> bool {
        matches!(self, Self::Status {
            status: 401 | 403,
            ..
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPage {
    pub keys:            Vec<String>,
    pub next_page_token: Option<String>,
    pub is_last:         bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraIssue {
    pub key:         String,
    pub summary:     String,
    pub description: Option<String>,
    pub status:      String,
    pub issue_type:  Option<String>,
    pub priority:    Option<String>,
    pub labels:      Vec<String>,
    pub comments:    Vec<JiraComment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraComment {
    pub author:  String,
    pub created: String,
    pub body:    String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchResponse {
    #[serde(default)]
    issues:          Vec<KeyOnly>,
    next_page_token: Option<String>,
    #[serde(default)]
    is_last:         bool,
}

#[derive(Deserialize)]
struct KeyOnly {
    key: String,
}

#[derive(Deserialize)]
struct IssueResponse {
    key:    String,
    fields: Fields,
}

#[derive(Deserialize)]
struct Fields {
    #[serde(default)]
    summary:     String,
    description: Option<String>,
    status:      Option<Named>,
    issuetype:   Option<Named>,
    priority:    Option<Named>,
    #[serde(default)]
    labels:      Vec<String>,
    comment:     Option<CommentPage>,
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

#[derive(Deserialize, Default)]
struct CommentPage {
    #[serde(default)]
    total:    usize,
    #[serde(default)]
    comments: Vec<RawComment>,
}

#[derive(Deserialize)]
struct RawComment {
    author:  Option<Author>,
    #[serde(default)]
    created: String,
    #[serde(default)]
    body:    String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Author {
    #[serde(default)]
    display_name: String,
}

impl From<RawComment> for JiraComment {
    fn from(raw: RawComment) -> Self {
        Self {
            author:  raw
                .author
                .map(|a| a.display_name)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "unknown".to_string()),
            created: raw.created,
            body:    raw.body,
        }
    }
}

#[derive(Deserialize)]
struct TransitionsResponse {
    #[serde(default)]
    transitions: Vec<Transition>,
}

#[derive(Deserialize)]
struct Transition {
    id:   String,
    name: String,
    to:   Option<Named>,
}

pub struct JiraClient {
    site_url: String,
    email:    String,
    token:    String,
    http:     HttpClient,
}

impl JiraClient {
    pub fn new(
        site_url: String,
        email: String,
        api_token: String,
    ) -> Result<Self, HttpClientBuildError> {
        Ok(Self::with_http(
            site_url,
            email,
            api_token,
            fabro_http::http_client()?,
        ))
    }

    #[must_use]
    pub fn with_http(
        mut site_url: String,
        email: String,
        api_token: String,
        http: HttpClient,
    ) -> Self {
        site_url.truncate(site_url.trim_end_matches('/').len());
        Self {
            site_url,
            email,
            token: api_token,
            http,
        }
    }

    #[must_use]
    pub fn site_url(&self) -> &str {
        &self.site_url
    }

    pub async fn search_keys(
        &self,
        jql: &str,
        next_page_token: Option<&str>,
    ) -> Result<SearchPage, JiraError> {
        let mut query = vec![
            ("jql", jql),
            ("maxResults", SEARCH_PAGE_SIZE),
            ("fields", "key"),
        ];
        if let Some(token) = next_page_token {
            query.push(("nextPageToken", token));
        }
        let request = self
            .request(Method::Get, "/rest/api/2/search/jql")
            .query(&query);
        let page: SearchResponse = self.send_json(request).await?;
        Ok(SearchPage {
            keys:            page.issues.into_iter().map(|i| i.key).collect(),
            next_page_token: page.next_page_token,
            is_last:         page.is_last,
        })
    }

    pub async fn get_issue(&self, key: &str) -> Result<JiraIssue, JiraError> {
        let raw: IssueResponse = self
            .send_json(
                self.request(Method::Get, &format!("/rest/api/2/issue/{key}"))
                    .query(&[("fields", ISSUE_FIELDS)]),
            )
            .await?;
        let fields = raw.fields;
        let embedded = fields.comment.unwrap_or_default();
        let total = embedded.total;
        let mut comments: Vec<JiraComment> =
            embedded.comments.into_iter().map(Into::into).collect();
        while comments.len() < total {
            let start_at = comments.len().to_string();
            let max_results = COMMENT_PAGE_SIZE.to_string();
            let request = self
                .request(Method::Get, &format!("/rest/api/2/issue/{key}/comment"))
                .query(&[
                    ("startAt", start_at.as_str()),
                    ("maxResults", max_results.as_str()),
                ]);
            let page: CommentPage = self.send_json(request).await?;
            if page.comments.is_empty() {
                break;
            }
            comments.extend(page.comments.into_iter().map(Into::into));
        }
        Ok(JiraIssue {
            key: raw.key,
            summary: fields.summary,
            description: fields.description,
            status: fields.status.map_or_else(String::new, |s| s.name),
            issue_type: fields.issuetype.map(|t| t.name),
            priority: fields.priority.map(|p| p.name),
            labels: fields.labels,
            comments,
        })
    }

    pub async fn add_comment(&self, key: &str, body: &str) -> Result<(), JiraError> {
        let request = self
            .request(Method::Post, &format!("/rest/api/2/issue/{key}/comment"))
            .json(&json!({ "body": body }));
        self.send(request).await.map(drop)
    }

    pub async fn transition(&self, key: &str, status_name: &str) -> Result<(), JiraError> {
        let path = format!("/rest/api/2/issue/{key}/transitions");
        let available: TransitionsResponse =
            self.send_json(self.request(Method::Get, &path)).await?;
        let wanted = status_name.to_lowercase();
        let Some(found) = available.transitions.iter().find(|t| {
            t.name.to_lowercase() == wanted
                || t.to
                    .as_ref()
                    .is_some_and(|to| to.name.to_lowercase() == wanted)
        }) else {
            return Err(JiraError::TransitionNotFound {
                key:         key.to_string(),
                status_name: status_name.to_string(),
                available:   available.transitions.into_iter().map(|t| t.name).collect(),
            });
        };
        let request = self
            .request(Method::Post, &path)
            .json(&json!({ "transition": { "id": found.id } }));
        self.send(request).await.map(drop)
    }

    fn request(&self, method: Method, path: &str) -> fabro_http::RequestBuilder {
        let url = format!("{}{path}", self.site_url);
        let builder = match method {
            Method::Get => self.http.get(url),
            Method::Post => self.http.post(url),
        };
        builder
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .header("User-Agent", "fabro")
            .timeout(REQUEST_TIMEOUT)
    }

    async fn send(&self, request: fabro_http::RequestBuilder) -> Result<String, JiraError> {
        let response = request.send().await.map_err(JiraError::Http)?;
        let status = response.status();
        let body = response.text().await.map_err(JiraError::Http)?;
        if status.is_success() {
            Ok(body)
        } else {
            Err(JiraError::Status {
                status: status.as_u16(),
                body,
            })
        }
    }

    async fn send_json<T: for<'de> Deserialize<'de>>(
        &self,
        request: fabro_http::RequestBuilder,
    ) -> Result<T, JiraError> {
        let body = self.send(request).await?;
        serde_json::from_str(&body).map_err(JiraError::Parse)
    }
}

#[derive(Clone, Copy)]
enum Method {
    Get,
    Post,
}

#[cfg(test)]
mod tests {
    use httpmock::prelude::*;
    use serde_json::json;

    use super::*;

    fn client(server: &MockServer) -> JiraClient {
        JiraClient::with_http(
            format!("{}/", server.base_url()),
            "me@example.com".into(),
            "tok".into(),
            fabro_http::test_http_client().unwrap(),
        )
    }

    #[tokio::test]
    async fn search_keys_pages_with_next_page_token() {
        let server = MockServer::start();
        let first = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/2/search/jql")
                .query_param("jql", "project=ABC")
                .query_param("maxResults", "50")
                .query_param("fields", "key")
                .query_param_missing("nextPageToken");
            then.json_body(json!({
                "issues": [{"key": "ABC-1"}, {"key": "ABC-2"}],
                "nextPageToken": "t2",
                "isLast": false
            }));
        });
        let second = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/2/search/jql")
                .query_param("nextPageToken", "t2");
            then.json_body(json!({"issues": [{"key": "ABC-3"}], "isLast": true}));
        });
        let client = client(&server);
        let page = client.search_keys("project=ABC", None).await.unwrap();
        assert_eq!(page.keys, ["ABC-1", "ABC-2"]);
        assert_eq!(page.next_page_token.as_deref(), Some("t2"));
        assert!(!page.is_last);
        let page = client.search_keys("project=ABC", Some("t2")).await.unwrap();
        assert_eq!(page.keys, ["ABC-3"]);
        assert!(page.is_last);
        first.assert();
        second.assert();
    }

    #[tokio::test]
    async fn get_issue_maps_fields_and_pages_comments() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/2/issue/ABC-1")
                .query_param(
                    "fields",
                    "summary,description,status,issuetype,priority,labels,comment",
                );
            then.json_body(json!({
                "key": "ABC-1",
                "fields": {
                    "summary": "Fix login",
                    "description": "Broken",
                    "status": {"name": "To Do"},
                    "issuetype": {"name": "Bug"},
                    "priority": {"name": "High"},
                    "labels": ["auth"],
                    "comment": {"total": 2, "maxResults": 1, "comments": [
                        {"author": {"displayName": "alice"}, "created": "c1", "body": "first"}
                    ]}
                }
            }));
        });
        let rest = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/2/issue/ABC-1/comment")
                .query_param("startAt", "1")
                .query_param("maxResults", "100");
            then.json_body(json!({"total": 2, "comments": [
                {"author": {"displayName": "bob"}, "created": "c2", "body": "second"},
                {"author": {"accountId": "x"}, "created": "c3", "body": "third"}
            ]}));
        });
        let issue = client(&server).get_issue("ABC-1").await.unwrap();
        rest.assert();
        assert_eq!(issue.summary, "Fix login");
        assert_eq!(issue.description.as_deref(), Some("Broken"));
        assert_eq!(issue.status, "To Do");
        assert_eq!(issue.issue_type.as_deref(), Some("Bug"));
        assert_eq!(issue.priority.as_deref(), Some("High"));
        assert_eq!(issue.labels, ["auth"]);
        assert_eq!(issue.comments.len(), 3);
        assert_eq!(issue.comments[2].author, "unknown");
        assert_eq!(issue.comments[0].author, "alice");
        assert_eq!(issue.comments[1].author, "bob");
        assert_eq!(issue.comments[1].created, "c2");
        assert_eq!(issue.comments[1].body, "second");
    }

    fn transitions_mock(server: &MockServer) {
        server.mock(|when, then| {
            when.method(GET).path("/rest/api/2/issue/ABC-1/transitions");
            then.json_body(json!({"transitions": [
                {"id": "31", "name": "In Progress", "to": {"name": "In Progress"}}
            ]}));
        });
    }

    #[tokio::test]
    async fn transition_resolves_name_to_id_case_insensitively() {
        let server = MockServer::start();
        transitions_mock(&server);
        let post = server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/2/issue/ABC-1/transitions")
                .json_body(json!({"transition": {"id": "31"}}));
            then.status(204);
        });
        client(&server)
            .transition("ABC-1", "in progress")
            .await
            .unwrap();
        post.assert();
    }

    #[tokio::test]
    async fn transition_unknown_name_is_transition_not_found() {
        let server = MockServer::start();
        transitions_mock(&server);
        let post = server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/2/issue/ABC-1/transitions");
            then.status(204);
        });
        let err = client(&server)
            .transition("ABC-1", "Done")
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            JiraError::TransitionNotFound { ref available, .. } if available == &["In Progress"]
        ));
        post.assert_calls(0);
    }

    #[tokio::test]
    async fn status_401_is_auth_error() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/rest/api/2/issue/ABC-1");
            then.status(401).body("nope");
        });
        let err = client(&server).get_issue("ABC-1").await.unwrap_err();
        assert!(err.is_auth());
        assert!(matches!(err, JiraError::Status { status: 401, .. }));
    }

    #[tokio::test]
    async fn requests_use_basic_auth() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/2/search/jql")
                .header("Authorization", "Basic bWVAZXhhbXBsZS5jb206dG9r");
            then.json_body(json!({"issues": [], "isLast": true}));
        });
        client(&server).search_keys("x", None).await.unwrap();
        mock.assert();
    }
}
