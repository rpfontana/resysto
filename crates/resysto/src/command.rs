use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Serve the HTTP application
    Serve,
    /// Apply database migrations
    Migrate {
        #[command(subcommand)]
        command: Migrate,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum Migrate {
    /// Migrate the master database, then every ready or suspended tenant
    Up,
}
