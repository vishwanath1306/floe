//! floe - a VM-backed harness for kernel tasks.
//!
//! The core is Rust because everything here is process and VM lifecycle:
//! spawning children that can hang, killing process groups, capturing serial
//! consoles, classifying faults. Python drives it.


pub mod proc;
