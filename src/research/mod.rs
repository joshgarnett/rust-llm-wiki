//! Bounded acquisition and research workflow adapters.
pub mod acquire;

pub mod frontier;
pub mod gaps;
pub mod synthesis;
pub mod types;
pub use types::*;

pub mod plan;

pub mod stages;

pub mod inspection;

pub mod report;
pub mod runner;
