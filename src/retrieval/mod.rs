//! Deterministic discovery and exact-byte excerpts.
pub mod cursor;
pub mod excerpts;
pub mod filters;
pub mod lexical;
pub mod literal;
pub mod types;
pub use lexical::search;
pub use types::*;
