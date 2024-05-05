use clap::{Parser, Subcommand};

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
    List,
    Poll { url: Option<String> },
}

impl Cli {
    pub async fn run(self) -> anyhow::Result<()> {
        match self.command {
            Commands::List => dlq::list().await,
            Commands::Poll { url } => dlq::poll(url.as_deref()).await,
        };

        Ok(())
    }
}
