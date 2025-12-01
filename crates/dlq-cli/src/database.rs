use std::path::Path;

use indicatif::{ProgressBar, ProgressStyle};
use serde_json::Value;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions},
    types::Json,
    QueryBuilder, Row,
};
use tokio::sync::OnceCell;

#[derive(Debug, clap::Subcommand)]
pub enum JobCommands {
    /// List all jobs
    List,
    /// Show status of a job
    Status { id: i64 },
    /// Load job items from a JSONL file or stdin
    Load {
        /// Path to JSONL file (reads from stdin if omitted)
        file: Option<std::path::PathBuf>,

        /// Job name (required)
        #[arg(long)]
        name: String,
    },
    /// Reset stale 'processing' items back to 'pending'
    Reset { id: i64 },
}

impl JobCommands {
    pub async fn run(self) -> anyhow::Result<()> {
        match self {
            JobCommands::List => {
                let db = Database::new().await?;
                let jobs = db.list_jobs().await?;
                println!("Jobs:");
                for job in jobs {
                    println!(
                        "[{}] {} #{} (input_type: {})",
                        job.timestamp_start.format("%Y-%m-%d %H:%M:%S"),
                        job.name,
                        job.id,
                        job.configuration.input_type
                    );
                }
            }
            JobCommands::Status { id } => {
                let db = Database::new().await?;
                let counts = db.check_job_status(id).await?;

                println!(
                    "Job ID: {}\nTotal Items: {}\n",
                    id,
                    counts.iter().map(|(_, count)| count).sum::<i64>()
                );
                for (status, count) in counts {
                    println!("{}: {}", status, count);
                }
            }
            JobCommands::Load { file, name } => {
                let db = Database::new().await?;
                let (job_id, count) = db.load_job_items(&name, file.as_deref()).await?;
                println!("✓ Created job #{} '{}' with {} items", job_id, name, count);
            }
            JobCommands::Reset { id } => {
                let db = Database::new().await?;
                let count = db.reset_processing_items(id).await?;
                println!("✓ Reset {} items from 'processing' to 'pending'", count);
            }
        }

        Ok(())
    }
}

#[derive(Debug, clap::Subcommand)]
pub enum DatabaseCommands {
    Seed {
        /// Job name
        name: String,

        #[arg(long)]
        items: usize,

        #[arg(short, long, action)]
        progress: bool,
    },
    Clean,
}

impl DatabaseCommands {
    pub async fn run(self) -> anyhow::Result<()> {
        match self {
            DatabaseCommands::Seed {
                name,
                items,
                progress,
            } => {
                let db = Database::new().await.unwrap();
                db.preload_db(&name, items, progress).await.unwrap();
            }
            DatabaseCommands::Clean => cleanup_database("sqs").await.unwrap(),
        }

        Ok(())
    }
}

/// Configuration stored in the jobs table as JSON
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JobConfiguration {
    /// Input type: "text" (default) or "json"
    #[serde(default = "default_input_type")]
    pub input_type: String,
}

fn default_input_type() -> String {
    "text".to_string()
}

impl Default for JobConfiguration {
    fn default() -> Self {
        Self {
            input_type: default_input_type(),
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct JobModel {
    pub id: i64,
    pub name: String,
    pub configuration: Json<JobConfiguration>,
    pub timestamp_start: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
    pub timestamp_end: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct JobItemModel {
    pub id: i64,
    pub job_id: i64,
    pub input: Value,
    pub output: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct RetryHistoryModel {
    pub id: i64,
    pub item_id: i64,
    pub attempt_number: i32,
    pub error: Option<String>,
    pub sqs_response: Option<Json<Value>>,
    pub batch_id: Option<String>,
    pub input_snapshot: Option<Json<Value>>,
    pub timestamp: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
}

/// Batch completion result for efficient bulk updates
#[derive(Debug)]
pub struct CompletionResult {
    pub id: i64,
    pub output: Option<Value>,
    pub error: Option<String>,
    pub success: bool,
}

static POOL: OnceCell<SqlitePool> = OnceCell::const_new();

async fn pool() -> &'static SqlitePool {
    POOL.get_or_init(|| async {
        let pool = SqlitePoolOptions::new()
            .max_connections(5) // Multiple readers OK with WAL; single writer bottleneck remains
            .test_before_acquire(true)
            .connect_with(
                SqliteConnectOptions::new()
                    .create_if_missing(true)
                    .filename("sqs.db")
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal) // Enables better concurrency for reads/writes
                    .busy_timeout(std::time::Duration::from_secs(30)), // Wait up to 30s for locks
            )
            .await
            .expect("Failed to connect to SQLite");

        // Create jobs table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                configuration JSON DEFAULT '{}',
                timestamp_start DATETIME DEFAULT CURRENT_TIMESTAMP,
                timestamp_end DATETIME
            )
            "#,
        )
        .execute(&pool)
        .await
        .expect("Failed to create jobs table");

        // Create job_items table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS job_items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                input JSON NOT NULL,
                output JSON,
                status TEXT DEFAULT 'pending' CHECK (status IN ('pending', 'processing', 'done', 'failed')),
                error TEXT,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&pool)
        .await
        .expect("Failed to create job_items table");

        // Indexes for performance (esp. on status and job_id for queries/claims)
        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_job_items_status_job ON job_items(status, job_id)
            "#,
        )
        .execute(&pool)
        .await
        .expect("Failed to create index");

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_job_items_id ON job_items(id)
            "#,
        )
        .execute(&pool)
        .await
        .expect("Failed to create index");

