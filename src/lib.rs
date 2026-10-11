//! Local Markdown wiki operations. Canonical records outlive their indexes.
#[cfg(test)]
extern crate self as lwiki;
pub mod app;
pub mod catalog;
pub mod changes;
pub mod cli;
pub mod config;
pub mod domain;
pub mod graph;
pub mod jobs;
pub mod output;
pub mod providers;
pub mod records;
pub mod research;
pub mod retrieval;
pub mod sources;
pub mod storage;
#[cfg(test)]
#[path = "../test_support/paths.rs"]
mod test_paths;
mod text_projection;
pub mod vault;
