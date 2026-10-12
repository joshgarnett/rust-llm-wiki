//! Terminal and machine adapters for the same library operations.
pub mod arguments;
pub mod context;
pub mod dispatch;
pub mod extraction;
pub mod resolution;
pub use arguments::{Arguments, Command, OutputFormat};
pub use dispatch::{execute, present, present_detailed, present_with_wiki};
mod decisions;
mod review;

mod automatic_embeddings;
pub mod embeddings;
pub mod remote;
pub mod skill_export;

pub mod interrupt;
mod page_init;
pub mod research;
mod source_capture;
pub mod source_import;
pub mod storage;

#[cfg(test)]
mod maintenance_workflow_tests;

pub mod source_relations;

mod health;

pub mod config_set;
mod config_show;
mod graph_status;
mod search_status;
pub use search_status::human_warning;
mod human_status;
mod probe_diagnostic;
pub mod setup;
pub mod skill_install;

mod context_status;
pub use context_status::present_context_status;

mod read_status;
pub use read_status::present_read_status;
