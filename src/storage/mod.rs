//! Supported internal layout and exact-hash storage retention. Source evidence is never purged.
mod cleanup;
pub(crate) use cleanup::ImportEpochScope;
mod inventory;
pub mod layout;
pub(crate) mod maintenance_activation;
pub mod types;
pub(crate) use cleanup::import_epoch_active;
pub use cleanup::{cleanup, plan_cleanup};
pub use inventory::inventory;
pub use types::*;