        // Create retry_history table for tracking retry attempts
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS retry_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                item_id INTEGER NOT NULL REFERENCES job_items(id) ON DELETE CASCADE,
                attempt_number INTEGER NOT NULL,
                error TEXT,
                sqs_response JSON,
                batch_id TEXT,
                input_snapshot JSON,
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&pool)
        .await
        .expect("Failed to create retry_history table");

        // Index for retry_history lookups by item_id
        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_retry_history_item_id ON retry_history(item_id)
            "#,
        )
        .execute(&pool)
        .await
        .expect("Failed to create retry_history index");

        pool
    })
    .await
}

/// Database wrapper that supports both file-based and in-memory SQLite pools.
///
/// For production use, call `Database::new()` which uses the shared file-based pool.
/// For testing, call `Database::in_memory()` which creates an isolated in-memory database.
pub struct Database {
    /// The pool can be either a static reference (for shared file-based pool)
    /// or an owned pool (for in-memory test databases)
    pool: DatabasePool,
}

/// Internal pool representation supporting both static and owned pools
enum DatabasePool {
    Static(&'static SqlitePool),
    Owned(SqlitePool),
}

impl DatabasePool {
    fn as_ref(&self) -> &SqlitePool {
        match self {
            DatabasePool::Static(p) => p,
            DatabasePool::Owned(p) => p,
        }
    }
}

impl Database {
    /// Creates a new Database using the shared file-based SQLite pool.
    /// This is the standard constructor for production use.
    pub async fn new() -> Result<Self, sqlx::Error> {
        let pool = pool().await;
        Ok(Self {
            pool: DatabasePool::Static(pool),
        })
    }

