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

pub mod embeddings;
pub mod remote;
pub mod skill_export;

pub mod interrupt;
pub mod research;
pub mod storage;
