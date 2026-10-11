//! Explicit frozen preparation input. This file grants no provider authority.
use crate::{domain::*, jobs::TaskSpec, sources::selected::SelectedSourceBinding};
use serde::{Deserialize, Serialize};

pub(crate) const MANIFEST_MAX_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const PARTITION_TASKS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceRelationManifest {
    pub version: u32,
    pub vault_id: RecordId,
    pub profile_id: String,
    pub config_fingerprint: Blake3Hash,
    pub recipe_hash: Blake3Hash,
    pub max_output_tokens: u64,
    pub entries: Vec<SourceRelationManifestEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceRelationManifestEntry {
    pub source: SelectedSourceBinding,
    pub task: TaskSpec,
}

#[derive(Debug, Serialize)]
pub(crate) struct SourceRelationPreparationReport {
    pub run_id: RecordId,
    pub manifest_hash: Blake3Hash,
    pub manifest_sources: usize,
    pub admitted_tasks: usize,
    pub completed: usize,
    pub failed: usize,
    pub pending: usize,
    pub dry_run: bool,
    pub warnings: Vec<String>,
}
