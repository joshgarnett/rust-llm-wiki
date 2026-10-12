pub mod types;
#[cfg(test)]
use crate as lwiki;
pub use types::*;
#[cfg(test)]
mod accounting_tests;
pub mod budgets;
pub(crate) mod checkpoint;
pub mod diagnostics;
pub mod events;
mod ledger;
pub(crate) use ledger::initial_inspection;
mod replay;
/// Pure inspection of already authenticated storage history. This does not
/// establish checkpoint authority, current source freshness or send permission.
pub(crate) fn replay_storage_frames(
    frames: &[JournalFrame],
) -> crate::domain::Result<LedgerInspection> {
    Ok(replay::replay(frames, 0)?.inspection)
}
pub mod tasks;
#[cfg(test)]
#[allow(
    clippy::duplicate_mod,
    reason = "Instantiate shared fixture code in independent test namespaces; sharing module state would change the test contract."
)]
#[path = "../../tests/fixtures/p15/common.rs"]
mod test_support;

pub use checkpoint::settle_receipt;
