use clap::Args;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct ResolveArguments {
    /// Strict lwiki.graph-resolution.v1 JSON; use - for stdin.
    #[arg(long)]
    pub file: PathBuf,
}
