//! Explicit roots always win; discovery never combines nested vaults.
use super::VaultRoot;
use crate::domain::Result;
use std::path::Path;

pub fn resolve(explicit: Option<&Path>, start: &Path, initialization: bool) -> Result<VaultRoot> {
    match explicit {
        Some(path) if initialization => VaultRoot::for_initialization(path),
        Some(path) => VaultRoot::explicit(path),
        None if initialization => VaultRoot::for_initialization(start),
        None => VaultRoot::discover(start),
    }
}
