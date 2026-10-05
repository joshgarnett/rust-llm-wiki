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
mod indexed_graph_neighbors_tests;
#[cfg(test)]
mod named_neighbors_diagnostics_tests;

#[cfg(test)]
mod full_check_holder;
#[cfg(test)]
mod general_query_fixture;
#[cfg(test)]
mod refresh_fixture_export;
#[cfg(test)]
mod refresh_path_profile;

mod page_rename;
mod pages;
pub mod probe;
pub use pages::{PageBatchRequest, PageUpdate};

mod source_import;
mod source_import_state;
mod source_import_types;
pub use source_import::{
    SourceImportGroup, SourceImportOutcome, SourceImportPendingItem, SourceImportPreparation,
    SourceImportedItem, prepare_source_import,
};
#[cfg(test)]
mod source_import_tests;
