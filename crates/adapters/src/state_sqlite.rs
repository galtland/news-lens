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
            if parent.as_os_str().is_empty() {
                return Self::open(db_path).await;
            }
            std::fs::create_dir_all(parent)
                .map_err(|error| StateError::Database(error.to_string()))?;
        }

        Self::open(db_path).await
    }

    async fn open(db_path: &Path) -> Result<Self, StateError> {
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
        self.create_processed_posts_table("processed_posts").await?;
        self.migrate_processed_posts_primary_key().await?;
        self.add_gaps_column_if_missing().await?;

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

    async fn create_processed_posts_table(&self, table: &str) -> Result<(), StateError> {
        let sql = processed_posts_table_sql(table);
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|error| StateError::Database(error.to_string()))?;
        Ok(())
    }

    async fn migrate_processed_posts_primary_key(&self) -> Result<(), StateError> {
        let has_lens_scoped_primary_key = self.primary_key_columns("processed_posts").await?
            == vec!["post_id".to_string(), "lens_id".to_string()];
        let has_old_table = self.table_exists("processed_posts_old").await?;

        if has_lens_scoped_primary_key && !has_old_table {
            return Ok(());
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| StateError::Database(error.to_string()))?;

        if !has_lens_scoped_primary_key {
            sqlx::query("ALTER TABLE processed_posts RENAME TO processed_posts_old")
                .execute(&mut *tx)
                .await
                .map_err(|error| StateError::Database(error.to_string()))?;

            let sql = processed_posts_table_sql("processed_posts");
            sqlx::query(&sql)
                .execute(&mut *tx)
                .await
                .map_err(|error| StateError::Database(error.to_string()))?;
        }

        sqlx::query(
            r#"
            INSERT OR REPLACE INTO processed_posts
            (post_id, lens_id, processed_at, stance, raw_path, thesis_slug, x_post_id, nostr_event_id)
            SELECT post_id, lens_id, processed_at, stance, raw_path, thesis_slug, x_post_id, nostr_event_id
            FROM processed_posts_old
            "#,
        )
        .execute(&mut *tx)
        .await
        .map_err(|error| StateError::Database(error.to_string()))?;

        sqlx::query("DROP TABLE processed_posts_old")
            .execute(&mut *tx)
            .await
            .map_err(|error| StateError::Database(error.to_string()))?;

        tx.commit()
            .await
            .map_err(|error| StateError::Database(error.to_string()))?;

        Ok(())
    }

    async fn add_gaps_column_if_missing(&self) -> Result<(), StateError> {
        let columns = self.table_columns("processed_posts").await?;
        if columns.iter().any(|c| c == "gaps") {
            return Ok(());
        }
        sqlx::query("ALTER TABLE processed_posts ADD COLUMN gaps TEXT")
            .execute(&self.pool)
            .await
            .map_err(|error| StateError::Database(error.to_string()))?;
        Ok(())
    }

    async fn table_exists(&self, table: &str) -> Result<bool, StateError> {
        let count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?")
                .bind(table)
                .fetch_one(&self.pool)
                .await
                .map_err(|error| StateError::Database(error.to_string()))?;
        Ok(count.0 > 0)
    }

    async fn primary_key_columns(&self, table: &str) -> Result<Vec<String>, StateError> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM pragma_table_info(?) WHERE pk > 0 ORDER BY pk")
                .bind(table)
                .fetch_all(&self.pool)
                .await
                .map_err(|error| StateError::Database(error.to_string()))?;
        Ok(rows.into_iter().map(|(name,)| name).collect())
    }

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

