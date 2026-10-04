//! Rebuildable catalog generations and complete graph eligibility.
pub mod eligibility;
pub(crate) mod file_types;
mod integrity;
pub(crate) mod normalized_audit;
pub(crate) mod normalized_build;
pub(crate) mod normalized_delta;
#[cfg(test)]
mod normalized_delta_tests;
pub(crate) mod normalized_read;
pub(crate) mod normalized_schema;
#[cfg(test)]
mod ownership_tests;
pub mod publish;
pub(crate) mod query;
pub(crate) mod query_types;
pub(crate) mod row_projection;
pub mod scan;
pub(crate) mod selector;
pub mod snapshot;
pub(crate) mod source_refresh;
pub mod sql;
pub mod types;
pub use types::*;
