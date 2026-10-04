//! Retained proposals and recoverable, guarded canonical mutations.
pub mod apply;
pub mod history;
pub(crate) mod immutable;
pub(crate) mod indexed_refresh;
pub mod journal;
pub(crate) mod operation_authority;
pub use history::CommittedOutputProof;
pub mod outcome;
pub mod prepare;
pub mod recover;
pub(crate) mod replay;
pub mod resolve;
pub mod rollback;
pub mod types;
pub use types::*;
