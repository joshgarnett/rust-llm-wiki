//! Trusted provider operations and credential boundaries.
#[cfg(test)]
mod credential_tests;
pub mod credentials;
#[cfg(test)]
mod dispatch_tests;
pub mod dispatcher;
pub mod retry;
pub mod transport;
pub mod types;

mod diagnostics;
mod embedding_wire;
mod generation_schema;
mod generation_wire;
pub mod wire;
mod wire_json;
#[cfg(test)]
pub(crate) mod wire_tests;

mod history;
#[cfg(test)]
mod history_tests;
