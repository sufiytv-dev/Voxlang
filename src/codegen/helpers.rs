// helpers.rs - Small, pure utilities used by several modules.
//
// Extracted from the original utils.rs.
// Contains the `create_temp_file` helper and `sanitize_type_name` for
// generating safe LLVM identifiers from Vox type names.
//
// NEW (2026-06-05): Added `expand_type_aliases` method to CodegenEngine
// for recursively expanding type aliases before LLVM mapping.

use crate::codegen::CodegenEngine;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io;
use std::path::PathBuf;

/// Creates a temporary file with a unique name in the system temporary directory.
/// Returns a tuple `(path, file)` where the file is opened for read/write
/// and is created with exclusive access (fails if the file already exists).
///
/// The file will be deleted when the `File` handle is closed.
pub fn create_temp_file(prefix: &str, suffix: &str) -> io::Result<(PathBuf, std::fs::File)> {
    let temp_dir = std::env::temp_dir();
    let pid = std::process::id();
    let mut counter = 0;
    loop {
        let name = format!("{}_{}_{}{}", prefix, pid, counter, suffix);
        let path = temp_dir.join(name);
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => counter += 1,
            Err(e) => return Err(e),
        }
    }
}

/// Sanitize a Vox type name for use as part of an LLVM identifier.
/// Replaces `&`, `*`, `<`, `>`, and spaces with safe alternatives.
pub fn sanitize_type_name(s: &str) -> String {
    s.replace("&", "ref_")
        .replace("*", "ptr")
        .replace('<', "_LT_")
        .replace('>', "_GT_")
        .replace(' ', "")
}

impl CodegenEngine {
    /// Recursively expand type aliases in a type string.
    /// Uses the `type_aliases` map stored in the engine (populated from semantic analysis).
    /// Returns the fully expanded type, or the original string if no alias matches.
    pub fn expand_type_aliases(&self, ty: &str) -> String {
        let mut current = ty.to_string();
        let mut changed = true;
        while changed {
            changed = false;
            if let Some(expanded) = self.type_aliases.get(&current) {
                current = expanded.clone();
                changed = true;
            }
        }
        current
    }
}
