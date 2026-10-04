//! Admission for actual receipt/proof notes, independent of unrelated catalog size.
use crate::{
    domain::{ErrorCode, RecordId, Result, VaultRelativePath, WikiError},
    records::ParsedNote,
};
use std::collections::{BTreeMap, BTreeSet};
pub(crate) struct ReceiptBudget {
    max_files: usize,
    max_bytes: usize,
    seen: BTreeSet<VaultRelativePath>,
    bytes: usize,
}
impl Default for ReceiptBudget {
    fn default() -> Self {
        Self {
            max_files: 4096,
            max_bytes: 64 * 1024 * 1024,
            seen: BTreeSet::new(),
            bytes: 0,
        }
    }
}
impl ReceiptBudget {
    pub(crate) fn admit(&mut self, path: &VaultRelativePath, note: &ParsedNote) -> Result<()> {
        if self.seen.contains(path) {
            return Ok(());
        }
        let bytes = self
            .bytes
            .checked_add(note.raw.len())
            .filter(|bytes| *bytes <= self.max_bytes)
            .ok_or_else(exhausted)?;
        if self.seen.len() >= self.max_files {
            return Err(exhausted());
        }
        self.seen.insert(path.clone());
        self.bytes = bytes;
        Ok(())
    }
    /// Preserve exhaustive canonical-identity discovery, including duplicate
    /// witnesses. Only matching notes enter the proof byte/count allowance.
    pub(crate) fn find<'a>(
        &mut self,
        notes: &'a BTreeMap<VaultRelativePath, ParsedNote>,
        id: &RecordId,
    ) -> Result<(&'a VaultRelativePath, &'a ParsedNote)> {
        let mut found = None;
        for (path, note) in notes {
            if note
                .canonical
                .as_ref()
                .is_some_and(|record| record.id() == id)
            {
                self.admit(path, note)?;
                if found.replace((path, note)).is_some() {
                    return Err(super::remap::bad("duplicate canonical ID"));
                }
            }
        }
        found.ok_or_else(|| super::remap::bad(format!("missing canonical record {id}")))
    }
    #[cfg(test)]
    pub(super) fn limited(max_files: usize, max_bytes: usize) -> Self {
        Self {
            max_files,
            max_bytes,
            ..Self::default()
        }
    }
    #[cfg(test)]
    pub(super) fn usage(&self) -> (usize, usize) {
        (self.seen.len(), self.bytes)
    }
}
pub(super) fn exhausted() -> WikiError {
    WikiError::new(
        ErrorCode::BudgetExceeded,
        "receipt proof input allowance exhausted",
    )
}
#[cfg(test)]
#[path = "receipt_budget_tests.rs"]
pub(super) mod tests;
