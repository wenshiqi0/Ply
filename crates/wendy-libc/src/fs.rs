//! cwd-relative file operations over the IO layer.
//!
//! These are the building blocks for both outlets: `rm -r` and
//! `fs.rmSync(p, {recursive: true})` both call [`Fs::remove_all`]. Every
//! method resolves against the current directory, then goes through
//! [`Io`].

use wendy_io::{DirItem, Entry, EntryKind, Io};

use crate::errno::{Errno, SysError};
use crate::path;

pub type Result<T> = std::result::Result<T, SysError>;

#[derive(Clone)]
pub struct Fs {
    io: Io,
    cwd: String,
}

impl Fs {
    pub fn new(io: Io, cwd: impl Into<String>) -> Self {
        Fs { io, cwd: path::normalize(&cwd.into()) }
    }

    pub fn io(&self) -> &Io {
        &self.io
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    pub fn with_cwd(&self, cwd: &str) -> Fs {
        Fs { io: self.io.clone(), cwd: self.resolve(cwd) }
    }

    pub fn resolve(&self, p: &str) -> String {
        path::resolve(&self.cwd, p)
    }

    pub async fn read(&self, p: &str) -> Result<Vec<u8>> {
        Ok(self.io.read(&self.resolve(p)).await?)
    }

    pub async fn write(&self, p: &str, data: &[u8]) -> Result<()> {
        Ok(self.io.write(&self.resolve(p), data).await?)
    }

    pub async fn append(&self, p: &str, data: &[u8]) -> Result<()> {
        Ok(self.io.append(&self.resolve(p), data).await?)
    }

    pub async fn stat(&self, p: &str) -> Result<Entry> {
        Ok(self.io.stat(&self.resolve(p)).await?)
    }

    pub async fn exists(&self, p: &str) -> bool {
        self.stat(p).await.is_ok()
    }

    pub async fn is_dir(&self, p: &str) -> bool {
        self.stat(p).await.is_ok_and(|e| e.kind == EntryKind::Dir)
    }

    pub async fn is_file(&self, p: &str) -> bool {
        self.stat(p).await.is_ok_and(|e| e.kind == EntryKind::File)
    }

    pub async fn read_dir(&self, p: &str) -> Result<Vec<DirItem>> {
        Ok(self.io.list(&self.resolve(p)).await?)
    }

    /// `parents`: create missing ancestors and accept an existing directory
    /// (`mkdir -p`, `{recursive: true}`).
    pub async fn mkdir(&self, p: &str, parents: bool) -> Result<()> {
        Ok(self.io.mkdir(&self.resolve(p), parents).await?)
    }

    /// Remove a non-directory.
    pub async fn remove_file(&self, p: &str) -> Result<()> {
        let abs = self.resolve(p);
        if self.io.stat(&abs).await?.kind == EntryKind::Dir {
            return Err(SysError::new(Errno::EISDIR, abs));
        }
        Ok(self.io.remove(&abs, false).await?)
    }

    /// Remove an empty directory.
    pub async fn remove_dir(&self, p: &str) -> Result<()> {
        let abs = self.resolve(p);
        if self.io.stat(&abs).await?.kind != EntryKind::Dir {
            return Err(SysError::new(Errno::ENOTDIR, abs));
        }
        Ok(self.io.remove(&abs, false).await?)
    }

    /// Remove a file or a whole tree.
    pub async fn remove_all(&self, p: &str) -> Result<()> {
        Ok(self.io.remove(&self.resolve(p), true).await?)
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<()> {
        Ok(self.io.rename(&self.resolve(from), &self.resolve(to)).await?)
    }

    pub async fn copy_file(&self, from: &str, to: &str) -> Result<()> {
        let data = self.read(from).await?;
        self.write(to, &data).await
    }

    /// Copy a file or directory tree; `to` is the new root's path.
    pub async fn copy_tree(&self, from: &str, to: &str) -> Result<()> {
        let src = self.resolve(from);
        let dst = self.resolve(to);
        if dst == src || dst.starts_with(&format!("{src}/")) {
            return Err(SysError::new(Errno::EINVAL, format!("cannot copy {src} into itself")));
        }
        for (abs, entry) in self.walk(&src).await? {
            let target = format!("{dst}{}", &abs[src.len()..]);
            match entry.kind {
                EntryKind::Dir => self.io.mkdir(&target, true).await?,
                EntryKind::File => {
                    let data = self.io.read(&abs).await?;
                    self.io.write(&target, &data).await?;
                }
            }
        }
        Ok(())
    }

    /// The tree under `p` in pre-order (the root first), entries sorted by
    /// name. Paths are absolute.
    pub async fn walk(&self, p: &str) -> Result<Vec<(String, Entry)>> {
        let root = self.resolve(p);
        let mut out = Vec::new();
        let mut stack = vec![(root.clone(), self.io.stat(&root).await?)];
        while let Some((abs, entry)) = stack.pop() {
            let is_dir = entry.kind == EntryKind::Dir;
            out.push((abs.clone(), entry));
            if is_dir {
                let children = self.io.list(&abs).await?;
                for child in children.into_iter().rev() {
                    stack.push((path::join(&abs, &child.name), child.entry));
                }
            }
        }
        Ok(out)
    }
}
