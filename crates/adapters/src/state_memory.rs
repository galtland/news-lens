//! In-memory state store for testing and offline mode.

use async_trait::async_trait;
use news_lens_domain::{AccountState, ProcessedPostRecord, StateError, StateStore};
use std::collections::HashMap;
use std::sync::RwLock;

/// In-memory state store implementation.
pub struct InMemoryStateStore {
    accounts: RwLock<HashMap<String, AccountState>>,
    processed: RwLock<HashMap<(String, String), ProcessedPostRecord>>,
}

impl InMemoryStateStore {
    pub fn new() -> Self {
        Self {
            accounts: RwLock::new(HashMap::new()),
            processed: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StateStore for InMemoryStateStore {
    async fn get_account_state(&self, account: &str) -> Result<Option<AccountState>, StateError> {
        let accounts = self
            .accounts
            .read()
            .map_err(|error| StateError::Database(error.to_string()))?;
        Ok(accounts.get(account).cloned())
    }

    async fn set_account_state(&self, state: &AccountState) -> Result<(), StateError> {
        let mut accounts = self
            .accounts
            .write()
            .map_err(|error| StateError::Database(error.to_string()))?;
        accounts.insert(state.account.clone(), state.clone());
        Ok(())
    }

    async fn is_processed(&self, post_id: &str, lens_id: &str) -> Result<bool, StateError> {
        let processed = self
            .processed
            .read()
            .map_err(|error| StateError::Database(error.to_string()))?;
        Ok(processed.contains_key(&(post_id.to_string(), lens_id.to_string())))
    }

    async fn record_processed(&self, record: &ProcessedPostRecord) -> Result<(), StateError> {
        let mut processed = self
            .processed
            .write()
            .map_err(|error| StateError::Database(error.to_string()))?;
        processed.insert(
            (record.post_id.clone(), record.lens_id.clone()),
            record.clone(),
        );
        Ok(())
    }

    async fn get_processed(
        &self,
        post_id: &str,
        lens_id: &str,
    ) -> Result<Option<ProcessedPostRecord>, StateError> {
        let processed = self
            .processed
            .read()
            .map_err(|error| StateError::Database(error.to_string()))?;
        Ok(processed
            .get(&(post_id.to_string(), lens_id.to_string()))
            .cloned())
    }

    async fn list_processed_with_gaps(
        &self,
        lens_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<ProcessedPostRecord>, StateError> {
        let processed = self
            .processed
            .read()
            .map_err(|error| StateError::Database(error.to_string()))?;
        let mut records: Vec<ProcessedPostRecord> = processed
            .values()
            .filter(|r| r.gaps.as_ref().is_some_and(|g| !g.is_empty()))
            .filter(|r| lens_id.is_none_or(|id| r.lens_id == id))
            .cloned()
            .collect();
        records.sort_by(|a, b| b.processed_at.cmp(&a.processed_at));
        if let Some(n) = limit {
            records.truncate(n as usize);
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use news_lens_domain::Stance;
    use time::OffsetDateTime;

    #[tokio::test]
    async fn account_state_roundtrip() {
        let store = InMemoryStateStore::new();

        let state = AccountState {
            account: "testuser".to_string(),
            since_id: Some("12345".to_string()),
            updated_at: OffsetDateTime::now_utc(),
        };

        store.set_account_state(&state).await.unwrap();
        let retrieved = store.get_account_state("testuser").await.unwrap();

        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().since_id, Some("12345".to_string()));
    }

    #[tokio::test]
    async fn processed_record_roundtrip() {
        let store = InMemoryStateStore::new();
        let record = ProcessedPostRecord {
            post_id: "post123".to_string(),
            lens_id: "lens".to_string(),
            processed_at: OffsetDateTime::now_utc(),
            stance: Stance::Critique,
            raw_path: Some("raw/news/post.md".to_string()),
            thesis_slug: Some("post".to_string()),
            x_post_id: Some("xpost789".to_string()),
            nostr_event_id: None,
            gaps: None,
        };

        store.record_processed(&record).await.unwrap();

        assert!(store.is_processed("post123", "lens").await.unwrap());
        assert!(!store.is_processed("post123", "other-lens").await.unwrap());
        assert!(!store.is_processed("other-post", "lens").await.unwrap());

        let retrieved = store.get_processed("post123", "lens").await.unwrap();
        assert_eq!(retrieved.unwrap().x_post_id, Some("xpost789".to_string()));
    }
}
