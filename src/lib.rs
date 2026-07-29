//! floe - a VM-backed harness for kernel tasks.
//!
//! The core is Rust because everything here is process and VM lifecycle:
//! spawning children that can hang, killing process groups, capturing serial
//! consoles, classifying faults. Python drives it.


pub mod grade;
pub mod proc;
pub mod task;
pub mod trial;
pub mod vmm;
pub mod vsock;
pub mod workspace;

pub use grade::Outcome;
pub use trial::{Agent, Config, TrialResult};
