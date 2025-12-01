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

/// Load AWS SDK configuration based on CLI flags.
/// - If `local` is true: uses test credentials ("test"/"test") and localhost:4566 endpoint
/// - If `endpoint` is provided, it overrides the default endpoint
/// - Otherwise: loads credentials from the standard AWS environment/config chain
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

#[derive(Debug, Subcommand)]
enum Commands {
    Info,
    List,
    Poll {
        /// the url of your dead letter queue
        /// ( can optionally be set via environment variable DLQ_URL=... )
        url: Option<String>,
    },
    Send,
    /// Send job items to an SQS queue in batches
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
    },
    Job {
        #[command(subcommand)]
        command: database::JobCommands,
    },
    Database {
        #[command(subcommand)]
        command: database::DatabaseCommands,
    },
}

impl Cli {
    pub async fn run(self) -> anyhow::Result<()> {
        // Load AWS config once, based on global flags
        let aws_config = load_aws_config(self.local, self.endpoint.as_deref()).await;
        let dlq = DeadLetterQueue::from_config(aws_config.clone(), None);

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
                dlq.poll(url.as_deref()).await;
            }
            Commands::Send => send::run(aws_config).await,
            Commands::SendBatch {
                job_id,
                queue_url,
                batch_size,
                stage_size,
                concurrency,
                retry_limit,
            } => {
                send::run_batch(
                    aws_config,
                    job_id,
                    &queue_url,
                    batch_size.min(10), // SQS max is 10
                    stage_size,
                    concurrency,
                    retry_limit,
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
