//! Managed paths, cooperating writer locks, and durable filesystem boundaries.
pub mod discovery;
pub mod fs;
pub mod lock;
pub mod paths;

pub use fs::{BeforeImage, DirectorySync, DurableIo, ExpectedState, NativeIo, StagedFile, VaultFs};
pub use lock::WriterPermit;
pub use paths::VaultRoot;
