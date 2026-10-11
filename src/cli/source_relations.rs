//! Arguments and bounded exact-byte input for explicit Source preparation.
use crate::domain::*;
use clap::{Args, Subcommand};
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    time::Instant,
};
const MANIFEST_MAX_BYTES: usize = 16 * 1024 * 1024;
#[derive(Debug, Args)]
pub struct SourceRelationsArguments {
    #[command(subcommand)]
    pub command: SourceRelationsCommand,
}
#[derive(Debug, Subcommand)]
pub enum SourceRelationsCommand {
    /// Plan complete current Sources without sending requests or creating jobs.
    Prepare(SourceRelationsPrepareArguments),
    /// Execute or resume the same Run using the exact prepared manifest bytes.
    Run(SourceRelationsRunArguments),
}
impl SourceRelationsCommand {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Prepare(_) => "relations prepare",
            Self::Run(_) => "relations run",
        }
    }
}
#[derive(Debug, Args)]
pub struct SourceRelationsPrepareArguments {
    #[command(flatten)]
    pub remote: super::remote::RemoteArguments,
    /// Complete current Source ID to prepare; repeat up to4096 times.
    #[arg(long = "source-id", required = true)]
    pub source_ids: Vec<RecordId>,
    /// Maximum generated output tokens for each prepared Source task.
    #[arg(long, default_value_t = 4096)]
    pub max_output_tokens: u64,
    /// Seal every actual request and report finite costs without sending it.
    #[arg(long)]
    pub quote: bool,
}
#[derive(Debug, Args)]
pub struct SourceRelationsRunArguments {
    #[command(flatten)]
    pub remote: super::remote::RemoteArguments,
    /// Exact manifest JSON file, or - for standard input.
    #[arg(long)]
    pub file: PathBuf,
    /// Explicit Run identity; repeat this identity and exact manifest to resume.
    #[arg(long)]
    pub run: RecordId,
    /// Maximum Source tasks processed in this invocation.
    #[arg(long, default_value_t=16, value_parser=clap::value_parser!(usize))]
    pub max_tasks: usize,
}
fn check(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "relation manifest input deadline exceeded",
        ));
    }
    Ok(())
}
fn bounded_input(reader: impl Read, deadline: Instant) -> Result<Vec<u8>> {
    check(deadline)?;
    let mut bytes = Vec::new();
    reader
        .take(MANIFEST_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| WikiError::invalid(format!("cannot read relation manifest: {e}")))?;
    check(deadline)?;
    if bytes.is_empty() || bytes.len() > MANIFEST_MAX_BYTES {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "relation manifest must contain1..16MiB exact bytes",
        ));
    }
    Ok(bytes)
}
impl SourceRelationsRunArguments {
    pub fn manifest_bytes(&self, deadline: Instant) -> Result<Vec<u8>> {
        check(deadline)?;
        if self.max_tasks == 0 || self.max_tasks > 4096 {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "relation invocation task ceiling must be1..4096",
            ));
        }
        if self.file == Path::new("-") {
            return bounded_input(io::stdin().lock(), deadline);
        }
        let file = File::open(&self.file)
            .map_err(|e| WikiError::invalid(format!("cannot open relation manifest: {e}")))?;
        let metadata = file
            .metadata()
            .map_err(|e| WikiError::invalid(format!("cannot inspect relation manifest: {e}")))?;
        if !metadata.is_file() {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "relation manifest path must be a regular file",
            ));
        }
        if metadata.len() == 0 || metadata.len() > MANIFEST_MAX_BYTES as u64 {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "relation manifest exceeds exact-byte input ceiling before allocation",
            ));
        }
        bounded_input(file, deadline)
    }
}
#[cfg(test)]
#[path = "source_relations_tests.rs"]
mod tests;
