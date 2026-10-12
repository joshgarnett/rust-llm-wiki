//! Terminal and machine adapters for the same library operations.
pub mod arguments;
pub mod context;
pub mod dispatch;
pub mod extraction;
pub mod resolution;
pub use arguments::{Arguments, Command, OutputFormat};
pub use dispatch::{execute, present, present_with_wiki};
mod decisions;
mod review;

mod automatic_embeddings;
pub mod embeddings;
pub mod remote;
pub mod skill_export;

pub mod interrupt;
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
mod human_status;
mod probe_diagnostic;
pub mod setup;
pub mod skill_install;
