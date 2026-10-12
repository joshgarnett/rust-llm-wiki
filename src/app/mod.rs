//! Library operations shared by CLI and agent adapters.
pub mod extraction;
mod init_recovery;
#[cfg(test)]
pub(crate) use init_recovery::with_checkpoint as with_init_checkpoint;
pub mod offline;
pub mod resolution;
pub mod types;
pub use types::*;
pub mod decisions;
pub mod embeddings;
mod indexed_embedding_inputs;
#[cfg(test)]
mod indexed_embedding_workflow_tests;
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
pub(crate) mod refresh_path_profile;

pub(crate) mod page_citations;
pub(crate) use page_citations::{migrate_source_citation_block, migration_source_refs};
mod page_rename;
mod pages;
pub use page_citations::PageSourceRefs;
#[cfg(test)]
mod normalized_change_lifecycle_tests;
#[cfg(test)]
mod page_citations_tests;
pub mod probe;
pub use pages::{PageBatchRequest, PageUpdate};

mod source_capture;
mod source_import;
mod source_read;
pub use source_capture::{
    SourceCaptureAction, SourceCaptureItem, SourceCaptureOutcome, SourceCaptureRequest,
    SourceCaptureTotals,
};
pub use source_read::{SourceReadOutcome, SourceReadRequest};
mod source_refresh_batch;
mod source_refresh_batch_types;
pub use source_refresh_batch_types::*;
mod source_import_state;
mod source_import_types;
#[cfg(test)]
mod source_refresh_batch_tests;
pub use source_import::{
    SourceImportGroup, SourceImportOutcome, SourceImportPendingItem, SourceImportPreparation,
    SourceImportedItem, prepare_source_import,
};
#[cfg(test)]
mod source_import_tests;

mod generation_publication;
pub(crate) mod source_relation_inputs;
pub(crate) mod source_relation_materialization;
pub(crate) mod source_relation_planning;
pub(crate) mod source_relation_preparation;
pub(crate) mod source_relation_preparation_types;

pub(crate) mod source_relation_prepare;
pub(crate) mod source_relation_quote;
#[cfg(test)]
mod source_relation_workflow_tests;

#[cfg(test)]
mod init_tests;

pub mod literal;
mod path_refresh;

#[cfg(test)]
mod normalized_graph_write_tests;
