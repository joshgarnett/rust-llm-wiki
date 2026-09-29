//! Local, durable handoffs to a host agent. This module never executes tools.
mod codec;
mod engine;
mod inspection;
mod storage;
mod types;
pub use engine::*;
pub use types::*;
