use crate::{
    app::OfflineApp,
    domain::*,
    graph::{ExportRequest, ExtractionLimits, MAX_WINDOW_BYTES, MAX_WINDOWS},
};
use clap::{Args, ValueEnum};
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Executor {
    Agent,
    Api,
}
#[derive(Debug, Args)]
pub struct ExtractArguments {
    #[arg(long)]
    pub source_id: RecordId,
    #[arg(long)]
    pub revision_id: Option<RecordId>,
    #[arg(long, value_enum, default_value = "agent")]
    pub executor: Executor,
    /// Exact UTF-8 source byte window START:END; repeat up to16 times.
    #[arg(long = "window")]
    pub windows: Vec<String>,
    #[arg(long, default_value_t = 64)]
    pub max_mentions: usize,
    #[arg(long, default_value_t = 128)]
    pub max_assertions: usize,
    #[arg(long, default_value_t = 262144)]
    pub max_output_bytes: usize,
    #[arg(long = "candidate-id")]
    pub candidate_ids: Vec<RecordId>,
}
impl ExtractArguments {
    pub fn request(&self, app: &OfflineApp) -> Result<ExportRequest> {
        if !matches!(self.executor, Executor::Agent) {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "API extraction is unavailable until accounted generation is implemented",
            ));
        }
        if self.windows.len() > MAX_WINDOWS {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "extraction windows exceed16",
            ));
        }
        let mut windows = Vec::new();
        for value in &self.windows {
            let invalid = || {
                WikiError::new(
                    ErrorCode::Usage,
                    "window must be START:END with at most12000 source bytes",
                )
            };
            let (start, end) = value.split_once(':').ok_or_else(invalid)?;
            let span = ByteSpan::new(
                start.parse().map_err(|_| invalid())?,
                end.parse().map_err(|_| invalid())?,
            )
            .map_err(|_| invalid())?;
            if span.is_empty() || span.len() > MAX_WINDOW_BYTES as u64 {
                return Err(invalid());
            }
            windows.push(span);
        }
        Ok(ExportRequest {
            source_id: self.source_id.clone(),
            revision_id: self.revision_id.clone(),
            windows,
            limits: ExtractionLimits {
                max_mentions: self.max_mentions,
                max_assertions: self.max_assertions,
                max_output_bytes: self.max_output_bytes,
            },
            candidate_context: app.extraction_candidates(&self.candidate_ids)?,
        })
    }
}
#[derive(Debug, Args)]
pub struct ImportArguments {
    #[arg(long)]
    pub file: PathBuf,
    #[arg(long)]
    pub new_extraction: bool,
}
pub fn response_input(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let result = if path == Path::new("-") {
        io::stdin().lock().take(262145).read_to_end(&mut bytes)
    } else {
        let file = File::open(path)
            .map_err(|_| WikiError::new(ErrorCode::Usage, "cannot open extraction response"))?;
        if !file
            .metadata()
            .map_err(|_| WikiError::new(ErrorCode::Usage, "cannot inspect extraction response"))?
            .is_file()
        {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "extraction response must be a regular file or stdin",
            ));
        }
        file.take(262145).read_to_end(&mut bytes)
    };
    result.map_err(|_| WikiError::new(ErrorCode::Usage, "cannot read extraction response"))?;
    if bytes.len() > 262144 {
        return Err(WikiError::new(
            ErrorCode::ExtractionInvalid,
            "extraction response exceeds262144 bytes",
        ));
    }
    Ok(bytes)
}
