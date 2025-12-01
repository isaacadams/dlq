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
    /// AWS access key ID
    #[arg(long)]
    access_key_id: Option<String>,

    /// AWS secret access key
    #[arg(long)]
    secret_access_key: Option<String>,

    /// AWS endpoint
    #[arg(long)]
    endpoint: Option<String>,

    #[command(subcommand)]
    command: Commands,
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
        let credentials = credentials(
            self.access_key_id.as_deref(),
            self.secret_access_key.as_deref(),
        );
        let dlq = DeadLetterQueue::new(credentials, self.endpoint.as_deref(), None).await;

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
            Commands::Send => send::run().await,
            Commands::SendBatch {
                job_id,
                queue_url,
                batch_size,
                stage_size,
                concurrency,
                retry_limit,
            } => {
                send::run_batch(
                    job_id,
                    &queue_url,
                    self.endpoint.as_deref(),
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

pub fn credentials(
    access_key_id: Option<&str>,
    secret_access_key: Option<&str>,
) -> Option<aws_sdk_sqs::config::Credentials> {
    match (access_key_id, secret_access_key) {
        (Some(key), Some(secret)) => Some(aws_sdk_sqs::config::Credentials::new(
            key, secret, None, None, "static",
        )),
        _ => None,
    }
}
