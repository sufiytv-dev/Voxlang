//! Vox collections module – forwarding to Rust’s standard library.

pub mod hashmap;
pub mod hashset;
pub mod vec;

pub use hashmap::HashMap;
pub use hashset::HashSet;
pub use vec::Vec;
