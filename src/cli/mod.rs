//! Terminal and machine adapters for the same library operations.
pub mod arguments;
pub mod context;
pub mod dispatch;
pub mod extraction;
pub use arguments::{Arguments, Command, OutputFormat};
pub use dispatch::{execute, present};
