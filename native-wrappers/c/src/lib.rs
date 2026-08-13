//! Stable C ABI for the dual-protocol rumqttc native client.
//!
//! # Safety
//!
//! Except for the pointer-free version queries, exported functions are unsafe to call from Rust.
//! Callers must satisfy the ownership, lifetime, alignment, readability, and writability contracts
//! documented in `rumqttc.h` for every non-null pointer.
//!
//! ```compile_fail
//! use rumqttc::rumqttc_config_destroy;
//!
//! rumqttc_config_destroy(std::ptr::null_mut());
//! ```

#![deny(unsafe_op_in_unsafe_fn)]

pub(crate) mod client;
pub(crate) mod completion;
pub(crate) mod config;
pub(crate) mod error;
pub(crate) mod event;
mod ffi;
pub(crate) mod panic;

pub use ffi::*;
