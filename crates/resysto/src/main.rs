mod command;

use std::error::Error;
use std::process::ExitCode;

use clap::Parser;

use crate::command::{Command, Migrate};

#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[tokio::main]
async fn main() -> ExitCode {
    let result: Result<(), Box<dyn Error>> = match Cli::parse().command {
        Command::Serve => resysto_server::serve().await.map_err(Into::into),
        Command::Migrate {
            command: Migrate::Up,
        } => resysto_migrator::migrate_up().await.map_err(Into::into),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", chain(error.as_ref()));
            ExitCode::FAILURE
        }
    }
}

fn chain(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text = format!("{text}: {cause}");
        source = cause.source();
    }
    text
}
