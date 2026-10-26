//! [`Io`]: the handle runtimes hold. Cheap to clone; clones share mounts.

use std::io;
use std::path::Path;
use std::sync::Arc;

use ply_storage::{DirItem, Entry, Storage, normalize};

use crate::namespace::{Mount, Namespace, Resolved, synthetic_entry, synthetic_items};
use crate::{MountInfo, MountMode};

#[derive(Clone, Default)]
pub struct Io {
    pub(crate) ns: Arc<Namespace>,
}

impl Io {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- mounts ----

    pub fn mount(&self, at: &str, storage: Arc<dyn Storage>, mode: MountMode) -> io::Result<MountInfo> {
        self.ns.mount(at, storage, mode)
    }

    pub fn unmount(&self, at: &str) -> io::Result<MountInfo> {
        self.ns.unmount(at)
    }

    /// Point `at` at a different storage (disk ↔ memory ↔ database) in one step.
    pub fn swap(&self, at: &str, storage: Arc<dyn Storage>, mode: MountMode) -> io::Result<MountInfo> {
        self.ns.swap(at, storage, mode)
    }

    pub fn mounts(&self) -> Vec<MountInfo> {
        self.ns.list()
    }

    // ---- file operations ----

    pub async fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        let path = normalize(Path::new(path))?;
        match self.ns.resolve(&path) {
            Resolved::Mounted { mount, inner } => mount.storage.read(Path::new(&inner)).await,
            Resolved::Synthetic { .. } => Err(is_a_dir(&path)),
            Resolved::Missing => Err(not_found(&path)),
        }
    }

    pub async fn write(&self, path: &str, data: &[u8]) -> io::Result<()> {
        let (mount, inner) = self.writable(path)?;
        mount.storage.write(Path::new(&inner), data).await
    }

    pub async fn append(&self, path: &str, data: &[u8]) -> io::Result<()> {
        let (mount, inner) = self.writable(path)?;
        mount.storage.append(Path::new(&inner), data).await
    }

    pub async fn mkdir(&self, path: &str, recursive: bool) -> io::Result<()> {
        let (mount, inner) = self.writable(path)?;
        mount.storage.mkdir(Path::new(&inner), recursive).await
    }

    pub async fn remove(&self, path: &str, recursive: bool) -> io::Result<()> {
        let (mount, inner) = self.writable(path)?;
        if inner == "/" {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{}: is a mount point", mount.at)));
        }
        mount.storage.remove(Path::new(&inner), recursive).await
    }

    pub async fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        let (mount, inner_from) = self.writable(from)?;
        let to_path = normalize(Path::new(to))?;
        match self.ns.resolve(&to_path) {
            Resolved::Mounted { mount: dst, inner } if dst.at == mount.at => {
                if dst.mode == MountMode::ReadOnly {
                    Err(read_only(&to_path))
                } else if inner_from == "/" || inner == "/" {
                    Err(io::Error::new(io::ErrorKind::PermissionDenied, "cannot rename a mount point"))
                } else {
                    mount.storage.rename(Path::new(&inner_from), Path::new(&inner)).await
                }
            }
            Resolved::Mounted { .. } => {
                Err(io::Error::new(io::ErrorKind::CrossesDevices, format!("{from} -> {to_path}: crosses mounts")))
            }
            _ => Err(outside_mounts(&to_path)),
        }
    }

    pub async fn stat(&self, path: &str) -> io::Result<Entry> {
        let path = normalize(Path::new(path))?;
        match self.ns.resolve(&path) {
            Resolved::Mounted { mount, inner } => mount.storage.stat(Path::new(&inner)).await,
            Resolved::Synthetic { .. } => Ok(synthetic_entry()),
            Resolved::Missing => Err(not_found(&path)),
        }
    }

    pub async fn list(&self, path: &str) -> io::Result<Vec<DirItem>> {
        let path = normalize(Path::new(path))?;
        match self.ns.resolve(&path) {
            Resolved::Mounted { mount, inner } => {
                let mut items = mount.storage.list(Path::new(&inner)).await?;
                // Mount points below this directory show up even if the
                // storage itself has no such entry.
                for name in self.ns.mount_children(&path) {
                    if !items.iter().any(|i| i.name == name) {
                        items.extend(synthetic_items(vec![name]));
                    }
                }
                items.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(items)
            }
            Resolved::Synthetic { children } => Ok(synthetic_items(children)),
            Resolved::Missing => Err(not_found(&path)),
        }
    }

    pub async fn exists(&self, path: &str) -> bool {
        self.stat(path).await.is_ok()
    }

    /// The mount a write lands on, refusing read-only mounts and paths
    /// outside every mount.
    fn writable(&self, path: &str) -> io::Result<(Mount, String)> {
        let path = normalize(Path::new(path))?;
        match self.ns.resolve(&path) {
            Resolved::Mounted { mount, inner } if mount.mode == MountMode::ReadWrite => Ok((mount, inner)),
            Resolved::Mounted { .. } => Err(read_only(&path)),
            _ => Err(outside_mounts(&path)),
        }
    }
}

fn not_found(path: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{path}: No such file or directory"))
}

fn is_a_dir(path: &str) -> io::Error {
    io::Error::new(io::ErrorKind::IsADirectory, format!("{path}: Is a directory"))
}

fn read_only(path: &str) -> io::Error {
    io::Error::new(io::ErrorKind::ReadOnlyFilesystem, format!("{path}: Read-only file system"))
}

pub(crate) fn outside_mounts(path: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, format!("{path}: not on any mounted storage"))
}
