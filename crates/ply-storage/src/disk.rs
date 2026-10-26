//! A host directory as storage. The only backend that touches the real disk.
//!
//! Containment: a storage path is normalized (no `..` above `/`), joined under
//! the canonical root, and then the deepest existing ancestor is canonicalized
//! and must still be inside the root. That blocks `..` tricks and symlinks
//! (existing or dangling) that point outside. Races with other host processes
//! that swap a directory for a symlink between check and use are out of scope:
//! the mounted directory is assumed to be owned by the host, not the agent.

use std::io;
use std::path::{Path, PathBuf};

use crate::{DirItem, Entry, EntryKind, Storage, StorageInfo, denied, normalize};

pub struct DiskStorage {
    root: PathBuf,
}

impl DiskStorage {
    /// `root` must be an existing directory.
    pub fn open(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = std::fs::canonicalize(root.as_ref())?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{}: not a directory", root.display()),
            ));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    async fn host_path(&self, path: &Path) -> io::Result<PathBuf> {
        let rel = normalize(path)?;
        let joined = self.root.join(rel.trim_start_matches('/'));

        if let Ok(meta) = tokio::fs::symlink_metadata(&joined).await
            && meta.file_type().is_symlink()
            && tokio::fs::canonicalize(&joined).await.is_err()
        {
            return Err(denied(format!("{rel}: dangling symlink")));
        }

        let mut probe = joined.clone();
        loop {
            match tokio::fs::canonicalize(&probe).await {
                Ok(real) if real.starts_with(&self.root) => break,
                Ok(_) => {
                    return Err(denied(format!("{rel}: resolves outside the mounted directory")));
                }
                Err(_) if probe.pop() => continue,
                Err(err) => return Err(err),
            }
        }
        Ok(joined)
    }

    fn entry(meta: &std::fs::Metadata) -> Entry {
        Entry {
            kind: if meta.is_dir() { EntryKind::Dir } else { EntryKind::File },
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified: meta.modified().unwrap_or(std::time::UNIX_EPOCH),
        }
    }
}

#[async_trait::async_trait]
impl Storage for DiskStorage {
    fn info(&self) -> StorageInfo {
        StorageInfo { kind: "disk".into(), location: self.root.display().to_string() }
    }

    fn host_root(&self) -> Option<&Path> {
        Some(&self.root)
    }

    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        tokio::fs::read(self.host_path(path).await?).await
    }

    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        tokio::fs::write(self.host_path(path).await?, data).await
    }

    async fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        use tokio::io::AsyncWriteExt;
        let target = self.host_path(path).await?;
        let mut file =
            tokio::fs::OpenOptions::new().create(true).append(true).open(target).await?;
        file.write_all(data).await?;
        file.flush().await
    }

    async fn mkdir(&self, path: &Path, recursive: bool) -> io::Result<()> {
        let target = self.host_path(path).await?;
        if recursive {
            tokio::fs::create_dir_all(target).await
        } else {
            tokio::fs::create_dir(target).await
        }
    }

    async fn remove(&self, path: &Path, recursive: bool) -> io::Result<()> {
        let target = self.host_path(path).await?;
        if target == self.root {
            return Err(denied("refusing to remove the storage root"));
        }
        let meta = tokio::fs::symlink_metadata(&target).await?;
        if meta.is_dir() {
            if recursive {
                tokio::fs::remove_dir_all(target).await
            } else {
                tokio::fs::remove_dir(target).await
            }
        } else {
            tokio::fs::remove_file(target).await
        }
    }

    async fn stat(&self, path: &Path) -> io::Result<Entry> {
        let meta = tokio::fs::metadata(self.host_path(path).await?).await?;
        Ok(Self::entry(&meta))
    }

    async fn list(&self, path: &Path) -> io::Result<Vec<DirItem>> {
        let dir = self.host_path(path).await?;
        let mut reader = tokio::fs::read_dir(dir).await?;
        let mut items = Vec::new();
        while let Some(item) = reader.next_entry().await? {
            // Hide anything (e.g. a symlink) whose real location is outside the root.
            match tokio::fs::canonicalize(item.path()).await {
                Ok(real) if real.starts_with(&self.root) => {}
                _ => continue,
            }
            let Ok(meta) = tokio::fs::metadata(item.path()).await else { continue };
            let Some(name) = item.file_name().to_str().map(str::to_string) else { continue };
            items.push(DirItem { name, entry: Self::entry(&meta) });
        }
        items.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(items)
    }

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let src = self.host_path(from).await?;
        let dst = self.host_path(to).await?;
        if src == self.root || dst == self.root {
            return Err(denied("refusing to rename the storage root"));
        }
        tokio::fs::rename(src, dst).await
    }
}
