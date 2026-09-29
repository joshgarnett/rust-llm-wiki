//! Library operations shared by CLI and agent adapters.
pub mod extraction;
pub mod offline;
pub mod resolution;
pub mod types;
pub use types::*;
pub mod decisions;
pub mod embeddings;
pub mod remote;
mod review;

pub mod probe;
