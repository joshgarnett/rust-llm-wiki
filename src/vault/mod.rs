//! Managed paths, cooperating writer locks, and durable filesystem boundaries.
pub mod discovery;
pub mod fs;
pub mod lock;
pub(crate) mod operational;
#[cfg(test)]
mod operational_tests;
pub mod paths;

pub use fs::{BeforeImage, DirectorySync, DurableIo, ExpectedState, NativeIo, StagedFile, VaultFs};
pub use lock::WriterPermit;
pub use paths::VaultRoot;

#[cfg(windows)]
pub(crate) mod acl_policy;
#[cfg(windows)]
pub(crate) mod windows_security;
