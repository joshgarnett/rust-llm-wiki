//! Cooperative admission for explicit maintenance, separate from query limits.
use crate::domain::Blake3Hash;
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Clone)]
pub(crate) struct MaintenanceFile {
    pub hash: Blake3Hash,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct MaintenanceLimits {
    pub max_files: usize,
    pub max_path_steps: usize,
    pub max_manifest_bytes: usize,
    pub max_file_bytes: usize,
    pub max_retained_note_bytes: usize,
    pub max_io_bytes: u64,
    pub max_elapsed: Duration,
}

/// Byte counters describe admitted input, not native process RSS.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct MaintenanceUsage {
    pub files: usize,
    pub path_steps: usize,
    pub manifest_bytes: usize,
    pub retained_note_bytes: usize,
    pub io_bytes: u64,
    /// Included in io_bytes: raw layout and migration authority reads.
    pub layout_io_bytes: u64,
    pub observed_assets: usize,
}
