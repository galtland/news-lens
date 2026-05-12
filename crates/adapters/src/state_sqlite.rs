//! SQLite state store implementation.

use async_trait::async_trait;
use news_lens_domain::{AccountState, ProcessedPostRecord, StateError, StateStore};
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::path::Path;
use time::OffsetDateTime;

/// SQLite-backed state store.
pub struct SqliteStateStore {
    pool: SqlitePool,
}

impl SqliteStateStore {
    /// Create a new SQLite state store, initializing the database if needed.
    pub async fn new(db_path: impl AsRef<Path>) -> Result<Self, StateError> {
        let db_path = db_path.as_ref();

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| StateError::Database(error.to_string()))?;
        }

        let db_url = format!("sqlite:{}?mode=rwc", db_path.display());

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&db_url)
            .await
            .map_err(|error| StateError::Database(error.to_string()))?;

        let store = Self { pool };
        store.run_migrations().await?;

        Ok(store)
    }

    /// Create an in-memory SQLite store for tests.
    pub async fn in_memory() -> Result<Self, StateError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .map_err(|error| StateError::Database(error.to_string()))?;

        let store = Self { pool };
        store.run_migrations().await?;

        Ok(store)
    }

    async fn run_migrations(&self) -> Result<(), StateError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS processed_posts (
                post_id        TEXT PRIMARY KEY,
                lens_id        TEXT NOT NULL,
                processed_at   TEXT NOT NULL,
                stance         TEXT NOT NULL,
                raw_path       TEXT,
                thesis_slug    TEXT,
                x_post_id      TEXT,
                nostr_event_id TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|error| StateError::Database(error.to_string()))?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS account_state (
                account    TEXT PRIMARY KEY,
                since_id   TEXT,
                updated_at TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|error| StateError::Database(error.to_string()))?;

        Ok(())
    }

    #[cfg(test)]
    async fn table_columns(&self, table: &str) -> Result<Vec<String>, StateError> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM pragma_table_info(?) ORDER BY cid")
                .bind(table)
                .fetch_all(&self.pool)
                .await
                .map_err(|error| StateError::Database(error.to_string()))?;
        Ok(rows.into_iter().map(|(name,)| name).collect())
    }
}

#[async_trait]
impl StateStore for SqliteStateStore {
    async fn get_account_state(&self, account: &str) -> Result<Option<AccountState>, StateError> {
        let row: Option<(String, Option<String>, String)> = sqlx::query_as(
            "SELECT account, since_id, updated_at FROM account_state WHERE account = ?",
        )
        .bind(account)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| StateError::Database(error.to_string()))?;

        match row {
            Some((account, since_id, updated_at)) => Ok(Some(AccountState {
                account,
                since_id,
                updated_at: parse_time(&updated_at)?,
            })),
            None => Ok(None),
        }
    }

    async fn set_account_state(&self, state: &AccountState) -> Result<(), StateError> {
        let updated_at = format_time(state.updated_at)?;

        sqlx::query(
            r#"
            INSERT INTO account_state (account, since_id, updated_at)
            VALUES (?, ?, ?)
            ON CONFLICT(account) DO UPDATE SET
                since_id = excluded.since_id,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(&state.account)
        .bind(&state.since_id)
        .bind(&updated_at)
        .execute(&self.pool)
        .await
        .map_err(|error| StateError::Database(error.to_string()))?;

        Ok(())
    }

    async fn is_processed(&self, post_id: &str) -> Result<bool, StateError> {
        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM processed_posts WHERE post_id = ?")
                .bind(post_id)
                .fetch_one(&self.pool)
                .await
                .map_err(|error| StateError::Database(error.to_string()))?;

        Ok(count.0 > 0)
    }

    async fn record_processed(&self, record: &ProcessedPostRecord) -> Result<(), StateError> {
        let processed_at = format_time(record.processed_at)?;

        sqlx::query(
            r#"
            INSERT INTO processed_posts
            (post_id, lens_id, processed_at, stance, raw_path, thesis_slug, x_post_id, nostr_event_id)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(post_id) DO UPDATE SET
                lens_id = excluded.lens_id,
                processed_at = excluded.processed_at,
                stance = excluded.stance,
                raw_path = excluded.raw_path,
                thesis_slug = excluded.thesis_slug,
                x_post_id = excluded.x_post_id,
                nostr_event_id = excluded.nostr_event_id
            "#,
        )
        .bind(&record.post_id)
        .bind(&record.lens_id)
        .bind(&processed_at)
        .bind(record.stance.as_str())
        .bind(&record.raw_path)
        .bind(&record.thesis_slug)
        .bind(&record.x_post_id)
        .bind(&record.nostr_event_id)
        .execute(&self.pool)
        .await
        .map_err(|error| StateError::Database(error.to_string()))?;

        Ok(())
    }

    async fn get_processed(
        &self,
        post_id: &str,
    ) -> Result<Option<ProcessedPostRecord>, StateError> {
        let row: Option<(
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            r#"
            SELECT post_id, lens_id, processed_at, stance, raw_path, thesis_slug,
                   x_post_id, nostr_event_id
            FROM processed_posts
            WHERE post_id = ?
            "#,
        )
        .bind(post_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| StateError::Database(error.to_string()))?;

        match row {
            Some((
                post_id,
                lens_id,
                processed_at,
                stance,
                raw_path,
                thesis_slug,
                x_post_id,
                nostr_event_id,
            )) => Ok(Some(ProcessedPostRecord {
                post_id,
                lens_id,
                processed_at: parse_time(&processed_at)?,
                stance: stance.parse().map_err(
                    |error: news_lens_domain::AgentValidationError| {
                        StateError::Serialization(error.to_string())
                    },
                )?,
                raw_path,
                thesis_slug,
                x_post_id,
                nostr_event_id,
            })),
            None => Ok(None),
        }
    }
}

fn format_time(value: OffsetDateTime) -> Result<String, StateError> {
    value
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| StateError::Serialization(error.to_string()))
}

fn parse_time(value: &str) -> Result<OffsetDateTime, StateError> {
    OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map_err(|error| StateError::Serialization(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use news_lens_domain::Stance;

    #[tokio::test]
    async fn account_state_roundtrip() {
        let store = SqliteStateStore::in_memory().await.unwrap();

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
        let store = SqliteStateStore::in_memory().await.unwrap();

        let record = ProcessedPostRecord {
            post_id: "post123".to_string(),
            lens_id: "lens".to_string(),
            processed_at: OffsetDateTime::now_utc(),
            stance: Stance::Critique,
            raw_path: Some("raw/news/post.md".to_string()),
            thesis_slug: Some("post".to_string()),
            x_post_id: Some("xpost789".to_string()),
            nostr_event_id: None,
        };

        store.record_processed(&record).await.unwrap();

        assert!(store.is_processed("post123").await.unwrap());
        assert!(!store.is_processed("other-post").await.unwrap());

        let retrieved = store.get_processed("post123").await.unwrap();
        assert_eq!(retrieved.unwrap().x_post_id, Some("xpost789".to_string()));
    }

    #[tokio::test]
    async fn migration_matches_spec_schema() {
        let store = SqliteStateStore::in_memory().await.unwrap();

        let processed = store.table_columns("processed_posts").await.unwrap();
        assert_eq!(
            processed,
            vec![
                "post_id",
                "lens_id",
                "processed_at",
                "stance",
                "raw_path",
                "thesis_slug",
                "x_post_id",
                "nostr_event_id"
            ]
        );
        assert!(!processed.iter().any(|column| column == "taxonomy_hash"));
        assert!(!processed.iter().any(|column| column == "cost_usd"));
    }
}
