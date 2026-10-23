//! The mount table: which storage answers for which path.
//!
//! Mounts can be added, removed and swapped at any time; the runtimes only
//! ever see paths. Longest mount point wins. A path that is not inside any
//! mount but is an ancestor of one (always true for `/`) is a synthetic,
//! read-only directory whose children are the next mount-point components.

use std::io;
use std::path::Path;
use std::sync::{Arc, RwLock};

use serde::Serialize;
use wendy_storage::{DirItem, Entry, EntryKind, Storage, StorageInfo, normalize};

use crate::gate::under;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MountMode {
    #[serde(rename = "ro")]
    ReadOnly,
    #[serde(rename = "rw")]
    ReadWrite,
}

#[derive(Debug, Clone, Serialize)]
pub struct MountInfo {
    pub at: String,
    pub mode: MountMode,
    pub storage: StorageInfo,
}

#[derive(Clone)]
pub(crate) struct Mount {
    pub at: String,
    pub mode: MountMode,
    pub storage: Arc<dyn Storage>,
}

impl Mount {
    pub fn info(&self) -> MountInfo {
        MountInfo { at: self.at.clone(), mode: self.mode, storage: self.storage.info() }
    }
}

pub(crate) enum Resolved {
    /// Inside a mount; `inner` is the path within that storage.
    Mounted { mount: Mount, inner: String },
    /// An ancestor of mount points, not itself backed by storage.
    Synthetic { children: Vec<String> },
    Missing,
}

#[derive(Default)]
pub struct Namespace {
    mounts: RwLock<Vec<Mount>>,
}

impl Namespace {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount `storage` at `at`. Fails if something is already mounted there.
    pub fn mount(&self, at: &str, storage: Arc<dyn Storage>, mode: MountMode) -> io::Result<MountInfo> {
        let at = normalize(Path::new(at))?;
        let mut mounts = self.mounts.write().expect("mount table poisoned");
        if mounts.iter().any(|m| m.at == at) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{at}: already a mount point")));
        }
        let mount = Mount { at, mode, storage };
        let info = mount.info();
        mounts.push(mount);
        mounts.sort_by_key(|m| std::cmp::Reverse(m.at.len()));
        Ok(info)
    }

    pub fn unmount(&self, at: &str) -> io::Result<MountInfo> {
        let at = normalize(Path::new(at))?;
        let mut mounts = self.mounts.write().expect("mount table poisoned");
        let index = mounts
            .iter()
            .position(|m| m.at == at)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{at}: not a mount point")))?;
        Ok(mounts.remove(index).info())
    }

    /// Replace whatever is mounted at `at` (or mount fresh) in one step.
    pub fn swap(&self, at: &str, storage: Arc<dyn Storage>, mode: MountMode) -> io::Result<MountInfo> {
        let at = normalize(Path::new(at))?;
        let mut mounts = self.mounts.write().expect("mount table poisoned");
        mounts.retain(|m| m.at != at);
        let mount = Mount { at, mode, storage };
        let info = mount.info();
        mounts.push(mount);
        mounts.sort_by_key(|m| std::cmp::Reverse(m.at.len()));
        Ok(info)
    }

    /// Every mount, longest mount point first.
    pub(crate) fn snapshot(&self) -> Vec<Mount> {
        self.mounts.read().expect("mount table poisoned").clone()
    }

    pub fn list(&self) -> Vec<MountInfo> {
        let mut infos: Vec<MountInfo> =
            self.mounts.read().expect("mount table poisoned").iter().map(Mount::info).collect();
        infos.sort_by(|a, b| a.at.cmp(&b.at));
        infos
    }

    pub(crate) fn resolve(&self, path: &str) -> Resolved {
        let mounts = self.mounts.read().expect("mount table poisoned");
        if let Some(mount) = mounts.iter().find(|m| under(path, &m.at)) {
            let inner = if mount.at == "/" { path.to_string() } else { path[mount.at.len()..].to_string() };
            let inner = if inner.is_empty() { "/".to_string() } else { inner };
            return Resolved::Mounted { mount: mount.clone(), inner };
        }
        let children = mount_children(&mounts, path);
        if children.is_empty() && path != "/" { Resolved::Missing } else { Resolved::Synthetic { children } }
    }

    /// Mount-point names directly below `path` (to merge into listings).
    pub(crate) fn mount_children(&self, path: &str) -> Vec<String> {
        mount_children(&self.mounts.read().expect("mount table poisoned"), path)
    }
}

fn mount_children(mounts: &[Mount], path: &str) -> Vec<String> {
    let mut names: Vec<String> = mounts
        .iter()
        .filter(|m| m.at != path && under(&m.at, path))
        .filter_map(|m| {
            let rest = if path == "/" { &m.at[1..] } else { &m.at[path.len() + 1..] };
            rest.split('/').next().map(str::to_string)
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

pub(crate) fn synthetic_entry() -> Entry {
    Entry { kind: EntryKind::Dir, size: 0, modified: std::time::UNIX_EPOCH }
}

pub(crate) fn synthetic_items(children: Vec<String>) -> Vec<DirItem> {
    children.into_iter().map(|name| DirItem { name, entry: synthetic_entry() }).collect()
}
