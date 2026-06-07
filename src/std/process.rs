//! Forwarding stub for `std::process`.

pub use std::process::exit;
pub use std::process::{Child, Command, ExitStatus, Output, Stdio}; // Add this for `std::process::exit(1)`
