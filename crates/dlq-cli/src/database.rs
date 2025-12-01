use std::error::Error;
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
    List,
    Status { id: i64 },
}

impl JobCommands {
    pub async fn run(self) -> Result<(), sqlx::Error> {
        match self {
            JobCommands::List => {
                let db = Database::new().await?;
                let jobs = db.list_jobs().await?;
                println!("Jobs:");
                for job in jobs {
                    println!(
                        "[{}] {} #{}",
                        job.timestamp_start.format("%Y-%m-%d %H:%M:%S").to_string(),
                        job.name,
                        job.id
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
        }

        Ok(())
    }
}

#[derive(Debug, clap::Subcommand)]
pub enum DatabaseCommands {
    Seed {
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
            DatabaseCommands::Seed { items, progress } => {
                let db = Database::new().await.unwrap();
                db.preload_db(items, progress).await.unwrap();
            }
            DatabaseCommands::Clean => cleanup_database("sqs").await.unwrap(),
        }

        Ok(())
    }
}

#[derive(Debug, sqlx::FromRow)]
pub struct JobModel {
    pub id: i64,
    pub name: String,
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
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal), // Enables better concurrency for reads/writes
            )
            .await
            .expect("Failed to connect to SQLite");

        // Create jobs table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
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

        pool
    })
    .await
}

struct Database {
    pool: &'static SqlitePool,
}

impl Database {
    pub async fn new() -> Result<Self, sqlx::Error> {
        let pool = pool().await;
        Ok(Self { pool })
    }

    pub async fn list_jobs(&self) -> Result<Vec<JobModel>, sqlx::Error> {
        sqlx::query_as::<_, JobModel>("SELECT id, name, timestamp_start, timestamp_end FROM jobs")
            .fetch_all(self.pool)
            .await
    }
    /// Creates a new job and returns its ID.
    pub async fn create_job(&self, name: &str) -> Result<i64, sqlx::Error> {
        let row = sqlx::query(
            r#"
        INSERT INTO jobs (name)
        VALUES ($1)
        RETURNING id
        "#,
        )
        .bind(name)
        .fetch_one(self.pool)
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
            let mut tx = self.pool.begin().await?;

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

    /// Atomically claims a batch of pending items (sets to 'processing') and returns (id, input) pairs.
    /// Uses a transaction to select-then-update atomically. Filters by job_id if provided.
    pub async fn claim_pending_items(
        &self,
        job_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<(i64, Value)>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;

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
        let rows = fetch_stmt.fetch_all(self.pool).await?;

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
        .execute(self.pool)
        .await?;

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
        .fetch_all(self.pool)
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
        .fetch_one(self.pool)
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
        .execute(self.pool)
        .await?;

        Ok(None)
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
        num_items: usize,
        show_progress: bool,
    ) -> Result<i64, Box<dyn Error>> {
        let job_id = self.create_job("preload_job").await?;

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
            let mut tx = self.pool.begin().await?;

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
pub async fn cleanup_database(name: &str) -> Result<(), Box<dyn Error>> {
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
