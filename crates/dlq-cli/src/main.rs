use clap::{Parser, Subcommand};
use dlq::DeadLetterQueue;

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
                println!("{}", queues.join(","));
            }
            Commands::Poll { url } => {
                dlq.poll(url.as_deref()).await;
            }
            Commands::Send => send::run().await,
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
