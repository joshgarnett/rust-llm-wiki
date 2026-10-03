//! Immutable source captures and exact, scoped evidence verification.
pub mod capture;
pub mod evidence;
pub(crate) mod identity;
pub mod lifecycle;
mod lookup;
pub mod revision;
pub(crate) mod selected;
pub mod types;
pub use types::*;
