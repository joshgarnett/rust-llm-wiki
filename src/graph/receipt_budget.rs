//! Admission for actual receipt/proof notes, independent of unrelated catalog size.
use super::policy_inputs::{PolicyInputKey, PolicyInputTrace, PolicyWork};
use crate::{
    domain::{ErrorCode, RecordId, Result, VaultRelativePath, WikiError},
    records::ParsedNote,
};
use std::collections::{BTreeMap, BTreeSet};
pub(crate) struct ReceiptBudget<'a> {
    max_files: usize,
    max_bytes: usize,
    seen: BTreeSet<VaultRelativePath>,
    bytes: usize,
    certificates: Option<BTreeSet<PolicyInputKey>>,
    pending: Option<PolicyInputKey>,
    trace: PolicyInputTrace,
    tracing: bool,
    meter: Option<&'a mut dyn FnMut(PolicyWork) -> Result<()>>,
    vault_id: Option<RecordId>,
    #[cfg(test)]
    remap_input_probe_mode: super::remap_input_probe::RemapInputProbeMode,
}
impl Default for ReceiptBudget<'_> {
    fn default() -> Self {
        Self {
            max_files: 4096,
            max_bytes: 64 * 1024 * 1024,
            seen: BTreeSet::new(),
            bytes: 0,
            certificates: None,
            pending: None,
            trace: PolicyInputTrace::default(),
            tracing: false,
            meter: None,
            vault_id: None,
            #[cfg(test)]
            remap_input_probe_mode: super::remap_input_probe::mode(),
        }
    }
}
impl<'a> ReceiptBudget<'a> {
    pub(crate) fn certified(certificates: Option<BTreeSet<PolicyInputKey>>) -> Self {
        Self {
            certificates,
            tracing: true,
            ..Self::default()
        }
    }
    pub(crate) fn with_meter(mut self, meter: &'a mut dyn FnMut(PolicyWork) -> Result<()>) -> Self {
        self.meter = Some(meter);
        self
    }
    pub(crate) fn bind_vault(&mut self, vault_id: &RecordId) -> Result<()> {
        if self.vault_id.as_ref().is_some_and(|old| old != vault_id) {
            return Err(super::remap::bad("receipt evaluator vault binding changed"));
        }
        self.vault_id = Some(vault_id.clone());
        Ok(())
    }
    pub(crate) fn vault_id(&self) -> Option<&RecordId> {
        self.vault_id.as_ref()
    }
    #[cfg(test)]
    pub(crate) fn remap_input_probe_mode(&self) -> super::remap_input_probe::RemapInputProbeMode {
        self.remap_input_probe_mode
    }
    /// Classification can inspect UTF8 and a lossy Markdown representation.
    /// Charge both potential byte passes; this never changes production budgets.
    #[cfg(test)]
    pub(crate) fn remap_probe_classification(&mut self, bytes: usize) -> Result<()> {
        self.step()?;
        let bytes = bytes.checked_mul(2).ok_or_else(exhausted)?;
        if let Some(meter) = self.meter.as_mut() {
            meter(PolicyWork { steps: 0, bytes })?;
        }
        Ok(())
    }
    pub(crate) fn step(&mut self) -> Result<()> {
        if let Some(meter) = self.meter.as_mut() {
            meter(PolicyWork { steps: 1, bytes: 0 })?;
        }
        Ok(())
    }
    pub(crate) fn require(&mut self, key: PolicyInputKey) -> Result<()> {
        self.step()?;
        if self.tracing {
            self.trace.keys.insert(key.clone());
        }
        if self
            .certificates
            .as_ref()
            .is_some_and(|keys| !keys.contains(&key))
        {
            self.pending = Some(key);
            return Err(super::remap::bad("internal incomplete policy input"));
        }
        Ok(())
    }
    pub(crate) fn into_trace(self) -> (Option<PolicyInputKey>, PolicyInputTrace) {
        (self.pending, self.trace)
    }

    pub(crate) fn observe(&mut self, path: &VaultRelativePath, note: &ParsedNote) -> Result<()> {
        self.step()?;
        if self.tracing && !self.trace.paths.contains_key(path) {
            if let Some(meter) = self.meter.as_mut() {
                meter(PolicyWork {
                    steps: 0,
                    bytes: note.raw.len(),
                })?;
            }
            self.trace
                .paths
                .insert(path.clone(), crate::domain::Blake3Hash::digest(&note.raw));
            self.step()?;
        }
        Ok(())
    }
    pub(crate) fn admit(&mut self, path: &VaultRelativePath, note: &ParsedNote) -> Result<()> {
        self.observe(path, note)?;
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
    pub(crate) fn find<'notes>(
        &mut self,
        notes: &'notes BTreeMap<VaultRelativePath, ParsedNote>,
        id: &RecordId,
    ) -> Result<(&'notes VaultRelativePath, &'notes ParsedNote)> {
        self.require(PolicyInputKey::CanonicalIdentity(id.clone()))?;
        let mut found = None;
        for (path, note) in notes {
            self.step()?;
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
