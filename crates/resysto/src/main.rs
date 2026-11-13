mod command;

use clap::Parser;

use crate::command::{Command, Migrate};

#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Serve => resysto_server::serve().await?,
        Command::Migrate {
            command: Migrate::Up,
        } => resysto_migrator::migrate_up().await?,
    }
    Ok(())
}
