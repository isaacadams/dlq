use clap::{Parser, Subcommand};

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
}

impl Cli {
    pub async fn run(self) -> anyhow::Result<()> {
        match self.command {
            Commands::Info => dlq::info().await,
            Commands::List => dlq::list().await,
            Commands::Poll { url } => dlq::poll(url.as_deref()).await,
        };

        Ok(())
    }
}
