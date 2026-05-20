use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use fabro_types::IdpIdentity;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::record::{JsonCodec, Record, Repository, transaction};
use crate::{KeyedMutex, Result};

const REPLAY_REVOCATION_TTL_SECONDS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshToken {
    pub token_hash:   [u8; 32],
    pub chain_id:     Uuid,
    pub identity:     IdpIdentity,
    pub login:        String,
    pub name:         String,
    pub email:        String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url:   Option<String>,
    pub issued_at:    DateTime<Utc>,
    pub expires_at:   DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub used:         bool,
    pub user_agent:   String,
}

impl Record for RefreshToken {
    type Id = [u8; 32];
    type Codec = JsonCodec;

    const PREFIX: &'static str = "auth/refresh";

    fn id(&self) -> Self::Id {
        self.token_hash
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsumeOutcome {
    Rotated(RefreshToken, Box<RefreshToken>),
    Reused(RefreshToken),
    Expired,
    NotFound,
}

pub struct RefreshTokenStore {
    db:                 Arc<slatedb::Db>,
    repo:               Repository<RefreshToken>,
    consume_locks:      KeyedMutex<[u8; 32]>,
    /// In-memory only: persisting attacker-supplied hashes would be an
    /// unbounded-growth surface under a token-stuffing attack.
    replay_revocations: DashMap<[u8; 32], DateTime<Utc>>,
}

impl std::fmt::Debug for RefreshTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshTokenStore").finish_non_exhaustive()
    }
}

impl RefreshTokenStore {
    pub(crate) fn new(db: Arc<slatedb::Db>) -> Self {
        Self {
            repo: Repository::new(Arc::clone(&db)),
            db,
            consume_locks: KeyedMutex::new(),
            replay_revocations: DashMap::new(),
        }
    }

    pub async fn insert_refresh_token(&self, token: RefreshToken) -> Result<()> {
        self.repo.put(&token).await
    }

    pub async fn find_refresh_token(&self, token_hash: &[u8; 32]) -> Result<Option<RefreshToken>> {
        self.repo.get(token_hash).await
    }

    pub async fn active_cli_sessions(
        &self,
        identity: &IdpIdentity,
        now: DateTime<Utc>,
    ) -> Result<Vec<RefreshToken>> {
        let mut active_by_chain = std::collections::HashMap::<Uuid, RefreshToken>::new();
        let mut tokens = self.repo.scan_stream();

        while let Some(result) = tokens.next().await {
            let (_, token) = result?;
            if token.identity != *identity || token.used || token.expires_at <= now {
                continue;
            }

            active_by_chain
                .entry(token.chain_id)
                .and_modify(|current| {
                    if token.last_used_at > current.last_used_at {
                        *current = token.clone();
                    }
                })
                .or_insert(token);
        }

        Ok(active_by_chain.into_values().collect())
    }

    pub async fn consume_and_rotate(
        &self,
        presented_hash: [u8; 32],
        new_token: RefreshToken,
        now: DateTime<Utc>,
    ) -> Result<ConsumeOutcome> {
        let _guard = self.consume_locks.lock(presented_hash).await;

        let outcome = match self.repo.get(&presented_hash).await? {
            None => ConsumeOutcome::NotFound,
            Some(existing) if now >= existing.expires_at => ConsumeOutcome::Expired,
            Some(existing) if existing.used => ConsumeOutcome::Reused(existing),
            Some(existing) => {
                let mut old_token = existing.clone();
                old_token.used = true;
                old_token.last_used_at = now;

                transaction(&self.db, |tx| {
                    tx.put(&old_token)?;
                    tx.put(&new_token)?;
                    Ok(())
                })
                .await?;

                ConsumeOutcome::Rotated(old_token, Box::new(new_token))
            }
        };

        Ok(outcome)
    }

    pub async fn delete_chain(&self, chain_id: Uuid) -> Result<u64> {
        self.repo.gc(|token| token.chain_id == chain_id).await
    }

