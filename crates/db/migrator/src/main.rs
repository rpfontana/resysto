use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Migrate the master database, then every ready or suspended tenant
    Up,
    /// Show what up would do on each database, changing nothing
    Plan,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    match Cli::parse().command {
        Command::Up => resysto_migrator::migrate_up().await?,
        Command::Plan => resysto_migrator::migrate_plan().await?,
    }
    Ok(())
}