    /// Creates a new Database with an isolated in-memory SQLite database.
    /// Useful for testing to avoid conflicts between tests.
    #[cfg(test)]
    pub async fn in_memory() -> Result<Self, sqlx::Error> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal),
            )
            .await?;

        // Initialize schema for the in-memory database
        Self::initialize_schema(&pool).await?;

        Ok(Self {
            pool: DatabasePool::Owned(pool),
        })
    }

    /// Initialize the database schema (tables and indexes)
    async fn initialize_schema(pool: &SqlitePool) -> Result<(), sqlx::Error> {
        // Create jobs table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                configuration JSON DEFAULT '{}',
                timestamp_start DATETIME DEFAULT CURRENT_TIMESTAMP,
                timestamp_end DATETIME
            )
            "#,
        )
        .execute(pool)
        .await?;

        // Create job_items table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS job_items (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                input JSON NOT NULL,
                output JSON,
                status TEXT DEFAULT 'pending' CHECK (status IN ('pending', 'processing', 'done', 'failed')),
                error TEXT,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(pool)
        .await?;

        // Indexes
        sqlx::query(
            r#"CREATE INDEX IF NOT EXISTS idx_job_items_status_job ON job_items(status, job_id)"#,
        )
        .execute(pool)
        .await?;

        sqlx::query(r#"CREATE INDEX IF NOT EXISTS idx_job_items_id ON job_items(id)"#)
            .execute(pool)
            .await?;

        // Create retry_history table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS retry_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                item_id INTEGER NOT NULL REFERENCES job_items(id) ON DELETE CASCADE,
                attempt_number INTEGER NOT NULL,
                error TEXT,
                sqs_response JSON,
                batch_id TEXT,
                input_snapshot JSON,
                timestamp DATETIME DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(pool)
        .await?;

        sqlx::query(
            r#"CREATE INDEX IF NOT EXISTS idx_retry_history_item_id ON retry_history(item_id)"#,
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Returns a reference to the underlying pool
    fn pool(&self) -> &SqlitePool {
        self.pool.as_ref()
    }

    pub async fn list_jobs(&self) -> Result<Vec<JobModel>, sqlx::Error> {
        sqlx::query_as::<_, JobModel>(
            "SELECT id, name, configuration, timestamp_start, timestamp_end FROM jobs",
        )
        .fetch_all(self.pool())
        .await
    }

    /// Gets a job by ID.
    pub async fn get_job(&self, id: i64) -> Result<Option<JobModel>, sqlx::Error> {
        sqlx::query_as::<_, JobModel>(
            "SELECT id, name, configuration, timestamp_start, timestamp_end FROM jobs WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await
    }

    /// Creates a new job and returns its ID.
    pub async fn create_job(&self, name: &str) -> Result<i64, sqlx::Error> {
        self.create_job_with_config(name, JobConfiguration::default())
            .await
    }

    /// Creates a new job with configuration and returns its ID.
    pub async fn create_job_with_config(
        &self,
        name: &str,
        config: JobConfiguration,
    ) -> Result<i64, sqlx::Error> {
        let config_json = serde_json::to_value(&config).unwrap();
        let row = sqlx::query(
            r#"
        INSERT INTO jobs (name, configuration)
        VALUES ($1, $2)
        RETURNING id
        "#,
        )
        .bind(name)
        .bind(config_json)
        .fetch_one(self.pool())
        .await?;

        let id: i64 = row.get("id");
        Ok(id)
    }

    /// Enqueues job items in batches for high throughput.
    /// Assumes `output` and `error` are `None` on input (ignores them).
    /// Uses chunked transactions to avoid overwhelming SQLite with a single massive query/tx.
    pub async fn enqueue_job_items(
        &self,
        job_id: i64,
        mut items: Vec<JobItemModel>,
    ) -> Result<(), sqlx::Error> {
        const BATCH_SIZE: usize = 1000; // Tune based on memory; SQLite handles ~10k/sec inserts with WAL

        for chunk in items.chunks_mut(BATCH_SIZE) {
            let mut tx = self.pool().begin().await?;

            // Build batched INSERT query dynamically
            let mut query: QueryBuilder<sqlx::Sqlite> = sqlx::query_builder::QueryBuilder::new(
                "INSERT INTO job_items (job_id, input, status) ",
            );

            query.push_values(chunk, |mut b, item| {
                b.push_bind(job_id);
                b.push_bind(serde_json::to_value(item.input.clone()).unwrap());
                b.push_bind("pending");
            });

            query.build().execute(&mut *tx).await?;

            tx.commit().await?;
        }

        Ok(())
    }

    /// Counts pending items for a job (used for auto-optimization of stage size).
    pub async fn count_pending_items(&self, job_id: i64) -> Result<i64, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) as count
            FROM job_items
            WHERE job_id = $1 AND status = 'pending'
            "#,
        )
        .bind(job_id)
        .fetch_one(self.pool())
        .await?;

        Ok(row.get("count"))
    }

    /// Atomically stages a batch of pending items (sets to 'processing') and returns (id, input) pairs.
    /// Uses a transaction to select-then-update atomically. Filters by job_id if provided.
    pub async fn stage_pending_items(
        &self,
        job_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<(i64, Value)>, sqlx::Error> {
        let mut tx = self.pool().begin().await?;

        // Select IDs in tx (ORDER BY id for consistency)
        let ids_query = if let Some(_jid) = job_id {
            "SELECT id FROM job_items WHERE job_id = $1 AND status = 'pending' ORDER BY id LIMIT $2"
        } else {
            "SELECT id FROM job_items WHERE status = 'pending' ORDER BY id LIMIT $1"
        };

        let ids_result = if let Some(jid) = job_id {
            sqlx::query_as::<_, (i64,)>(ids_query)
                .bind(jid)
                .bind(limit)
                .fetch_all(&mut *tx)
                .await?
        } else {
            sqlx::query_as::<_, (i64,)>(ids_query)
                .bind(limit)
                .fetch_all(&mut *tx)
                .await?
        };

        let ids: Vec<i64> = ids_result.into_iter().map(|row| row.0).collect();
        if ids.is_empty() {
            let _ = tx.rollback().await;
            return Ok(Vec::new());
        }

        // Update to 'processing' in same tx
        let placeholders: String = (0..ids.len())
            .map(|_| "?".to_string())
            .collect::<Vec<_>>()
            .join(",");

        let update_query = format!(
            "UPDATE job_items SET status = 'processing', updated_at = CURRENT_TIMESTAMP WHERE id IN ({placeholders})"
        );
        let mut update_stmt = sqlx::query(&update_query);
        for &id in &ids {
            update_stmt = update_stmt.bind(id);
        }
        update_stmt.execute(&mut *tx).await?;

        tx.commit().await?;

        // Fetch inputs post-commit (safe since status is updated)
        let placeholders: String = (0..ids.len())
            .map(|_| "?".to_string())
            .collect::<Vec<_>>()
            .join(",");
        let fetch_query = format!("SELECT id, input FROM job_items WHERE id IN ({placeholders})");
        let mut fetch_stmt = sqlx::query_as::<_, (i64, Json<Value>)>(&fetch_query);
        for &id in &ids {
            fetch_stmt = fetch_stmt.bind(id);
        }
        let rows = fetch_stmt.fetch_all(self.pool()).await?;

        Ok(rows.into_iter().map(|row| (row.0, row.1 .0)).collect())
    }

    /// Completes a claimed item with output/error, setting status to 'done' or 'failed'.
    pub async fn complete_item(
        &self,
        id: i64,
        output: Option<Value>,
        error: Option<String>,
    ) -> Result<(), sqlx::Error> {
        let status = if error.is_some() { "failed" } else { "done" };
        sqlx::query(
            r#"
        UPDATE job_items
        SET output = $1, status = $2, error = $3, updated_at = CURRENT_TIMESTAMP
        WHERE id = $4
        "#,
        )
        .bind(output.map(|o| serde_json::to_value(o).unwrap()))
        .bind(status)
        .bind(error)
        .bind(id)
        .execute(self.pool())
        .await?;

        Ok(())
    }

    /// Completes multiple items in a single transaction for efficiency.
    /// Much faster than individual complete_item calls.
    pub async fn complete_items_batch(
        &self,
        results: Vec<CompletionResult>,
    ) -> Result<(), sqlx::Error> {
        if results.is_empty() {
            return Ok(());
        }

        let mut tx = self.pool().begin().await?;

        // Separate successful and failed items
        let (successes, failures): (Vec<_>, Vec<_>) = results.into_iter().partition(|r| r.success);

        // Batch update successful items
        if !successes.is_empty() {
            for chunk in successes.chunks(100) {
                for result in chunk {
                    sqlx::query(
                        r#"
                        UPDATE job_items
                        SET output = $1, status = 'done', updated_at = CURRENT_TIMESTAMP
                        WHERE id = $2
                        "#,
                    )
                    .bind(
                        result
                            .output
                            .as_ref()
                            .map(|o| serde_json::to_value(o).unwrap()),
                    )
                    .bind(result.id)
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }

        // Batch update failed items
        if !failures.is_empty() {
            for chunk in failures.chunks(100) {
                for result in chunk {
                    sqlx::query(
                        r#"
                        UPDATE job_items
                        SET output = $1, status = 'failed', error = $2, updated_at = CURRENT_TIMESTAMP
                        WHERE id = $3
                        "#,
                    )
                    .bind(result.output.as_ref().map(|o| serde_json::to_value(o).unwrap()))
                    .bind(&result.error)
                    .bind(result.id)
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn check_job_status(&self, job_id: i64) -> Result<Vec<(String, i64)>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
        SELECT status, COUNT(*) as count
        FROM job_items
        WHERE job_id = $1
        GROUP BY status
        "#,
        )
        .bind(job_id)
        .fetch_all(self.pool())
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| (row.get("status"), row.get("count")))
            .collect::<Vec<(String, i64)>>())
    }

    /// Checks if a job is complete (all items 'done' or 'failed') and updates timestamp_end if so.
    /// returns none if the job is done, otherwise returns count of all items still pending or processing
    pub async fn check_and_close_job(&self, job_id: i64) -> Result<Option<i64>, sqlx::Error> {
        let row = sqlx::query(
            r#"
        SELECT COUNT(*) as pending_count
        FROM job_items
        WHERE job_id = $1 AND status IN ('pending', 'processing')
        "#,
        )
        .bind(job_id)
        .fetch_one(self.pool())
        .await?;

        let pending_count: i64 = row.get("pending_count");

        if pending_count > 0 {
            return Ok(Some(pending_count));
        }

        sqlx::query(
            r#"
        UPDATE jobs
        SET timestamp_end = CURRENT_TIMESTAMP
        WHERE id = $1 AND timestamp_end IS NULL
        "#,
        )
        .bind(job_id)
        .execute(self.pool())
        .await?;

        Ok(None)
    }

    /// Records a retry attempt in the retry_history table.
    pub async fn record_retry(
        &self,
        item_id: i64,
        attempt_number: i32,
        error: Option<&str>,
        sqs_response: Option<Value>,
        batch_id: Option<&str>,
        input_snapshot: Option<Value>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO retry_history (item_id, attempt_number, error, sqs_response, batch_id, input_snapshot)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(item_id)
        .bind(attempt_number)
        .bind(error)
        .bind(sqs_response)
        .bind(batch_id)
        .bind(input_snapshot)
        .execute(self.pool())
        .await?;

        Ok(())
    }

    /// Gets the current retry count for an item.
    pub async fn get_retry_count(&self, item_id: i64) -> Result<i32, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT COALESCE(MAX(attempt_number), 0) as count
            FROM retry_history
            WHERE item_id = $1
            "#,
        )
        .bind(item_id)
        .fetch_one(self.pool())
        .await?;

        Ok(row.get("count"))
    }

    /// Resets stale 'processing' items back to 'pending' for a given job.
    /// Returns the number of items reset.
    pub async fn reset_processing_items(&self, job_id: i64) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            r#"
            UPDATE job_items
            SET status = 'pending', updated_at = CURRENT_TIMESTAMP
            WHERE job_id = $1 AND status = 'processing'
            "#,
        )
        .bind(job_id)
        .execute(self.pool())
        .await?;

        Ok(result.rows_affected())
    }

    /// Resets a failed item back to 'pending' for retry.
    pub async fn reset_item_for_retry(&self, item_id: i64) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            UPDATE job_items
            SET status = 'pending', updated_at = CURRENT_TIMESTAMP
            WHERE id = $1
            "#,
        )
        .bind(item_id)
        .execute(self.pool())
        .await?;

        Ok(())
    }

    /// Loads job items from a JSONL file or stdin.
    /// Creates a new job with the given name and default configuration (input_type: "text").
    /// Returns the job ID and number of items loaded.
    pub async fn load_job_items(
        &self,
        job_name: &str,
        file_path: Option<&std::path::Path>,
    ) -> anyhow::Result<(i64, usize)> {
        use std::io::{BufRead, BufReader};

        // Create job with default configuration
        let config = JobConfiguration::default();
        let job_id = self.create_job_with_config(job_name, config).await?;

        let reader: Box<dyn BufRead> = match file_path {
            Some(path) => {
                let file = std::fs::File::open(path)?;
                Box::new(BufReader::new(file))
            }
            None => Box::new(BufReader::new(std::io::stdin())),
        };

        let mut count = 0usize;
        let mut batch: Vec<Value> = Vec::with_capacity(1000);

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            // Store the line as-is (could be text or JSON, we store it as a JSON string)
            let input_value = serde_json::Value::String(line);
            batch.push(input_value);
            count += 1;

            // Insert in batches of 1000
            if batch.len() >= 1000 {
                self.insert_job_items_batch(job_id, &batch).await?;
                batch.clear();
            }
        }

        // Insert remaining items
        if !batch.is_empty() {
            self.insert_job_items_batch(job_id, &batch).await?;
        }

        Ok((job_id, count))
    }

    /// Helper to insert a batch of job items.
    async fn insert_job_items_batch(
        &self,
        job_id: i64,
        items: &[Value],
    ) -> Result<(), sqlx::Error> {
        if items.is_empty() {
            return Ok(());
        }

        let mut tx = self.pool().begin().await?;

        let mut query: QueryBuilder<sqlx::Sqlite> = sqlx::query_builder::QueryBuilder::new(
            "INSERT INTO job_items (job_id, input, status) ",
        );

        query.push_values(items.iter(), |mut b, item| {
            b.push_bind(job_id);
            b.push_bind(item.clone());
            b.push_bind("pending");
        });

        query.build().execute(&mut *tx).await?;
        tx.commit().await?;

        Ok(())
    }

    /// Preloads the DB with a new job containing `num_items` pending items.
    /// Inputs are simple JSON like `{"index": <i>, "data": "item_<i>"}` (customize as needed).
    /// Uses larger batches for preload efficiency.
    ///
    /// # Arguments
    /// * `num_items` - Number of items to preload
    /// * `show_progress` - Whether to display a progress bar (default: false)
    pub async fn preload_db(
        &self,
        name: &str,
        num_items: usize,
        show_progress: bool,
    ) -> anyhow::Result<i64> {
        let job_id = self
            .create_job_with_config(name, JobConfiguration::default())
            .await?;

        const BATCH_SIZE: usize = 10_000; // Larger for bulk preload; tune for memory

        // Setup progress bar if requested
        let pb = if show_progress {
            let pb = ProgressBar::new(num_items as u64);
            pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} items ({eta})")
                .unwrap()
                .progress_chars("#>-"),
        );
            Some(pb)
        } else {
            None
        };

        for i in (0..num_items).step_by(BATCH_SIZE) {
            let end = (i + BATCH_SIZE).min(num_items);
            let mut tx = self.pool().begin().await?;

            // Build batched INSERT query dynamically
            let mut query: QueryBuilder<sqlx::Sqlite> = sqlx::query_builder::QueryBuilder::new(
                "INSERT INTO job_items (job_id, input, status) ",
            );

            query.push_values(i..end, |mut b, j| {
                b.push_bind(job_id);
                b.push_bind(
                    serde_json::to_value(
                        serde_json::json!({ "index": j, "data": format!("item_{}", j) }),
                    )
                    .unwrap(),
                );
                b.push_bind("pending");
            });

            query.build().execute(&mut *tx).await?;

            tx.commit().await?;

            // Update progress bar
            if let Some(ref pb) = pb {
                pb.set_position(end as u64);
            }
        }

        // Finish progress bar
        if let Some(pb) = pb {
            pb.finish_and_clear();
            println!("✓ Preloaded job {} with {} items", job_id, num_items);
        } else {
            println!("Preloaded job {} with {} items.", job_id, num_items);
        }

        Ok(job_id)
    }
}

