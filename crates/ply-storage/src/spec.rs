//! Storage chosen by configuration, so switching a mount is a config change.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{DiskStorage, MemoryStorage, SqliteStorage, Storage};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum StorageSpec {
    /// An existing host directory.
    Disk { root: PathBuf },
    /// A fresh, empty in-memory tree.
    Memory,
    /// A SQLite database file (created if missing).
    Sqlite { path: PathBuf },
}

impl StorageSpec {
    /// Open the storage; relative paths resolve against `base`.
    pub fn open(&self, base: &Path) -> io::Result<Arc<dyn Storage>> {
        Ok(match self {
            StorageSpec::Disk { root } => Arc::new(DiskStorage::open(base.join(root))?),
            StorageSpec::Memory => Arc::new(MemoryStorage::empty()),
            StorageSpec::Sqlite { path } => Arc::new(SqliteStorage::open(base.join(path))?),
        })
    }
}