fn processed_posts_table_sql(table: &str) -> String {
    format!(
        r#"
            CREATE TABLE IF NOT EXISTS {table} (
                post_id        TEXT NOT NULL,
                lens_id        TEXT NOT NULL,
                processed_at   TEXT NOT NULL,
                stance         TEXT NOT NULL,
                raw_path       TEXT,
                thesis_slug    TEXT,
                x_post_id      TEXT,
                nostr_event_id TEXT,
                gaps           TEXT,
                PRIMARY KEY (post_id, lens_id)
            )
            "#
    )
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

    async fn is_processed(&self, post_id: &str, lens_id: &str) -> Result<bool, StateError> {
        let count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM processed_posts WHERE post_id = ? AND lens_id = ?",
        )
        .bind(post_id)
        .bind(lens_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| StateError::Database(error.to_string()))?;

        Ok(count.0 > 0)
    }

    async fn record_processed(&self, record: &ProcessedPostRecord) -> Result<(), StateError> {
        let processed_at = format_time(record.processed_at)?;
        let gaps_json = record
            .gaps
            .as_ref()
            .filter(|g| !g.is_empty())
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| StateError::Serialization(error.to_string()))?;

        sqlx::query(
            r#"
            INSERT INTO processed_posts
            (post_id, lens_id, processed_at, stance, raw_path, thesis_slug, x_post_id, nostr_event_id, gaps)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(post_id, lens_id) DO UPDATE SET
                processed_at = excluded.processed_at,
                stance = excluded.stance,
                raw_path = excluded.raw_path,
                thesis_slug = excluded.thesis_slug,
                x_post_id = excluded.x_post_id,
                nostr_event_id = excluded.nostr_event_id,
                gaps = excluded.gaps
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
        .bind(&gaps_json)
        .execute(&self.pool)
        .await
        .map_err(|error| StateError::Database(error.to_string()))?;

        Ok(())
    }

    async fn get_processed(
        &self,
        post_id: &str,
        lens_id: &str,
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
            Option<String>,
        )> = sqlx::query_as(
            r#"
            SELECT post_id, lens_id, processed_at, stance, raw_path, thesis_slug,
                   x_post_id, nostr_event_id, gaps
            FROM processed_posts
            WHERE post_id = ? AND lens_id = ?
            "#,
        )
        .bind(post_id)
        .bind(lens_id)
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
                gaps,
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
                gaps: parse_gaps(gaps.as_deref())?,
            })),
            None => Ok(None),
        }
    }

    async fn list_processed_with_gaps(
        &self,
        lens_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<ProcessedPostRecord>, StateError> {
        let limit_clause = limit.map(|n| format!(" LIMIT {n}")).unwrap_or_default();
        let (sql, bind_lens) = match lens_id {
            Some(_) => (
                format!(
                    r#"
                    SELECT post_id, lens_id, processed_at, stance, raw_path, thesis_slug,
                           x_post_id, nostr_event_id, gaps
                    FROM processed_posts
                    WHERE gaps IS NOT NULL AND gaps != '' AND lens_id = ?
                    ORDER BY processed_at DESC{limit_clause}
                    "#
                ),
                true,
            ),
            None => (
                format!(
                    r#"
                    SELECT post_id, lens_id, processed_at, stance, raw_path, thesis_slug,
                           x_post_id, nostr_event_id, gaps
                    FROM processed_posts
                    WHERE gaps IS NOT NULL AND gaps != ''
                    ORDER BY processed_at DESC{limit_clause}
                    "#
                ),
                false,
            ),
        };

        let mut query = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
            ),
        >(&sql);
        if bind_lens {
            query = query.bind(lens_id.unwrap());
        }

        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|error| StateError::Database(error.to_string()))?;

        rows.into_iter()
            .map(
                |(
                    post_id,
                    lens_id,
                    processed_at,
                    stance,
                    raw_path,
                    thesis_slug,
                    x_post_id,
                    nostr_event_id,
                    gaps,
                )| {
                    Ok(ProcessedPostRecord {
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
                        gaps: parse_gaps(gaps.as_deref())?,
                    })
                },
            )
            .collect()
    }
}

