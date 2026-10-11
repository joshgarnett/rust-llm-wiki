//! Bounded catalog access shared by strict and generation-scoped retrieval.
use super::{CatalogDiagnostic, DocumentRow, ReaderSnapshot, RecordRow, SnapshotVerification};
use crate::domain::{
    Blake3Hash, ErrorCode, ReadSnapshot, RecordId, Result, VaultRelativePath, WikiError,
};
use rusqlite::{Connection, Row};
use std::collections::BTreeSet;

/// Decoded cache work, separate from canonical evidence verification budgets.
#[derive(Debug, Clone, Copy)]
pub(crate) struct QueryReadLimits {
    pub max_rows: usize,
    pub max_row_bytes: usize,
    pub max_bytes: usize,
    pub max_elapsed_ms: u64,
    pub max_vm_steps: u64,
}

impl Default for QueryReadLimits {
    fn default() -> Self {
        Self {
            max_rows: 4096,
            max_row_bytes: 8 * 1024 * 1024,
            max_bytes: 256 * 1024 * 1024,
            max_elapsed_ms: 30_000,
            max_vm_steps: 10_000_000,
        }
    }
}

impl QueryReadLimits {
    pub(crate) fn validate(&self) -> Result<()> {
        let ceiling = Self::default();
        if self.max_rows == 0
            || self.max_rows > ceiling.max_rows
            || self.max_row_bytes == 0
            || self.max_row_bytes > ceiling.max_row_bytes
            || self.max_bytes == 0
            || self.max_bytes > ceiling.max_bytes
            || self.max_elapsed_ms == 0
            || self.max_elapsed_ms > ceiling.max_elapsed_ms
            || self.max_vm_steps == 0
            || self.max_vm_steps > ceiling.max_vm_steps
        {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "catalog read budget exceeds its ceiling",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueryReadUsage {
    pub rows: usize,
    pub bytes: usize,
}

/// Returning owned rows keeps bounded reads fallible instead of hiding a full
/// projection behind a borrowed reference. The interface makes no freshness claim.
pub(crate) trait QueryCatalog {
    /// Check cooperative elapsed work after native SQL and Rust assembly.
    /// Strict projection adapters have no generation-scoped query clock.
    fn check_query_budget(&self) -> Result<()> {
        Ok(())
    }
    /// SQL layout for direct bounded retrieval. Legacy adapters keep returning
    /// false because their private connection retains the original SQL shape.
    fn normalized_layout(&self) -> bool {
        false
    }
    fn publication_id(&self) -> Option<&str> {
        None
    }
    fn connection(&self) -> &Connection;
    fn snapshot(&self) -> &ReadSnapshot;
    fn vault_id(&self) -> &RecordId;
    fn verification(&self) -> &SnapshotVerification;
    fn record(&self, id: &RecordId) -> Result<Option<RecordRow>>;
    fn document(&self, path: &VaultRelativePath) -> Result<Option<DocumentRow>>;
    fn diagnostics(&self, paths: &BTreeSet<VaultRelativePath>) -> Result<Vec<CatalogDiagnostic>>;
    fn dependency_fingerprint(&self) -> Result<Blake3Hash>;
    /// Include scope in query/cursor binding without changing legacy cursors.
    fn query_scope(&self) -> &'static str;
    /// Inspect borrowed SQLite text and reserve its bytes before deserialization.
    fn decode_document(&self, row: &Row<'_>, column: usize) -> Result<DocumentRow>;
}

pub(crate) fn row_json<'a>(row: &'a Row<'_>, column: usize) -> Result<&'a str> {
    row.get_ref(column)
        .map_err(super::sql::sql_error)?
        .as_str()
        .map_err(|error| WikiError::new(ErrorCode::IndexCorrupt, error.to_string()))
}

impl QueryCatalog for ReaderSnapshot {
    fn connection(&self) -> &Connection {
        self.connection()
    }
    fn snapshot(&self) -> &ReadSnapshot {
        self.snapshot()
    }
    fn vault_id(&self) -> &RecordId {
        &self.projection().vault_id
    }
    fn verification(&self) -> &SnapshotVerification {
        self.verification()
    }
    fn record(&self, id: &RecordId) -> Result<Option<RecordRow>> {
        Ok(self.projection().records.get(id).cloned())
    }
    fn document(&self, path: &VaultRelativePath) -> Result<Option<DocumentRow>> {
        Ok(self
            .projection()
            .documents
            .iter()
            .find(|row| &row.path == path)
            .cloned())
    }
    fn diagnostics(&self, paths: &BTreeSet<VaultRelativePath>) -> Result<Vec<CatalogDiagnostic>> {
        Ok(self
            .projection()
            .diagnostics
            .iter()
            .filter(|row| paths.contains(&row.path))
            .cloned()
            .collect())
    }
    fn dependency_fingerprint(&self) -> Result<Blake3Hash> {
        serde_json::to_vec(&self.projection().dependencies)
            .map(Blake3Hash::digest)
            .map_err(|error| WikiError::new(ErrorCode::Internal, error.to_string()))
    }
    fn query_scope(&self) -> &'static str {
        "strict_catalog"
    }
    fn decode_document(&self, row: &Row<'_>, column: usize) -> Result<DocumentRow> {
        serde_json::from_str(row_json(row, column)?)
            .map_err(|error| WikiError::new(ErrorCode::IndexCorrupt, error.to_string()))
    }
}
