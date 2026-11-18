//! # DLQ CLI
//!
//! A command-line interface for working with AWS SQS dead letter queues.
//!
//! ## Features
//!
//! - **List Queues**: View all SQS queues in your AWS account
//! - **Poll Messages**: Receive and display messages from queues as JSON
//! - **Batch Sending**: High-throughput batch message sending with job tracking
//! - **Job Management**: SQLite-backed job queue for reliable processing
//! - **Local Development**: `--local` mode for SQS emulators (ElasticMQ, LocalStack)
//!
//! ## Usage
//!
//! ```bash
//! # List all queues
//! dlq list
//!
//! # Poll messages from a queue
//! dlq poll https://sqs.us-east-1.amazonaws.com/123456789/my-dlq
//!
//! # Move messages from a DLQ back to its source queue
//! dlq redrive https://sqs.us-east-1.amazonaws.com/123456789/my-dlq https://sqs.us-east-1.amazonaws.com/123456789/my-queue
//!
//! # Use a local SQS emulator (see docker-compose.yml)
//! dlq --local list
//!
//! # Debug logging (any `RUST_LOG` filter works)
//! RUST_LOG=debug dlq list
//! ```

use std::io::{BufRead, IsTerminal, Write};

use aws_config::SdkConfig;
use clap::{CommandFactory, Parser, Subcommand};
use dlq::DeadLetterQueue;

#[allow(dead_code)]
mod database;
mod reader;
mod send;

