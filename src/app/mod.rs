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

#[cfg(test)]
mod indexed_cli_tests;

#[cfg(test)]
mod general_query_fixture;
#[cfg(test)]
mod refresh_fixture_export;
#[cfg(test)]
mod refresh_path_profile;

mod pages;
pub mod probe;
pub use pages::{PageBatchRequest, PageUpdate};