fn parse_gaps(raw: Option<&str>) -> Result<Option<Vec<String>>, StateError> {
    match raw {
        None => Ok(None),
        Some("") => Ok(None),
        Some(s) => serde_json::from_str::<Vec<String>>(s)
            .map(|v| if v.is_empty() { None } else { Some(v) })
            .map_err(|error| StateError::Serialization(error.to_string())),
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
            gaps: Some(vec![
                "wiki has no focused article on X".to_string(),
                "wiki has no focused article on Y (suggest: ingest Z)".to_string(),
            ]),
        };

        store.record_processed(&record).await.unwrap();

        assert!(store.is_processed("post123", "lens").await.unwrap());
        assert!(!store.is_processed("post123", "other-lens").await.unwrap());
        assert!(!store.is_processed("other-post", "lens").await.unwrap());

        let retrieved = store
            .get_processed("post123", "lens")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.x_post_id, Some("xpost789".to_string()));
        assert_eq!(
            retrieved.gaps,
            Some(vec![
                "wiki has no focused article on X".to_string(),
                "wiki has no focused article on Y (suggest: ingest Z)".to_string(),
            ])
        );

        let with_gaps = store
            .list_processed_with_gaps(Some("lens"), None)
            .await
            .unwrap();
        assert_eq!(with_gaps.len(), 1);
        assert_eq!(with_gaps[0].post_id, "post123");
    }

    #[tokio::test]
    async fn new_accepts_relative_db_path_without_parent() {
        let path = std::path::PathBuf::from(format!(
            "state-sqlite-relative-{}.sqlite",
            uuid::Uuid::new_v4()
        ));

        let store = SqliteStateStore::new(&path).await.unwrap();
        let processed = store.table_columns("processed_posts").await.unwrap();
        assert_eq!(processed[0], "post_id");
        drop(store);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
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
                "nostr_event_id",
                "gaps",
            ]
        );
        assert!(!processed.iter().any(|column| column == "taxonomy_hash"));
        assert!(!processed.iter().any(|column| column == "cost_usd"));

        let pk_columns = store.primary_key_columns("processed_posts").await.unwrap();
        assert_eq!(pk_columns, vec!["post_id", "lens_id"]);
    }

    #[tokio::test]
    async fn migration_updates_post_only_primary_key_to_lens_scoped_key() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("state.sqlite");
        let db_url = format!("sqlite:{}?mode=rwc", path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&db_url)
            .await
            .expect("open old db");

        sqlx::query(
            r#"
            CREATE TABLE processed_posts (
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
        .execute(&pool)
        .await
        .expect("old schema");
        sqlx::query(
            r#"
            INSERT INTO processed_posts
            (post_id, lens_id, processed_at, stance, raw_path, thesis_slug, x_post_id, nostr_event_id)
            VALUES ('post123', 'lens-a', '1970-01-01T00:00:00Z', 'decline', 'raw/news/post.md', NULL, NULL, NULL)
            "#,
        )
        .execute(&pool)
        .await
        .expect("old row");
        drop(pool);

        let store = SqliteStateStore::new(&path).await.unwrap();

        assert_eq!(
            store.primary_key_columns("processed_posts").await.unwrap(),
            vec!["post_id", "lens_id"]
        );
        assert!(store.is_processed("post123", "lens-a").await.unwrap());
        assert!(!store.is_processed("post123", "lens-b").await.unwrap());

        let record = ProcessedPostRecord {
            post_id: "post123".to_string(),
            lens_id: "lens-b".to_string(),
            processed_at: OffsetDateTime::UNIX_EPOCH,
            stance: Stance::Critique,
            raw_path: Some("raw/news/post-b.md".to_string()),
            thesis_slug: Some("post-b".to_string()),
            x_post_id: None,
            nostr_event_id: None,
            gaps: None,
        };
        store.record_processed(&record).await.unwrap();

        assert!(
            store
                .get_processed("post123", "lens-a")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .get_processed("post123", "lens-b")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn migration_recovers_rows_from_leftover_old_table() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("state.sqlite");
        let db_url = format!("sqlite:{}?mode=rwc", path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&db_url)
            .await
            .expect("open partial db");

        sqlx::query(&processed_posts_table_sql("processed_posts"))
            .execute(&pool)
            .await
            .expect("new schema");
        sqlx::query(
            r#"
            CREATE TABLE processed_posts_old (
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
        .execute(&pool)
        .await
        .expect("old leftover schema");
        sqlx::query(
            r#"
            INSERT INTO processed_posts_old
            (post_id, lens_id, processed_at, stance, raw_path, thesis_slug, x_post_id, nostr_event_id)
            VALUES ('post123', 'lens-a', '1970-01-01T00:00:00Z', 'decline', 'raw/news/post.md', NULL, NULL, NULL)
            "#,
        )
        .execute(&pool)
        .await
        .expect("old leftover row");
        drop(pool);

        let store = SqliteStateStore::new(&path).await.unwrap();

        assert!(store.is_processed("post123", "lens-a").await.unwrap());
        assert!(!store.table_exists("processed_posts_old").await.unwrap());
    }
}
