//! Supported internal layout and exact-hash storage retention. Source evidence is never purged.
mod cleanup;
mod inventory;
pub mod layout;
pub mod types;
pub use cleanup::{cleanup, plan_cleanup};
pub use inventory::inventory;
pub use types::*;
