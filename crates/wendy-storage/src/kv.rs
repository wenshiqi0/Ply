//! Tree semantics on top of any key/value store.
//!
//! Keys are normalized absolute paths (`/`, `/a`, `/a/b.txt`). A directory is
//! a node with `dir = true`; files carry their bytes. Multi-key operations
//! (recursive remove, rename) are not transactional across backends: a crash
//! mid-rename can leave both copies. Backends that can do better (sqlite)
//! may override by wrapping their own transaction around these calls later.

use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{
    DirItem, Entry, EntryKind, Storage, StorageInfo, denied, name_of, normalize, not_found,
    parent_of,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub dir: bool,
    pub data: Vec<u8>,
    pub modified_ms: i64,
}

impl Node {
    pub fn dir() -> Self {
        Node { dir: true, data: Vec::new(), modified_ms: now_ms() }
    }

    pub fn file(data: Vec<u8>) -> Self {
        Node { dir: false, data, modified_ms: now_ms() }
    }

    fn entry(&self) -> Entry {
        Entry {
            kind: if self.dir { EntryKind::Dir } else { EntryKind::File },
            size: self.data.len() as u64,
            modified: UNIX_EPOCH + Duration::from_millis(self.modified_ms.max(0) as u64),
        }
    }
}

/// The minimum a key/value store must provide to back a mount.
pub trait KvBackend: Send + Sync + 'static {
    fn info(&self) -> StorageInfo;
    fn get(&self, key: &str) -> io::Result<Option<Node>>;
    fn put(&self, key: &str, node: Node) -> io::Result<()>;
    /// Remove `key` and every key below it.
    fn delete_tree(&self, key: &str) -> io::Result<()>;
    /// Every key strictly below directory `key`, at any depth.
    fn descendants(&self, key: &str) -> io::Result<Vec<(String, Node)>>;
}

/// Adapts a [`KvBackend`] into a [`Storage`].
pub struct KvStorage<B: KvBackend> {
    backend: B,
}

impl<B: KvBackend> KvStorage<B> {
    pub fn new(backend: B) -> io::Result<Self> {
        if backend.get("/")?.is_none() {
            backend.put("/", Node::dir())?;
        }
        Ok(Self { backend })
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    fn require_dir(&self, key: &str) -> io::Result<()> {
        match self.backend.get(key)? {
            Some(node) if node.dir => Ok(()),
            Some(_) => Err(io::Error::new(io::ErrorKind::NotADirectory, format!("{key}: not a directory"))),
            None => Err(not_found(key)),
        }
    }

    fn require_parent(&self, key: &str) -> io::Result<()> {
        match parent_of(key) {
            Some(parent) => self.require_dir(parent),
            None => Ok(()),
        }
    }
}

#[async_trait::async_trait]
impl<B: KvBackend> Storage for KvStorage<B> {
    fn info(&self) -> StorageInfo {
        self.backend.info()
    }

    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let key = normalize(path)?;
        match self.backend.get(&key)? {
            Some(node) if node.dir => Err(is_a_dir(&key)),
            Some(node) => Ok(node.data),
            None => Err(not_found(&key)),
        }
    }

    async fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let key = normalize(path)?;
        self.require_parent(&key)?;
        if matches!(self.backend.get(&key)?, Some(node) if node.dir) {
            return Err(is_a_dir(&key));
        }
        self.backend.put(&key, Node::file(data.to_vec()))
    }

    async fn append(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let key = normalize(path)?;
        self.require_parent(&key)?;
        let mut bytes = match self.backend.get(&key)? {
            Some(node) if node.dir => return Err(is_a_dir(&key)),
            Some(node) => node.data,
            None => Vec::new(),
        };
        bytes.extend_from_slice(data);
        self.backend.put(&key, Node::file(bytes))
    }

    async fn mkdir(&self, path: &Path, recursive: bool) -> io::Result<()> {
        let key = normalize(path)?;
        if let Some(node) = self.backend.get(&key)? {
            return if node.dir && recursive {
                Ok(())
            } else {
                Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{key}: already exists")))
            };
        }
        if recursive {
            let mut chain = Vec::new();
            let mut cursor = parent_of(&key);
            while let Some(dir) = cursor {
                match self.backend.get(dir)? {
                    Some(node) if node.dir => break,
                    Some(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::NotADirectory,
                            format!("{dir}: not a directory"),
                        ));
                    }
                    None => chain.push(dir.to_string()),
                }
                cursor = parent_of(dir);
            }
            for dir in chain.iter().rev() {
                self.backend.put(dir, Node::dir())?;
            }
        } else {
            self.require_parent(&key)?;
        }
        self.backend.put(&key, Node::dir())
    }

    async fn remove(&self, path: &Path, recursive: bool) -> io::Result<()> {
        let key = normalize(path)?;
        if key == "/" {
            return Err(denied("refusing to remove the storage root"));
        }
        let node = self.backend.get(&key)?.ok_or_else(|| not_found(&key))?;
        if node.dir && !recursive && !self.backend.descendants(&key)?.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::DirectoryNotEmpty,
                format!("{key}: directory not empty"),
            ));
        }
        self.backend.delete_tree(&key)
    }

    async fn stat(&self, path: &Path) -> io::Result<Entry> {
        let key = normalize(path)?;
        self.backend.get(&key)?.map(|n| n.entry()).ok_or_else(|| not_found(&key))
    }

    async fn list(&self, path: &Path) -> io::Result<Vec<DirItem>> {
        let key = normalize(path)?;
        self.require_dir(&key)?;
        let mut items: Vec<DirItem> = self
            .backend
            .descendants(&key)?
            .into_iter()
            .filter(|(child, _)| parent_of(child) == Some(key.as_str()))
            .map(|(child, node)| DirItem { name: name_of(&child).to_string(), entry: node.entry() })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(items)
    }

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let src = normalize(from)?;
        let dst = normalize(to)?;
        if src == "/" || dst == "/" {
            return Err(denied("refusing to rename the storage root"));
        }
        if dst.starts_with(&format!("{src}/")) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot move a directory into itself"));
        }
        let node = self.backend.get(&src)?.ok_or_else(|| not_found(&src))?;
        self.require_parent(&dst)?;
        let below = if node.dir { self.backend.descendants(&src)? } else { Vec::new() };
        self.backend.delete_tree(&dst)?;
        self.backend.put(&dst, node)?;
        for (child, child_node) in below {
            let moved = format!("{dst}{}", &child[src.len()..]);
            self.backend.put(&moved, child_node)?;
        }
        self.backend.delete_tree(&src)
    }
}

fn is_a_dir(key: &str) -> io::Error {
    io::Error::new(io::ErrorKind::IsADirectory, format!("{key}: is a directory"))
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}
