//! Retained proposals and recoverable, guarded canonical mutations.
pub mod apply;
pub mod history;
pub(crate) mod immutable;
pub mod journal;
pub use history::CommittedOutputProof;
pub mod outcome;
pub mod prepare;
pub mod recover;
pub mod resolve;
pub mod rollback;
pub mod types;
pub use types::*;
