#![deny(unsafe_op_in_unsafe_fn)]

//! Bounded transport for the versioned `LocalSearch` Agent Wire contract.

#[cfg(windows)]
pub mod windows_pipe;
