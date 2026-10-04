//! Rebuildable catalog generations and complete graph eligibility.
pub mod eligibility;
pub(crate) mod eligibility_facts;
pub(crate) mod eligibility_rules;
#[cfg(test)]
mod fact_query_tests;
pub(crate) mod file_types;
mod integrity;
pub(crate) mod link_facts;
pub(crate) mod maintenance;
pub(crate) mod maintenance_match;
pub(crate) mod navigation_resolution;
#[cfg(test)]
mod navigation_resolution_tests;
pub(crate) mod normalized_audit;
pub(crate) mod normalized_build;
pub(crate) mod normalized_delta;
#[cfg(test)]
mod normalized_delta_tests;
pub(crate) mod normalized_fact_delta;
#[cfg(test)]
mod normalized_fact_delta_tests;
#[cfg(test)]
mod normalized_fact_tests;
pub(crate) mod normalized_read;
pub(crate) mod normalized_schema;
#[cfg(test)]
mod ownership_tests;
#[cfg(test)]
mod projector_query_tests;
pub mod publish;
pub(crate) mod query;
pub(crate) mod query_types;
#[cfg(test)]
mod revision_reservation_tests;
pub(crate) mod row_projection;
pub(crate) mod maintenance_input;
pub(crate) mod maintenance_types;
pub mod scan;
pub(crate) mod selector;
pub mod snapshot;
pub(crate) mod source_projection;
pub(crate) mod source_refresh;
pub mod sql;
pub(crate) mod structural_rules;
pub mod types;
pub use types::*;