#[tokio::main]
pub async fn main() {
    reset_sigpipe();
    init_logging();

    if let Err(e) = Cli::parse().run().await {
        // `{:#}` includes the cause chain, e.g. "failed to list queues: dispatch failure: ..."
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// Exit quietly when stdout is closed early (e.g. `dlq poll ... | head`) instead of
/// panicking. Rust ignores SIGPIPE by default, which turns it into write errors.
fn reset_sigpipe() {
    #[cfg(unix)]
    // SAFETY: called once at startup before any other threads write to stdout.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// Logs to stderr, filtered by `RUST_LOG` (default: warnings and errors only).
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// AWS Dead Letter Queue CLI client.
///
/// A command-line tool for interacting with AWS SQS queues, with special focus
/// on dead letter queue operations like polling messages and batch processing.
#[derive(Debug, Parser)]
#[command(name = "dlq", version)]
#[command(about = "aws dead letter queue CLI client written in rust", long_about = None)]
pub struct Cli {
    /// Use local development mode (test credentials + localhost:4566 endpoint)
    #[arg(long, global = true)]
    local: bool,

    /// AWS endpoint URL (overrides default, works with --local)
    #[arg(long, global = true)]
    endpoint: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

/// Loads AWS SDK configuration based on CLI flags.
///
/// This function configures the AWS SDK based on the provided flags:
///
/// - If `local` is true: Uses test credentials ("test"/"test") and localhost:4566 endpoint
///   for local SQS emulators (ElasticMQ, LocalStack)
/// - If `endpoint` is provided: Uses that endpoint URL (overrides local default)
/// - Otherwise: Loads credentials from the standard AWS environment/config chain
///
/// # Arguments
///
/// * `local` - Whether to use local development mode (local SQS emulator)
/// * `endpoint` - Optional custom endpoint URL
///
/// # Returns
///
/// A configured `SdkConfig` ready for use with AWS SDK clients.
///
/// # Example
///
/// ```no_run
/// # async fn example() {
/// // Production mode - uses standard AWS credentials
/// let config = load_aws_config(false, None).await;
///
/// // Local emulator mode
/// let config = load_aws_config(true, None).await;
///
/// // Custom endpoint
/// let config = load_aws_config(false, Some("http://custom-endpoint:4566")).await;
/// # }
/// # use dlq_cli::load_aws_config;
/// ```
pub async fn load_aws_config(local: bool, endpoint: Option<&str>) -> SdkConfig {
    let mut loader = aws_config::from_env().region(
        aws_config::meta::region::RegionProviderChain::default_provider()
            .or_else(aws_config::Region::from_static("us-east-1")),
    );

    if local {
        // Use test credentials for local development (e.g., ElasticMQ or LocalStack)
        loader = loader.credentials_provider(aws_sdk_sqs::config::Credentials::new(
            "test", "test", None, None, "static",
        ));
    }

    // Determine endpoint: explicit flag > local default > none
    let effective_endpoint = endpoint.map(|s| s.to_string()).or_else(|| {
        if local {
            Some("http://localhost:4566".to_string())
        } else {
            None
        }
    });

    if let Some(ep) = effective_endpoint {
        loader = loader.endpoint_url(ep);
    }

    loader.load().await
}

/// Available CLI commands.
#[derive(Debug, Subcommand)]
enum Commands {
    /// Display current AWS configuration (endpoint, region, credentials)
    Info,

    /// List all SQS queue URLs in the AWS account
    List,

    /// Poll and display messages from a queue as JSON
    Poll {
        /// The URL of your dead letter queue.
        url: String,
    },

    /// Move messages from one queue to another (e.g. a DLQ back to its source queue)
    ///
    /// Each message is deleted from the source only after the destination accepts
    /// it. Message attributes are kept; FIFO destinations keep the original
    /// MessageGroupId and MessageDeduplicationId when present.
    Redrive {
        /// URL of the queue to move messages out of (usually the DLQ)
        source_url: String,

        /// URL of the queue to move messages into
        destination_url: String,

        /// Maximum number of messages to move (moves all if omitted)
        #[arg(long, short = 'n')]
        limit: Option<usize>,
    },

    /// Delete every message in a queue (cannot be undone)
    Purge {
        /// URL of the queue to purge
        url: String,

        /// Skip the confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Print a shell completion script
    ///
    /// Example: `dlq completions zsh > "${fpath[1]}/_dlq"`
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },

    /// Print the man page (roff) to stdout
    ///
    /// Example: `dlq man > /usr/local/share/man/man1/dlq.1`
    Man,

    /// Interactive message sending mode (reads from stdin)
    Send {
        /// The URL of your SQS queue to send messages to.
        url: String,
    },

    /// Send job items to an SQS queue in batches.
    ///
    /// Reads items from a job in the SQLite database and sends them to SQS
    /// using efficient batch operations. Supports concurrency, retries,
    /// and progress tracking.
    SendBatch {
        /// Job ID to process (must have name = 'send_batch')
        job_id: i64,

        /// SQS queue URL
        queue_url: String,

        /// Items per SQS batch (default: 10, max: 10)
        #[arg(long, default_value = "10")]
        batch_size: usize,

        /// Items to stage from SQLite per round (auto-optimizes if omitted)
        #[arg(long)]
        stage_size: Option<i64>,

        /// Parallel SQS batch sends (default: 5)
        #[arg(long, default_value = "5")]
        concurrency: usize,

        /// Max retries per item (default: 0 = disabled)
        #[arg(long, default_value = "0")]
        retry_limit: u32,

        /// Maximum number of items to process (processes all if omitted)
        #[arg(long, short = 'n')]
        limit: Option<i64>,

        /// Preview what would be sent without actually sending to SQS
        #[arg(long)]
        dry_run: bool,
    },

    /// Job management commands
    Job {
        #[command(subcommand)]
        command: database::JobCommands,
    },

    /// Database utility commands
    Database {
        #[command(subcommand)]
        command: database::DatabaseCommands,
    },
}

impl Cli {
    /// Executes the CLI command.
    ///
    /// Loads AWS configuration based on global flags, then dispatches to the
    /// appropriate command handler.
    ///
    /// # Returns
    ///
    /// * `Ok(())` - Command completed successfully
    /// * `Err(anyhow::Error)` - An error occurred during execution
    pub async fn run(self) -> anyhow::Result<()> {
        // Commands that don't talk to AWS
        match &self.command {
            Commands::Completions { shell } => {
                clap_complete::generate(*shell, &mut Cli::command(), "dlq", &mut std::io::stdout());
                return Ok(());
            }
            Commands::Man => {
                clap_mangen::Man::new(Cli::command()).render(&mut std::io::stdout())?;
                return Ok(());
            }
            _ => {}
        }

        // Load AWS config once, based on global flags
        let aws_config = load_aws_config(self.local, self.endpoint.as_deref()).await;
        let dlq = DeadLetterQueue::from_config(aws_config.clone());

        match self.command {
            Commands::Info => {
                println!("endpoint: {:#?}", dlq.config.endpoint_url());
                println!("region: {:#?}", dlq.config.region());
                println!("behavior: {:#?}", dlq.config.behavior_version());
                println!("credentials: {:#?}", dlq.config.credentials_provider());
            }
            Commands::List => {
                let queues = dlq.list().await?;
                if queues.is_empty() {
                    println!("No queues found.\n");
                    println!("To create a new queue, use the AWS CLI:");
                    println!("  aws sqs create-queue --queue-name <your-queue-name>\n");
                    println!("Or against a local SQS emulator (e.g. `docker compose up -d`):");
                    println!(
                        "  aws --endpoint-url=http://localhost:4566 sqs create-queue --queue-name <your-queue-name>"
                    );
                } else {
                    println!("┌─────────────────────────────────────────────────────────────────┐");
                    println!("│ Available SQS Queues                                            │");
                    println!("├─────────────────────────────────────────────────────────────────┤");
                    for (i, url) in queues.iter().enumerate() {
                        // Extract queue name from URL
                        let name = url.rsplit('/').next().unwrap_or(url);
                        println!("│ {}. {}", i + 1, name);
                        println!("│    {}", url);
                        if i < queues.len() - 1 {
                            println!("│");
                        }
                    }
                    println!("└─────────────────────────────────────────────────────────────────┘");
                    println!("\nTotal: {} queue(s)", queues.len());
                }
            }
            Commands::Poll { url } => {
                // Print each batch as it arrives rather than after every receive.
                dlq.poll_with(&url, |message| {
                    println!(
                        "{}",
                        serde_json::to_string(&message).expect("MessageModel serializes")
                    );
                })
                .await?;
            }
            Commands::Redrive {
                source_url,
                destination_url,
                limit,
            } => {
                let summary = dlq.redrive(&source_url, &destination_url, limit).await?;
                println!("Moved {} message(s)", summary.moved);
                if !summary.is_complete() {
                    anyhow::bail!(describe_incomplete_redrive(&summary, &source_url));
                }
            }
            Commands::Purge { url, yes } => {
                if !yes {
                    confirm_purge(&url)?;
                }
                dlq.purge(&url).await?;
                println!("Purged {url}");
            }
            Commands::Completions { .. } | Commands::Man => unreachable!("handled above"),
            Commands::Send { url } => send::run(aws_config, &url).await,
            Commands::SendBatch {
                job_id,
                queue_url,
                batch_size,
                stage_size,
                concurrency,
                retry_limit,
                limit,
                dry_run,
            } => {
                send::run_batch(
                    aws_config,
                    job_id,
                    &queue_url,
                    batch_size.min(10), // SQS max is 10
                    stage_size,
                    concurrency,
                    retry_limit,
                    limit,
                    dry_run,
                )
                .await?;
            }
            Commands::Database { command } => {
                command.run().await?;
            }
            Commands::Job { command } => {
                command.run().await?;
            }
        };

        Ok(())
    }
}

/// Asks the user to type the queue name before purging. Refuses when stdin isn't
/// a terminal, so scripts must pass `--yes` explicitly.
fn confirm_purge(url: &str) -> anyhow::Result<()> {
    let name = url.rsplit('/').next().unwrap_or(url);
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "refusing to purge {name} without confirmation; pass --yes to skip the prompt"
        );
    }

    eprint!("This permanently deletes every message in {name}.\nType the queue name to confirm: ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if answer.trim() != name {
        anyhow::bail!("confirmation did not match; nothing was purged");
    }
    Ok(())
}

/// Explains what a partial redrive left behind, by kind, so the user knows which
/// messages are safe to redrive again.
fn describe_incomplete_redrive(summary: &dlq::RedriveSummary, source_url: &str) -> String {
    fn codes(failures: &[dlq::RedriveFailure]) -> String {
        let mut codes: Vec<&str> = failures.iter().map(|f| f.error.code.as_str()).collect();
        codes.sort_unstable();
        codes.dedup();
        codes.join(", ")
    }

    let mut lines = vec!["redrive did not move every message:".to_string()];
    if !summary.rejected.is_empty() {
        lines.push(format!(
            "  - {} message(s) were rejected by the destination ({}) and remain in {source_url}",
            summary.rejected.len(),
            codes(&summary.rejected)
        ));
    }
    if !summary.blocked_groups.is_empty() {
        lines.push(format!(
            "  - FIFO group(s) {} stopped at a rejected message; their later messages were \
             not sent, to keep order, and remain in {source_url}",
            summary.blocked_groups.join(", ")
        ));
    }
    if !summary.not_deleted.is_empty() {
        lines.push(format!(
            "  - {} message(s) WERE delivered but could not be deleted from {source_url} ({}); \
             redriving again will deliver them twice",
            summary.not_deleted.len(),
            codes(&summary.not_deleted)
        ));
    }
    lines.join("\n")
}
