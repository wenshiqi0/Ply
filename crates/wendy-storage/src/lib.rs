//! Storage that sandbox mounts point at.
//!
//! Important decisions:
//! - This crate knows nothing about runtimes, tracing or policy. A
//!   [`Storage`] is just a rooted tree of files and directories; wendy-io
//!   mounts it and wraps it with the gate and the trace. That is what lets a
//!   mount move from a host directory to an in-memory tree to a database
//!   without the execution side noticing.
//! - Every path a storage sees is absolute *within that storage* (`/` is the
//!   storage root), never a host path. [`DiskStorage`] maps it under its root
//!   and refuses anything that resolves outside, including through symlinks.
//! - Key/value style backends (memory, sqlite, anything else) implement the
//!   small synchronous [`KvBackend`] and get the full tree semantics from
//!   [`KvStorage`], so a new database backend is ~4 methods.
//! - Errors are plain `std::io::Error` with POSIX-ish kinds, which is what
//!   shells and the interpreter already understand.

mod disk;
mod kv;
mod memory;
mod spec;
mod sqlite;

use std::io;
use std::path::Path;
use std::time::SystemTime;

pub use disk::DiskStorage;
pub use kv::{KvBackend, KvStorage, Node};
pub use memory::{MemoryKv, MemoryStorage};
pub use spec::StorageSpec;
pub use sqlite::{SqliteKv, SqliteStorage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub kind: EntryKind,
    pub size: u64,
    pub modified: SystemTime,
}

#[derive(Debug, Clone)]
pub struct DirItem {
    pub name: String,
    pub entry: Entry,
}

/// Human-readable identity of a storage, for mount listings and traces.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StorageInfo {
    pub kind: String,
    pub location: String,
}

/// A rooted tree of files and directories.
#[async_trait::async_trait]
pub trait Storage: Send + Sync {
    fn info(&self) -> StorageInfo;
    /// The host directory this storage *is*, when it is one. Real processes
    /// (which see host paths, not mounts) can only work on such storage.
    fn host_root(&self) -> Option<&Path> {
        None
    }
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// Create or replace a file. The parent directory must exist.
    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    /// Append to a file, creating it if missing.
    async fn append(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    async fn mkdir(&self, path: &Path, recursive: bool) -> io::Result<()>;
    async fn remove(&self, path: &Path, recursive: bool) -> io::Result<()>;
    async fn stat(&self, path: &Path) -> io::Result<Entry>;
    async fn list(&self, path: &Path) -> io::Result<Vec<DirItem>>;
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
}

/// Normalize a storage path to `/a/b` form. `..` may not climb above `/`.
pub fn normalize(path: &Path) -> io::Result<String> {
    use std::path::Component;
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(denied(format!("{} escapes the storage root", path.display())));
                }
            }
            Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "path is not valid UTF-8")
                })?;
                parts.push(part.to_string());
            }
        }
    }
    Ok(format!("/{}", parts.join("/")))
}

/// Parent of a normalized path; `None` for the root.
pub(crate) fn parent_of(path: &str) -> Option<&str> {
    if path == "/" {
        return None;
    }
    match path.rfind('/') {
        Some(0) => Some("/"),
        Some(i) => Some(&path[..i]),
        None => None,
    }
}

pub(crate) fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or("")
}

pub(crate) fn denied(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, msg.into())
}

pub(crate) fn not_found(path: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{path}: no such file or directory"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_refuses_to_climb_out() {
        assert_eq!(normalize(Path::new("/a/./b/../c")).unwrap(), "/a/c");
        assert_eq!(normalize(Path::new("a/b")).unwrap(), "/a/b");
        assert_eq!(normalize(Path::new("/")).unwrap(), "/");
        let err = normalize(Path::new("/a/../../etc")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn splits_parent_and_name() {
        assert_eq!(parent_of("/a/b"), Some("/a"));
        assert_eq!(parent_of("/a"), Some("/"));
        assert_eq!(parent_of("/"), None);
        assert_eq!(name_of("/a/b.txt"), "b.txt");
    }
}
