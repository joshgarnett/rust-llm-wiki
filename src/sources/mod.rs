//! Immutable source captures and exact, scoped evidence verification.
pub mod capture;
pub(crate) mod capture_inputs;
pub mod evidence;
pub(crate) mod identity;
pub(crate) mod import_manifest;
pub(crate) mod import_manifest_types;
pub(crate) mod indexed_refresh;
pub mod lifecycle;
pub(crate) mod local_text;
mod lookup;
pub(crate) mod presentation;
pub(crate) mod presentation_planning;
pub(crate) use lookup::SourceNotes;
pub mod revision;
pub(crate) mod selected;
pub mod types;
pub use types::*;

#[cfg(test)]
#[path = "capture_named_tests.rs"]
mod capture_named_tests;

pub mod path_refresh;
