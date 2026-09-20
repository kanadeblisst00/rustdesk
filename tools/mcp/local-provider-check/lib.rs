//! Compile and test the actual local providers without RustDesk's audio/video dependencies.
#![allow(dead_code)]
extern crate self as hbb_common;
pub use base64;
pub use libc;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "../../../src/platform/local_agent_unix/mod.rs"]
mod local_agent_unix;