    pub async fn delete_active_chain_for_identity(
        &self,
        identity: &IdpIdentity,
        chain_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<u64> {
        let mut token_hashes = Vec::new();
        let mut has_active_token = false;
        let mut tokens = self.repo.scan_stream();

        while let Some(result) = tokens.next().await {
            let (_, token) = result?;
            if token.identity != *identity || token.chain_id != chain_id {
                continue;
            }
            if !token.used && token.expires_at > now {
                has_active_token = true;
            }
            token_hashes.push(token.token_hash);
        }

        if !has_active_token {
            return Ok(0);
        }

        let deleted = u64::try_from(token_hashes.len()).unwrap_or(u64::MAX);
        transaction(&self.db, |tx| {
            for token_hash in &token_hashes {
                tx.delete::<RefreshToken>(token_hash)?;
            }
            Ok(())
        })
        .await?;
        Ok(deleted)
    }

    pub async fn gc_expired(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        self.repo.gc(|token| token.expires_at <= cutoff).await
    }

    pub fn mark_refresh_token_replay(&self, token_hash: [u8; 32], now: DateTime<Utc>) {
        self.replay_revocations.insert(
            token_hash,
            now + chrono::Duration::seconds(REPLAY_REVOCATION_TTL_SECONDS),
        );
        self.replay_revocations
            .retain(|_, expires_at| *expires_at > now);
    }

    pub fn was_recently_replay_revoked(&self, token_hash: &[u8; 32], now: DateTime<Utc>) -> bool {
        self.replay_revocations
            .retain(|_, expires_at| *expires_at > now);
        self.replay_revocations
            .get(token_hash)
            .is_some_and(|expires_at| *expires_at > now)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::Duration as ChronoDuration;
    use object_store::memory::InMemory;
    use tokio::task::JoinSet;
    use uuid::Uuid;

    use super::{ConsumeOutcome, RefreshToken, RefreshTokenStore};
    use crate::Database;

    async fn store() -> Arc<RefreshTokenStore> {
        let db = Database::new(
            Arc::new(InMemory::new()),
            "",
            Duration::from_millis(1),
            None,
        );
        db.refresh_tokens().await.unwrap()
    }

    fn refresh_token(hash: [u8; 32], chain_id: Uuid, used: bool) -> RefreshToken {
        let now = chrono::Utc::now();
        RefreshToken {
            token_hash: hash,
            chain_id,
            identity: fabro_types::IdpIdentity::new("https://github.com", "12345").unwrap(),
            login: "octocat".to_string(),
            name: "The Octocat".to_string(),
            email: "octocat@example.com".to_string(),
            avatar_url: None,
            issued_at: now,
            expires_at: now + ChronoDuration::days(30),
            last_used_at: now,
            used,
            user_agent: "fabro-test".to_string(),
        }
    }

    fn alternate_identity() -> fabro_types::IdpIdentity {
        fabro_types::IdpIdentity::new("https://github.com", "67890").unwrap()
    }

    #[tokio::test]
    async fn insert_find_rotate_and_reuse_work() {
        let store = store().await;
        let chain_id = Uuid::new_v4();
        let old_hash = [1_u8; 32];
        let new_hash = [2_u8; 32];
        let old = refresh_token(old_hash, chain_id, false);
        let new = refresh_token(new_hash, chain_id, false);
        store.insert_refresh_token(old.clone()).await.unwrap();

        assert_eq!(
            store.find_refresh_token(&old_hash).await.unwrap(),
            Some(old)
        );

        let rotated = store
            .consume_and_rotate(old_hash, new.clone(), chrono::Utc::now())
            .await
            .unwrap();
        let ConsumeOutcome::Rotated(old_used, new_saved) = rotated else {
            panic!("expected rotation");
        };
        assert!(old_used.used);
        assert_eq!(new_saved.token_hash, new_hash);
        assert_eq!(
            store.find_refresh_token(&old_hash).await.unwrap(),
            Some(old_used.clone())
        );
        assert!(
            store
                .find_refresh_token(&old_hash)
                .await
                .unwrap()
                .expect("rotated old token should still exist")
                .used
        );

        let replay = store
            .consume_and_rotate(
                old_hash,
                refresh_token([3_u8; 32], chain_id, false),
                chrono::Utc::now(),
            )
            .await
            .unwrap();
        let ConsumeOutcome::Reused(reused) = replay else {
            panic!("expected replay to return the original used row");
        };
        assert_eq!(reused.chain_id, chain_id);
    }

    #[test]
    fn deserializes_legacy_json_without_avatar_url() {
        let entry: RefreshToken = serde_json::from_value(serde_json::json!({
            "token_hash": [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
            "chain_id": "00000000-0000-4000-8000-000000000000",
            "identity": {
                "issuer": "https://github.com",
                "subject": "12345"
            },
            "login": "octocat",
            "name": "The Octocat",
            "email": "octocat@example.com",
            "issued_at": "2026-01-01T00:00:00Z",
            "expires_at": "2026-02-01T00:00:00Z",
            "last_used_at": "2026-01-01T00:00:00Z",
            "used": false,
            "user_agent": "fabro-test"
        }))
        .unwrap();

        assert_eq!(entry.avatar_url, None);
    }

    #[test]
    fn serializes_avatar_url_when_present() {
        let mut entry = refresh_token([1_u8; 32], Uuid::new_v4(), false);
        entry.avatar_url = Some("https://example.com/octocat.png".to_string());

        let json = serde_json::to_value(&entry).unwrap();

        assert_eq!(json["avatar_url"], "https://example.com/octocat.png");
    }

    #[tokio::test]
    async fn missing_and_expired_tokens_are_reported() {
        let store = store().await;
        let chain_id = Uuid::new_v4();

        assert_eq!(
            store
                .consume_and_rotate(
                    [7_u8; 32],
                    refresh_token([8_u8; 32], chain_id, false),
                    chrono::Utc::now(),
                )
                .await
                .unwrap(),
            ConsumeOutcome::NotFound
        );

        let mut expired = refresh_token([9_u8; 32], chain_id, false);
        expired.expires_at = chrono::Utc::now() - ChronoDuration::seconds(1);
        store.insert_refresh_token(expired.clone()).await.unwrap();

        assert_eq!(
            store
                .consume_and_rotate(
                    expired.token_hash,
                    refresh_token([10_u8; 32], chain_id, false),
                    chrono::Utc::now(),
                )
                .await
                .unwrap(),
            ConsumeOutcome::Expired
        );
        assert_eq!(
            store.find_refresh_token(&expired.token_hash).await.unwrap(),
            Some(expired)
        );
    }

    #[tokio::test]
    async fn concurrent_rotation_has_one_winner() {
        let store = store().await;
        let chain_id = Uuid::new_v4();
        let hash = [9_u8; 32];
        store
            .insert_refresh_token(refresh_token(hash, chain_id, false))
            .await
            .unwrap();

        let mut tasks = JoinSet::new();
        for idx in 0..16_u8 {
            let store = Arc::clone(&store);
            tasks.spawn(async move {
                store
                    .consume_and_rotate(
                        hash,
                        refresh_token([idx; 32], chain_id, false),
                        chrono::Utc::now(),
                    )
                    .await
                    .unwrap()
            });
        }

        let mut rotated = 0;
        let mut reused = 0;
        while let Some(result) = tasks.join_next().await {
            match result.unwrap() {
                ConsumeOutcome::Rotated(_, _) => rotated += 1,
                ConsumeOutcome::Reused(_) => reused += 1,
                other => panic!("unexpected outcome: {other:?}"),
            }
        }

        assert_eq!(rotated, 1);
        assert_eq!(reused, 15);
    }

    #[tokio::test]
    async fn delete_chain_removes_all_matching_tokens() {
        let store = store().await;
        let chain_id = Uuid::new_v4();
        store
            .insert_refresh_token(refresh_token([1_u8; 32], chain_id, false))
            .await
            .unwrap();
        store
            .insert_refresh_token(refresh_token([2_u8; 32], chain_id, true))
            .await
            .unwrap();

        assert_eq!(store.delete_chain(chain_id).await.unwrap(), 2);
        assert!(
            store
                .find_refresh_token(&[1_u8; 32])
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .find_refresh_token(&[2_u8; 32])
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn delete_active_chain_for_identity_requires_active_owned_token() {
        let store = store().await;
        let identity = fabro_types::IdpIdentity::new("https://github.com", "12345").unwrap();
        let chain_id = Uuid::new_v4();
        let other_chain_id = Uuid::new_v4();
        let active = refresh_token([1_u8; 32], chain_id, false);
        let used = refresh_token([2_u8; 32], chain_id, true);
        let mut other_identity = refresh_token([3_u8; 32], chain_id, false);
        other_identity.identity = alternate_identity();
        let other_chain = refresh_token([4_u8; 32], other_chain_id, false);

        for token in [
            active.clone(),
            used.clone(),
            other_identity.clone(),
            other_chain.clone(),
        ] {
            store.insert_refresh_token(token).await.unwrap();
        }

        assert_eq!(
            store
                .delete_active_chain_for_identity(&identity, chain_id, chrono::Utc::now())
                .await
                .unwrap(),
            2
        );
        assert!(
            store
                .find_refresh_token(&active.token_hash)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .find_refresh_token(&used.token_hash)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .find_refresh_token(&other_identity.token_hash)
                .await
                .unwrap(),
            Some(other_identity)
        );
        assert_eq!(
            store
                .find_refresh_token(&other_chain.token_hash)
                .await
                .unwrap(),
            Some(other_chain)
        );
    }

    #[tokio::test]
    async fn delete_active_chain_for_identity_returns_zero_without_active_token() {
        let store = store().await;
        let identity = fabro_types::IdpIdentity::new("https://github.com", "12345").unwrap();
        let chain_id = Uuid::new_v4();
        let used = refresh_token([1_u8; 32], chain_id, true);
        store.insert_refresh_token(used.clone()).await.unwrap();

        assert_eq!(
            store
                .delete_active_chain_for_identity(&identity, chain_id, chrono::Utc::now())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            store.find_refresh_token(&used.token_hash).await.unwrap(),
            Some(used)
        );
    }

    #[tokio::test]
    async fn active_cli_sessions_return_newest_active_token_per_chain_for_identity() {
        let store = store().await;
        let identity = fabro_types::IdpIdentity::new("https://github.com", "12345").unwrap();
        let now = chrono::Utc::now();
        let duplicate_chain_id = Uuid::new_v4();
        let other_chain_id = Uuid::new_v4();

        let mut old_duplicate = refresh_token([1_u8; 32], duplicate_chain_id, false);
        old_duplicate.last_used_at = now - ChronoDuration::minutes(10);
        old_duplicate.issued_at = now - ChronoDuration::minutes(20);
        let mut newest_duplicate = refresh_token([2_u8; 32], duplicate_chain_id, false);
        newest_duplicate.last_used_at = now - ChronoDuration::minutes(1);
        newest_duplicate.issued_at = now - ChronoDuration::minutes(15);
        let mut other_active = refresh_token([3_u8; 32], other_chain_id, false);
        other_active.last_used_at = now - ChronoDuration::minutes(3);

        store.insert_refresh_token(old_duplicate).await.unwrap();
        store
            .insert_refresh_token(newest_duplicate.clone())
            .await
            .unwrap();
        store
            .insert_refresh_token(other_active.clone())
            .await
            .unwrap();

        let sessions = store.active_cli_sessions(&identity, now).await.unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&newest_duplicate));
        assert!(sessions.contains(&other_active));
    }

    #[tokio::test]
    async fn active_cli_sessions_exclude_expired_used_and_other_identity_tokens() {
        let store = store().await;
        let identity = fabro_types::IdpIdentity::new("https://github.com", "12345").unwrap();
        let now = chrono::Utc::now();

        let active = refresh_token([1_u8; 32], Uuid::new_v4(), false);
        let mut expired = refresh_token([2_u8; 32], Uuid::new_v4(), false);
        expired.expires_at = now - ChronoDuration::seconds(1);
        let used = refresh_token([3_u8; 32], Uuid::new_v4(), true);
        let mut other_identity = refresh_token([4_u8; 32], Uuid::new_v4(), false);
        other_identity.identity = alternate_identity();

        for token in [active.clone(), expired, used, other_identity] {
            store.insert_refresh_token(token).await.unwrap();
        }

        let sessions = store.active_cli_sessions(&identity, now).await.unwrap();
        assert_eq!(sessions, vec![active]);
    }

    #[tokio::test]
    async fn gc_expired_removes_only_tokens_at_or_before_cutoff() {
        let store = store().await;
        let chain_id = Uuid::new_v4();

        let mut expired = refresh_token([4_u8; 32], chain_id, true);
        expired.expires_at = chrono::Utc::now() - ChronoDuration::days(8);
        let live = refresh_token([5_u8; 32], chain_id, false);

        store.insert_refresh_token(expired.clone()).await.unwrap();
        store.insert_refresh_token(live.clone()).await.unwrap();

        assert_eq!(store.gc_expired(chrono::Utc::now()).await.unwrap(), 1);
        assert!(
            store
                .find_refresh_token(&expired.token_hash)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store.find_refresh_token(&live.token_hash).await.unwrap(),
            Some(live)
        );
    }
}
