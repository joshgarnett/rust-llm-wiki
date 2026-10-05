//! Rebuildable catalog generations and complete graph eligibility.
pub(crate) mod capture_projection;
pub(crate) mod compact_audit;
mod decision_rules;
pub(crate) mod doctor;
pub mod eligibility;
pub(crate) mod eligibility_facts;
pub(crate) mod eligibility_rules;
#[cfg(test)]
mod fact_query_tests;
pub(crate) mod file_types;
pub(crate) mod full_check;
pub(crate) mod full_check_rows;
pub(crate) mod full_check_scratch;
pub(crate) mod full_check_types;
mod integrity;
pub(crate) mod link_facts;
pub(crate) mod maintenance;
pub(crate) mod maintenance_input;
pub(crate) mod maintenance_match;
pub(crate) mod maintenance_types;
pub(crate) mod missing_cache;
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
pub(crate) mod normalized_metadata;
pub(crate) mod normalized_read;
pub(crate) mod normalized_schema;
#[cfg(test)]
mod ownership_tests;
mod policy_delta;
pub(crate) mod policy_facts;
mod policy_projection;
mod policy_query;
#[cfg(test)]
mod projector_query_tests;
pub mod publish;
pub(crate) mod query;
#[cfg(test)]
pub(crate) mod query_diagnostics;
pub(crate) mod query_types;
pub(crate) mod rename_projection;
#[cfg(test)]
mod revision_reservation_tests;
pub(crate) mod row_projection;
pub mod scan;
pub(crate) mod selector;
pub mod snapshot;
pub(crate) mod source_projection;
pub(crate) mod source_refresh;
pub mod sql;
mod structural_projection;
pub(crate) mod structural_rules;
pub mod types;
pub(crate) mod withdraw_projection;
pub(crate) mod write_projection;
pub use types::*;
