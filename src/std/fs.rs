//! Forwarding stub for `std::fs`.

pub use std::fs::{
    self, DirEntry, File, Metadata, OpenOptions, ReadDir, canonicalize, copy, create_dir_all,
    metadata, read, read_dir, read_to_string, remove_dir, remove_dir_all, remove_file, write,
};
pub use std::io::{self, Read, Write};
