//! Immutable source captures and exact, scoped evidence verification.
pub mod capture;
pub mod evidence;
pub(crate) mod identity;
pub(crate) mod import_manifest;
pub(crate) mod import_manifest_types;
pub(crate) mod indexed_refresh;
pub mod lifecycle;
pub(crate) mod local_text;
mod lookup;
pub(crate) use lookup::SourceNotes;
pub mod revision;
pub(crate) mod selected;
pub mod types;
pub use types::*;

#[cfg(test)]
#[path = "capture_named_tests.rs"]
mod capture_named_tests;
