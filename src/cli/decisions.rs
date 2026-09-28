use clap::Args;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct DecideArguments {
    /// Strict lwiki.entity-decisions.v1 JSON; use - for stdin.
    #[arg(long)]
    pub file: PathBuf,
}
