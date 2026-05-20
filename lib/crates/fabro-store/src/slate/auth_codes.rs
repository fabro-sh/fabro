use std::sync::Arc;

use chrono::{DateTime, Utc};
use fabro_types::IdpIdentity;
use serde::{Deserialize, Serialize};

use crate::record::{JsonCodec, Record, Repository};
use crate::{KeyedMutex, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthCode {
    pub code:           String,
    pub identity:       IdpIdentity,
    pub login:          String,
    pub name:           String,
    pub email:          String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url:     Option<String>,
    pub code_challenge: String,
    pub redirect_uri:   String,
    pub expires_at:     DateTime<Utc>,
}

impl Record for AuthCode {
    type Id = String;
    type Codec = JsonCodec;

    const PREFIX: &'static str = "auth/code";

    fn id(&self) -> Self::Id {
        self.code.clone()
    }
}

pub struct AuthCodeStore {
    repo:          Repository<AuthCode>,
    consume_locks: KeyedMutex<String>,
}

impl std::fmt::Debug for AuthCodeStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthCodeStore").finish_non_exhaustive()
    }
}

impl AuthCodeStore {
    pub(crate) fn new(db: Arc<slatedb::Db>) -> Self {
        Self {
            repo:          Repository::new(db),
            consume_locks: KeyedMutex::new(),
        }
    }

    pub async fn insert(&self, entry: AuthCode) -> Result<()> {
        self.repo.put(&entry).await
    }

    pub async fn consume(&self, code: &str) -> Result<Option<AuthCode>> {
        let code = code.to_string();
        let _guard = self.consume_locks.lock(code.clone()).await;
        let entry = self.repo.get(&code).await?;
        let result = match entry {
            Some(entry) if entry.expires_at > Utc::now() => {
                self.repo.delete(&code).await?;
                Some(entry)
            }
            Some(_) => {
                self.repo.delete(&code).await?;
                None
            }
            None => None,
        };

        Ok(result)
    }

    pub async fn gc_expired(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        self.repo
            .gc(|auth_code| auth_code.expires_at <= cutoff)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::Duration as ChronoDuration;
    use object_store::memory::InMemory;
    use tokio::task::JoinSet;

    use super::{AuthCode, AuthCodeStore};
    use crate::Database;

    async fn store() -> Arc<AuthCodeStore> {
        let db = Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        );
        db.auth_codes().await.unwrap()
    }

    fn auth_code(code: &str, expires_at: chrono::DateTime<chrono::Utc>) -> AuthCode {
        AuthCode {
            code: code.to_string(),
            identity: fabro_types::IdpIdentity::new("https://github.com", "12345").unwrap(),
            login: "octocat".to_string(),
            name: "The Octocat".to_string(),
            email: "octocat@example.com".to_string(),
            avatar_url: None,
            code_challenge: "challenge".to_string(),
            redirect_uri: "http://127.0.0.1/callback".to_string(),
            expires_at,
        }
    }

    #[tokio::test]
    async fn insert_and_consume_is_single_use() {
        let store = store().await;
        store
            .insert(auth_code(
                "code-1",
                chrono::Utc::now() + ChronoDuration::seconds(60),
            ))
            .await
            .unwrap();

        assert!(store.consume("code-1").await.unwrap().is_some());
        assert!(store.consume("code-1").await.unwrap().is_none());
    }

    #[test]
    fn deserializes_legacy_json_without_avatar_url() {
        let entry: AuthCode = serde_json::from_value(serde_json::json!({
            "code": "legacy-code",
            "identity": {
                "issuer": "https://github.com",
                "subject": "12345"
            },
            "login": "octocat",
            "name": "The Octocat",
            "email": "octocat@example.com",
            "code_challenge": "challenge",
            "redirect_uri": "http://127.0.0.1/callback",
            "expires_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap();

        assert_eq!(entry.avatar_url, None);
    }

    #[test]
    fn serializes_avatar_url_when_present() {
        let mut entry = auth_code("avatar-code", chrono::Utc::now());
        entry.avatar_url = Some("https://example.com/octocat.png".to_string());

        let json = serde_json::to_value(&entry).unwrap();

        assert_eq!(json["avatar_url"], "https://example.com/octocat.png");
    }

    #[tokio::test]
    async fn concurrent_consume_has_one_winner() {
        let store = store().await;
        store
            .insert(auth_code(
                "code-2",
                chrono::Utc::now() + ChronoDuration::seconds(60),
            ))
            .await
            .unwrap();

        let mut tasks = JoinSet::new();
        for _ in 0..16 {
            let store = Arc::clone(&store);
            tasks.spawn(async move { store.consume("code-2").await.unwrap().is_some() });
        }

        let mut successes = 0;
        while let Some(result) = tasks.join_next().await {
            if result.unwrap() {
                successes += 1;
            }
        }

        assert_eq!(successes, 1);
    }

    #[tokio::test]
    async fn gc_expired_removes_only_expired_codes() {
        let store = store().await;
        store
            .insert(auth_code(
                "expired",
                chrono::Utc::now() - ChronoDuration::seconds(1),
            ))
            .await
            .unwrap();
        store
            .insert(auth_code(
                "live",
                chrono::Utc::now() + ChronoDuration::seconds(60),
            ))
            .await
            .unwrap();

        assert_eq!(store.gc_expired(chrono::Utc::now()).await.unwrap(), 1);
        assert!(store.consume("expired").await.unwrap().is_none());
        assert!(store.consume("live").await.unwrap().is_some());
    }
}
