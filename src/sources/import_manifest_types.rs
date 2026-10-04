//! Bounded local import manifests; these declarations are not write authority.
use crate::domain::Blake3Hash;
use serde::{Deserialize, Serialize};

pub(crate) const IMPORT_MANIFEST_VERSION: u32 = 1;
pub(crate) const MAX_IMPORT_MANIFEST_BYTES: u64 = 128 * 1024 * 1024;
pub(crate) const MAX_IMPORT_ITEMS: u64 = 200_000;
pub(crate) const MAX_IMPORT_ITEM_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_IMPORT_INPUT_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub(crate) const MAX_IMPORT_LINE_BYTES: usize = 65_536;

/// Explicit JSON Lines input list; relative paths use the list's directory.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportInput {
    pub path: String,
    pub title: Option<String>,
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ImportExtraction {
    Utf8Preserve,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportManifestItem {
    pub ordinal: u64,
    pub path: String,
    pub title: String,
    pub media_type: Option<String>,
    pub original_hash: Blake3Hash,
    pub byte_len: u64,
    pub extraction: ImportExtraction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ImportManifestRecord {
    Header { version: u32 },
    Item { item: ImportManifestItem },
    Footer { items: u64, body_hash: Blake3Hash },
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ManifestPreparation {
    pub path: String,
    pub manifest_hash: Blake3Hash,
    pub items: u64,
    pub input_bytes: u64,
    pub manifest_bytes: u64,
    pub first_item_offset: u64,
    pub preview: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ManifestBatch {
    pub items: Vec<ImportManifestItem>,
    pub next_offset: u64,
    pub eof: bool,
}
