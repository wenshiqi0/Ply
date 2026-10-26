//! In-process tree. Used as the sandbox's ephemeral scratch space and as a
//! mount target that should vanish with the process.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::Mutex;

use crate::{KvBackend, KvStorage, Node, StorageInfo, normalize, parent_of};

#[derive(Default)]
pub struct MemoryKv {
    nodes: Mutex<BTreeMap<String, Node>>,
}

pub type MemoryStorage = KvStorage<MemoryKv>;

impl MemoryStorage {
    pub fn empty() -> Self {
        KvStorage::new(MemoryKv::default()).expect("in-memory storage cannot fail")
    }

    /// A tree holding `files` (storage paths → bytes), with every parent
    /// directory created. Used to ship files inside the binary.
    pub fn with_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Self {
        let storage = Self::empty();
        let kv = storage.backend();
        for (path, data) in files {
            let key = normalize(Path::new(path)).expect("embedded file paths are well-formed");
            let mut dir = parent_of(&key);
            while let Some(d) = dir {
                if kv.get(d).ok().flatten().is_none() {
                    let _ = kv.put(d, Node::dir());
                }
                dir = parent_of(d);
            }
            let _ = kv.put(&key, Node::file(data.to_vec()));
        }
        storage
    }
}

fn below(key: &str) -> String {
    if key == "/" { "/".to_string() } else { format!("{key}/") }
}

impl KvBackend for MemoryKv {
    fn info(&self) -> StorageInfo {
        StorageInfo { kind: "memory".into(), location: "process memory".into() }
    }

    fn get(&self, key: &str) -> io::Result<Option<Node>> {
        Ok(self.nodes.lock().expect("memory kv poisoned").get(key).cloned())
    }

    fn put(&self, key: &str, node: Node) -> io::Result<()> {
        self.nodes.lock().expect("memory kv poisoned").insert(key.to_string(), node);
        Ok(())
    }

    fn delete_tree(&self, key: &str) -> io::Result<()> {
        let prefix = below(key);
        self.nodes
            .lock()
            .expect("memory kv poisoned")
            .retain(|k, _| k != key && !k.starts_with(&prefix));
        Ok(())
    }

    fn descendants(&self, key: &str) -> io::Result<Vec<(String, Node)>> {
        let prefix = below(key);
        Ok(self
            .nodes
            .lock()
            .expect("memory kv poisoned")
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .filter(|(k, _)| k.as_str() != key)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }
}
