//! Minimal walkdir replacement using our own fs::read_dir.

use crate::std::collections::Vec;
use crate::std::ffi::OsStr;
use crate::std::fs;
use crate::std::path::{Path, PathBuf};

pub struct WalkDir {
    root: PathBuf,
    follow_links: bool,
}

impl WalkDir {
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        WalkDir {
            root: root.as_ref().to_path_buf(),
            follow_links: false,
        }
    }

    pub fn follow_links(mut self, yes: bool) -> Self {
        self.follow_links = yes;
        self
    }

    pub fn into_iter(self) -> WalkDirIter {
        WalkDirIter::new(self.root, self.follow_links)
    }
}

pub struct WalkDirIter {
    stack: Vec<PathBuf>,
    _follow_links: bool,
}

impl WalkDirIter {
    fn new(root: PathBuf, follow_links: bool) -> Self {
        let mut stack = Vec::new();
        stack.push(root);
        WalkDirIter {
            stack,
            _follow_links: follow_links,
        }
    }
}

impl Iterator for WalkDirIter {
    type Item = std::io::Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(path) = self.stack.pop() {
            // If it's a file, yield it immediately.
            if path.is_file() {
                return Some(Ok(DirEntry { path }));
            }
            // If it's a directory, read its entries and push them onto stack.
            if path.is_dir() {
                match fs::read_dir(&path) {
                    Ok(entries) => {
                        for entry in entries {
                            match entry {
                                Ok(e) => {
                                    let child = e.path();
                                    self.stack.push(child);
                                }
                                Err(e) => return Some(Err(e)),
                            }
                        }
                    }
                    Err(e) => return Some(Err(e)),
                }
            }
        }
        None
    }
}

pub struct DirEntry {
    path: PathBuf,
}

impl DirEntry {
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }
    pub fn file_name(&self) -> &OsStr {
        self.path.file_name().unwrap()
    }
}
