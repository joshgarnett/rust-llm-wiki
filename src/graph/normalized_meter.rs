//! One cumulative owner for selected graph capture, planning and publication.
use crate::domain::{ErrorCode, Result, VaultRelativePath, WikiError};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub(crate) const GRAPH_CAPTURE_FILES: usize = 4096;
pub(crate) const GRAPH_CAPTURE_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const GRAPH_PROCESSING_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const GRAPH_COMMAND_TIME: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct GraphUsage {
    pub captured_files: usize,
    pub captured_bytes: usize,
    pub processing_bytes: usize,
    pub filesystem_reads: usize,
    pub filesystem_probes: usize,
}

pub(crate) struct GraphOperationMeter {
    started: Instant,
    captured: BTreeMap<VaultRelativePath, usize>,
    usage: GraphUsage,
}

impl GraphOperationMeter {
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
            captured: BTreeMap::new(),
            usage: GraphUsage::default(),
        }
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.started.elapsed() >= GRAPH_COMMAND_TIME {
            return Err(exhausted("selected graph command exceeds its deadline"));
        }
        Ok(())
    }

    pub(crate) fn remaining_file_bytes(&self) -> Result<usize> {
        self.check()?;
        Ok(GRAPH_CAPTURE_BYTES
            .saturating_sub(self.usage.captured_bytes)
            .min(GRAPH_PROCESSING_BYTES.saturating_sub(self.usage.processing_bytes)))
    }

    /// Charge every actual read, including repeat reads and hash inputs. Only
    /// the first capture of a path contributes to the unique capture envelope.
    pub(crate) fn capture_read(&mut self, path: &VaultRelativePath, bytes: usize) -> Result<()> {
        self.check()?;
        if let Some(original) = self.captured.get(path) {
            if *original != bytes {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "selected graph input changed length during capture",
                ));
            }
        } else {
            let total = add(self.usage.captured_bytes, bytes, GRAPH_CAPTURE_BYTES)?;
            if self.captured.len() >= GRAPH_CAPTURE_FILES {
                return Err(exhausted("selected graph capture exceeds its file ceiling"));
            }
            self.captured.insert(path.clone(), bytes);
            self.usage.captured_files = self.captured.len();
            self.usage.captured_bytes = total;
        }
        self.read(bytes)
    }

    pub(crate) fn read(&mut self, bytes: usize) -> Result<()> {
        self.charge_processing(bytes)?;
        self.usage.filesystem_reads = self
            .usage
            .filesystem_reads
            .checked_add(1)
            .ok_or_else(|| exhausted("selected graph read count overflow"))?;
        Ok(())
    }

    pub(crate) fn probe(&mut self) -> Result<()> {
        self.check()?;
        self.usage.filesystem_probes = self
            .usage
            .filesystem_probes
            .checked_add(1)
            .ok_or_else(|| exhausted("selected graph probe count overflow"))?;
        Ok(())
    }

    pub(crate) fn charge_processing(&mut self, bytes: usize) -> Result<()> {
        self.check()?;
        self.usage.processing_bytes =
            add(self.usage.processing_bytes, bytes, GRAPH_PROCESSING_BYTES)?;
        Ok(())
    }

    /// A mathematical guard-read floor rejects impossible plans before any
    /// canonical write. Actual reads still charge this same owner later.
    /// This preflight does not reserve bytes by counting them as completed work.
    /// The existing executor checks all unchanged preconditions at loop entry,
    /// immediately before replacement, and immediately after replacement.
    /// Preparation, finalization, tree guards and target reads are additional.
    pub(crate) fn preflight_guard_floor(
        &self,
        unchanged_bytes: usize,
        writes: usize,
    ) -> Result<()> {
        self.check()?;
        let passes = writes
            .checked_mul(3)
            .ok_or_else(|| exhausted("graph guard work overflow"))?;
        let required = unchanged_bytes
            .checked_mul(passes)
            .and_then(|n| n.checked_add(self.usage.processing_bytes))
            .filter(|n| *n <= GRAPH_PROCESSING_BYTES)
            .ok_or_else(|| exhausted("graph plan exceeds its mandatory repeated guard work"))?;
        let _ = required;
        Ok(())
    }

    pub(crate) fn usage(&self) -> &GraphUsage {
        &self.usage
    }
}

fn add(before: usize, bytes: usize, max: usize) -> Result<usize> {
    before
        .checked_add(bytes)
        .filter(|n| *n <= max)
        .ok_or_else(|| exhausted("selected graph processing or capture exceeds its byte ceiling"))
}

fn exhausted(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
