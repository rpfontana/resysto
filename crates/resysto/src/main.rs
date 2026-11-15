mod command;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::command::{Command, Migrate};

#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    match Cli::parse().command {
        Command::Serve => resysto_server::serve().await?,
        Command::Migrate {
            command: Migrate::Up,
        } => resysto_migrator::migrate_up().await?,
        Command::Migrate {
            command: Migrate::Plan,
        } => resysto_migrator::migrate_plan().await?,
    }
    Ok(())
}
