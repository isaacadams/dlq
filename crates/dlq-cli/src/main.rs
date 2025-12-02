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
//! - **LocalStack Support**: First-class local development support
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
//! # Use LocalStack for local development
//! dlq --local list
//! ```

use aws_config::SdkConfig;
use clap::{Parser, Subcommand};
use dlq::DeadLetterQueue;

#[allow(dead_code)]
mod database;
mod reader;
mod send;
#[cfg(test)]
mod test;

#[tokio::main]
pub async fn main() {
    if let Err(e) = Cli::parse().run().await {
        eprintln!("{}", e);
    }
}

/// AWS Dead Letter Queue CLI client.
///
/// A command-line tool for interacting with AWS SQS queues, with special focus
/// on dead letter queue operations like polling messages and batch processing.
#[derive(Debug, Parser)]
#[command(name = "dlq")]
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
///   for LocalStack compatibility
/// - If `endpoint` is provided: Uses that endpoint URL (overrides local default)
/// - Otherwise: Loads credentials from the standard AWS environment/config chain
///
/// # Arguments
///
/// * `local` - Whether to use local development mode (LocalStack)
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
/// // LocalStack mode
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
        // Use test credentials for local development (e.g., LocalStack)
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
                let queues = dlq.list().await;
                if queues.is_empty() {
                    println!("No queues found.\n");
                    println!("To create a new queue, use the AWS CLI:");
                    println!("  aws sqs create-queue --queue-name <your-queue-name>\n");
                    println!("Or with LocalStack:");
                    println!("  aws --endpoint-url=http://localhost:4566 sqs create-queue --queue-name <your-queue-name>");
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
                dlq.poll(&url).await;
            }
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
