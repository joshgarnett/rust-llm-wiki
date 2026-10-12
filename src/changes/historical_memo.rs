//! One apply call's interpretation of freshly observed historical manifests.
//! Nothing here authorizes omitting discovery, reads or mutable owner checks.
use super::ChangeEngine;
use crate::domain::{RecordId, VaultRelativePath};
use std::{cell::RefCell, mem::size_of, path::PathBuf};

const MAX_ENTRIES: usize = 256;
const MAX_RETAINED_BYTES: usize = 16 * 1024 * 1024;
// Conservative bookkeeping allowance per owned allocation, not an allocator
// or process-RSS measurement. Original transient read/parse buffers are separate.
const ALLOCATION_ALLOWANCE: usize = 64;

struct Binding {
    root: PathBuf,
    vault: Box<str>,
}
struct Entry {
    id: Box<str>,
    witness: Vec<u8>,
    roots: Vec<VaultRelativePath>,
    charge: usize,
}
struct State {
    entries: Vec<Entry>,
    retained_bytes: usize,
}
pub(crate) struct HistoricalRevisionMemo {
    binding: Option<Binding>,
    state: RefCell<State>,
}

fn allocation(capacity: usize) -> Option<usize> {
    capacity.checked_add(ALLOCATION_ALLOWANCE)
}
fn entry_charge(
    id: &RecordId,
    witness_capacity: usize,
    roots_capacity: usize,
    roots: &[VaultRelativePath],
) -> Option<usize> {
    let mut bytes = allocation(id.as_str().len())?
        .checked_add(allocation(witness_capacity)?)?
        .checked_add(allocation(
            roots_capacity.checked_mul(size_of::<VaultRelativePath>())?,
        )?)?;
    for root in roots {
        bytes = bytes.checked_add(allocation(root.owned_capacity())?)?;
    }
    Some(bytes)
}

impl HistoricalRevisionMemo {
    pub(crate) fn new(engine: &ChangeEngine) -> Self {
        let mut entries = Vec::new();
        if entries.try_reserve_exact(MAX_ENTRIES).is_err() {
            return Self::disabled();
        }
        let binding = Binding {
            root: engine.fs.root().path().to_path_buf(),
            vault: engine.vault_id.as_str().into(),
        };
        // Entry structs live in the preallocated slots; per-entry charges count
        // only their owned buffers. Include the memo, binding and slot allocation.
        let retained_bytes = (|| {
            size_of::<Self>()
                .checked_add(allocation(binding.root.capacity())?)?
                .checked_add(allocation(binding.vault.len())?)?
                .checked_add(allocation(
                    entries.capacity().checked_mul(size_of::<Entry>())?,
                )?)
        })();
        let Some(retained_bytes) = retained_bytes.filter(|bytes| *bytes <= MAX_RETAINED_BYTES)
        else {
            return Self::disabled();
        };
        Self {
            binding: Some(binding),
            state: RefCell::new(State {
                entries,
                retained_bytes,
            }),
        }
    }

    fn disabled() -> Self {
        Self {
            binding: None,
            state: RefCell::new(State {
                entries: Vec::new(),
                retained_bytes: size_of::<Self>(),
            }),
        }
    }

    fn matches(&self, engine: &ChangeEngine) -> bool {
        self.binding.as_ref().is_some_and(|binding| {
            binding.root.as_path() == engine.fs.root().path()
                && binding.vault.as_ref() == engine.vault_id.as_str()
        })
    }

    /// A hit proves only that these complete, freshly read bytes already passed
    /// the original pure decoder and tree inventory for this vault and ID.
    pub(crate) fn overlap(
        &self,
        engine: &ChangeEngine,
        id: &RecordId,
        observed: &[u8],
        predicate: impl FnMut(&VaultRelativePath) -> bool,
    ) -> Option<bool> {
        if !self.matches(engine) {
            return None;
        }
        let state = self.state.borrow();
        let index = state
            .entries
            .binary_search_by(|entry| entry.id.as_ref().cmp(id.as_str()))
            .ok()?;
        let entry = &state.entries[index];
        if entry.witness.as_slice() != observed {
            return None;
        }
        Some(entry.roots.iter().any(predicate))
    }

    /// The caller supplies roots only after both original pure checks succeed.
    /// An exhausted or oversized memo changes no validation result.
    pub(crate) fn remember(
        &self,
        engine: &ChangeEngine,
        id: &RecordId,
        witness: Vec<u8>,
        roots: Vec<VaultRelativePath>,
    ) {
        if !self.matches(engine) {
            return;
        }
        let Some(charge) = entry_charge(id, witness.capacity(), roots.capacity(), &roots) else {
            return;
        };
        let mut state = self.state.borrow_mut();
        let position = state
            .entries
            .binary_search_by(|entry| entry.id.as_ref().cmp(id.as_str()));
        let previous = match position {
            Ok(index) => state.entries[index].charge,
            Err(_) if state.entries.len() >= MAX_ENTRIES => return,
            Err(_) => 0,
        };
        let Some(retained_bytes) = state
            .retained_bytes
            .checked_sub(previous)
            .and_then(|bytes| bytes.checked_add(charge))
            .filter(|bytes| *bytes <= MAX_RETAINED_BYTES)
        else {
            return;
        };
        let entry = Entry {
            id: id.as_str().into(),
            witness,
            roots,
            charge,
        };
        match position {
            Ok(index) => state.entries[index] = entry,
            Err(index) => state.entries.insert(index, entry),
        }
        state.retained_bytes = retained_bytes;
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn retained_usage(&self) -> (usize, usize) {
        let state = self.state.borrow();
        (state.entries.len(), state.retained_bytes)
    }
}