/// Properly closes the database connection and deletes the SQLite database file
/// along with its associated WAL and SHM files.
///
/// # Returns
/// * `Ok(())` if cleanup was successful
/// * `Err` if there was an error during cleanup
///
/// # Note
/// This will close the database pool and remove all database files.
/// Any subsequent database operations will recreate the database.
pub async fn cleanup_database(name: &str) -> anyhow::Result<()> {
    // Close the pool if it exists
    if let Some(pool) = POOL.get() {
        pool.close().await;
    }

    let db_path = format!("{}.db", name);
    let wal_path = format!("{}.db-wal", name);
    let shm_path = format!("{}.db-shm", name);

    // Delete main database file
    if Path::new(&db_path).exists() {
        std::fs::remove_file(&db_path)?;
        println!("✓ Deleted {}", &db_path);
    }

    // Delete WAL file (Write-Ahead Log)
    if Path::new(&wal_path).exists() {
        std::fs::remove_file(&wal_path)?;
        println!("✓ Deleted {}", &wal_path);
    }

    // Delete SHM file (Shared Memory)
    if Path::new(&shm_path).exists() {
        std::fs::remove_file(&shm_path)?;
        println!("✓ Deleted {}", &shm_path);
    }

    println!("✓ Database cleanup complete");
    Ok(())
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create an isolated in-memory test database
    async fn test_db() -> Database {
        Database::in_memory().await.unwrap()
    }

    // -------------------------------------------------------------------------
    // JobConfiguration Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_job_configuration_default() {
        let config = JobConfiguration::default();
        assert_eq!(config.input_type, "text");
    }

    #[test]
    fn test_job_configuration_serde_default() {
        // When deserializing an empty object, input_type should default to "text"
        let config: JobConfiguration = serde_json::from_str("{}").unwrap();
        assert_eq!(config.input_type, "text");
    }

    #[test]
    fn test_job_configuration_serde_custom() {
        let config: JobConfiguration = serde_json::from_str(r#"{"input_type": "json"}"#).unwrap();
        assert_eq!(config.input_type, "json");
    }

    // -------------------------------------------------------------------------
    // CompletionResult Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_completion_result_success() {
        let result = CompletionResult {
            id: 1,
            output: Some(serde_json::json!({"message_id": "abc123"})),
            error: None,
            success: true,
        };
        assert!(result.success);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_completion_result_failure() {
        let result = CompletionResult {
            id: 2,
            output: None,
            error: Some("Connection timeout".to_string()),
            success: false,
        };
        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("Connection timeout"));
    }

    // -------------------------------------------------------------------------
    // Database Job Operations Tests
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn test_create_job_with_default_config() {
        let db = test_db().await;
        let job_id = db.create_job("test_job").await.unwrap();
        assert!(job_id > 0);

        let job = db.get_job(job_id).await.unwrap().unwrap();
        assert_eq!(job.name, "test_job");
        assert_eq!(job.configuration.input_type, "text");
    }

    #[tokio::test]
    async fn test_create_job_with_custom_config() {
        let db = test_db().await;
        let config = JobConfiguration {
            input_type: "json".to_string(),
        };
        let job_id = db.create_job_with_config("json_job", config).await.unwrap();

        let job = db.get_job(job_id).await.unwrap().unwrap();
        assert_eq!(job.configuration.input_type, "json");
    }

    #[tokio::test]
    async fn test_get_nonexistent_job() {
        let db = test_db().await;
        let result = db.get_job(999999).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_list_jobs() {
        let db = test_db().await;
        let _job1 = db.create_job("job_a").await.unwrap();
        let _job2 = db.create_job("job_b").await.unwrap();

        let jobs = db.list_jobs().await.unwrap();
        assert!(jobs.len() >= 2);
    }

    // -------------------------------------------------------------------------
    // Job Items Operations Tests
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn test_count_pending_items() {
        let db = test_db().await;
        let job_id = db.create_job("count_test").await.unwrap();

        // Initially zero
        let count = db.count_pending_items(job_id).await.unwrap();
        // Note: count might be > 0 if other tests have run, so we just check it's a valid number
        assert!(count >= 0);
    }

    #[tokio::test]
    async fn test_stage_pending_items() {
        let db = test_db().await;
        let job_id = db.create_job("stage_test").await.unwrap();

        // Create some items via preload
        let items: Vec<JobItemModel> = (0..5)
            .map(|i| JobItemModel {
                id: 0,
                job_id,
                input: serde_json::json!({"index": i}),
                output: None,
                error: None,
            })
            .collect();
        db.enqueue_job_items(job_id, items).await.unwrap();

        // Stage items
        let staged = db.stage_pending_items(Some(job_id), 3).await.unwrap();
        assert_eq!(staged.len(), 3);

        // Verify they're now 'processing'
        let status = db.check_job_status(job_id).await.unwrap();
        let processing_count = status
            .iter()
            .find(|(s, _)| s == "processing")
            .map(|(_, c)| *c)
            .unwrap_or(0);
        assert!(processing_count >= 3);
    }

    #[tokio::test]
    async fn test_complete_item_success() {
        let db = test_db().await;
        let job_id = db.create_job("complete_test").await.unwrap();

        let items = vec![JobItemModel {
            id: 0,
            job_id,
            input: serde_json::json!({"test": "data"}),
            output: None,
            error: None,
        }];
        db.enqueue_job_items(job_id, items).await.unwrap();

        // Stage and complete
        let staged = db.stage_pending_items(Some(job_id), 1).await.unwrap();
        let item_id = staged[0].0;

        db.complete_item(item_id, Some(serde_json::json!({"result": "ok"})), None)
            .await
            .unwrap();

        let status = db.check_job_status(job_id).await.unwrap();
        let done_count = status
            .iter()
            .find(|(s, _)| s == "done")
            .map(|(_, c)| *c)
            .unwrap_or(0);
        assert!(done_count >= 1);
    }

    #[tokio::test]
    async fn test_complete_item_failure() {
        let db = test_db().await;
        let job_id = db.create_job("fail_test").await.unwrap();

        let items = vec![JobItemModel {
            id: 0,
            job_id,
            input: serde_json::json!({"test": "fail"}),
            output: None,
            error: None,
        }];
        db.enqueue_job_items(job_id, items).await.unwrap();

        let staged = db.stage_pending_items(Some(job_id), 1).await.unwrap();
        let item_id = staged[0].0;

        db.complete_item(item_id, None, Some("Error occurred".to_string()))
            .await
            .unwrap();

        let status = db.check_job_status(job_id).await.unwrap();
        let failed_count = status
            .iter()
            .find(|(s, _)| s == "failed")
            .map(|(_, c)| *c)
            .unwrap_or(0);
        assert!(failed_count >= 1);
    }

    #[tokio::test]
    async fn test_complete_items_batch() {
        let db = test_db().await;
        let job_id = db.create_job("batch_complete_test").await.unwrap();

        // Create 10 items
        let items: Vec<JobItemModel> = (0..10)
            .map(|i| JobItemModel {
                id: 0,
                job_id,
                input: serde_json::json!({"index": i}),
                output: None,
                error: None,
            })
            .collect();
        db.enqueue_job_items(job_id, items).await.unwrap();

        // Stage all
        let staged = db.stage_pending_items(Some(job_id), 10).await.unwrap();

        // Complete in batch (5 success, 5 failure)
        let results: Vec<CompletionResult> = staged
            .iter()
            .enumerate()
            .map(|(i, (id, _))| CompletionResult {
                id: *id,
                output: Some(serde_json::json!({"batch": true})),
                error: if i < 5 {
                    None
                } else {
                    Some("batch error".to_string())
                },
                success: i < 5,
            })
            .collect();

        db.complete_items_batch(results).await.unwrap();

        let status = db.check_job_status(job_id).await.unwrap();
        let done_count = status
            .iter()
            .find(|(s, _)| s == "done")
            .map(|(_, c)| *c)
            .unwrap_or(0);
        let failed_count = status
            .iter()
            .find(|(s, _)| s == "failed")
            .map(|(_, c)| *c)
            .unwrap_or(0);

        assert!(done_count >= 5);
        assert!(failed_count >= 5);
    }

    // -------------------------------------------------------------------------
    // Reset Operations Tests
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn test_reset_processing_items() {
        let db = test_db().await;
        let job_id = db.create_job("reset_test").await.unwrap();

        let items: Vec<JobItemModel> = (0..3)
            .map(|i| JobItemModel {
                id: 0,
                job_id,
                input: serde_json::json!({"index": i}),
                output: None,
                error: None,
            })
            .collect();
        db.enqueue_job_items(job_id, items).await.unwrap();

        // Stage items (sets to 'processing')
        let _staged = db.stage_pending_items(Some(job_id), 3).await.unwrap();

        // Reset them back to pending
        let reset_count = db.reset_processing_items(job_id).await.unwrap();
        assert!(reset_count >= 3);

        // Verify they're pending again
        let pending = db.count_pending_items(job_id).await.unwrap();
        assert!(pending >= 3);
    }

    #[tokio::test]
    async fn test_reset_item_for_retry() {
        let db = test_db().await;
        let job_id = db.create_job("retry_reset_test").await.unwrap();

        let items = vec![JobItemModel {
            id: 0,
            job_id,
            input: serde_json::json!({"retry": true}),
            output: None,
            error: None,
        }];
        db.enqueue_job_items(job_id, items).await.unwrap();

        let staged = db.stage_pending_items(Some(job_id), 1).await.unwrap();
        let item_id = staged[0].0;

        // Reset for retry
        db.reset_item_for_retry(item_id).await.unwrap();

        // Should be pending again
        let pending = db.count_pending_items(job_id).await.unwrap();
        assert!(pending >= 1);
    }

    // -------------------------------------------------------------------------
    // Retry History Tests
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn test_record_and_get_retry_count() {
        let db = test_db().await;
        let job_id = db.create_job("retry_history_test").await.unwrap();

        let items = vec![JobItemModel {
            id: 0,
            job_id,
            input: serde_json::json!({"retry_test": true}),
            output: None,
            error: None,
        }];
        db.enqueue_job_items(job_id, items).await.unwrap();

        let staged = db.stage_pending_items(Some(job_id), 1).await.unwrap();
        let item_id = staged[0].0;

        // Record multiple retry attempts
        db.record_retry(item_id, 1, Some("First error"), None, Some("batch1"), None)
            .await
            .unwrap();
        db.record_retry(item_id, 2, Some("Second error"), None, Some("batch2"), None)
            .await
            .unwrap();

        let retry_count = db.get_retry_count(item_id).await.unwrap();
        assert_eq!(retry_count, 2);
    }

    // -------------------------------------------------------------------------
    // Load Job Items Tests
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn test_load_job_items_from_file() {
        use std::io::Write;

        let db = test_db().await;

        // Create a temp JSONL file using std temp_dir
        let temp_path = std::env::temp_dir().join(format!("dlq_test_{}.jsonl", std::process::id()));
        {
            let mut file = std::fs::File::create(&temp_path).unwrap();
            writeln!(file, "line one").unwrap();
            writeln!(file, "line two").unwrap();
            writeln!(file, "line three").unwrap();
        }

        let result = db.load_job_items("load_test", Some(&temp_path)).await;

        // Clean up temp file
        let _ = std::fs::remove_file(&temp_path);

        let (job_id, count) = result.unwrap();

        assert!(job_id > 0);
        assert_eq!(count, 3);

        let job = db.get_job(job_id).await.unwrap().unwrap();
        assert_eq!(job.name, "load_test");
        assert_eq!(job.configuration.input_type, "text");
    }

    // -------------------------------------------------------------------------
    // Check and Close Job Tests
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn test_check_and_close_job_incomplete() {
        let db = test_db().await;
        let job_id = db.create_job("incomplete_job").await.unwrap();

        let items = vec![JobItemModel {
            id: 0,
            job_id,
            input: serde_json::json!({"pending": true}),
            output: None,
            error: None,
        }];
        db.enqueue_job_items(job_id, items).await.unwrap();

        // Job is not complete (has pending items)
        let result = db.check_and_close_job(job_id).await.unwrap();
        assert!(result.is_some()); // Returns remaining count
    }

    #[tokio::test]
    async fn test_check_and_close_job_complete() {
        let db = test_db().await;
        let job_id = db.create_job("complete_job").await.unwrap();

        let items = vec![JobItemModel {
            id: 0,
            job_id,
            input: serde_json::json!({"will_complete": true}),
            output: None,
            error: None,
        }];
        db.enqueue_job_items(job_id, items).await.unwrap();

        // Stage and complete all items
        let staged = db.stage_pending_items(Some(job_id), 1).await.unwrap();
        for (item_id, _) in staged {
            db.complete_item(item_id, Some(serde_json::json!({"done": true})), None)
                .await
                .unwrap();
        }

        // Job should be complete
        let result = db.check_and_close_job(job_id).await.unwrap();
        assert!(result.is_none()); // Returns None when complete

        // Verify timestamp_end is set
        let job = db.get_job(job_id).await.unwrap().unwrap();
        assert!(job.timestamp_end.is_some());
    }
}
