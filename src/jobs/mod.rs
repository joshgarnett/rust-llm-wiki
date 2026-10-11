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
pub mod tasks;
#[cfg(test)]
#[allow(
    clippy::duplicate_mod,
    reason = "Instantiate shared fixture code in independent test namespaces; sharing module state would change the test contract."
)]
#[path = "../../tests/fixtures/p15/common.rs"]
mod test_support;

pub use checkpoint::settle_receipt;
