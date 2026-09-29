use clap::Args;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct ReviewArguments {
    /// Strict lwiki.graph-review.v1 JSON; use - for stdin.
    #[arg(long)]
    pub file: PathBuf,
}
