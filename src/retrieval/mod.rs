//! Deterministic discovery and exact-byte excerpts.
pub mod bundles;
pub mod context;
pub(crate) mod context_original_selection;
pub(crate) mod context_evidence;
pub(crate) mod context_selection;
pub mod context_selection_packet;
pub mod context_types;
pub(crate) mod context_units;
pub mod cursor;
mod evidence_set_selection;
pub mod excerpts;
pub mod filters;
pub(crate) mod indexed_context;
pub(crate) mod indexed_documents;
pub(crate) mod indexed_semantic;
pub(crate) mod indexed_units;
pub mod lexical;
pub mod literal;
#[cfg(test)]
mod normalized_literal_workflow_tests;
pub(crate) mod selected_documents;
pub mod selected_search;
#[cfg(test)]
mod selected_search_experiment;
pub mod types;
pub(crate) mod unit_inventory_types;
pub mod verification;
pub use context_types::*;
pub use lexical::search;
pub use types::*;
pub mod fusion;
pub mod render;
pub mod segment;
pub mod spaces;
pub mod vectors;
