//! Private file-backed memory. A single atomic CURRENT pointer publishes a
//! complete generation; readers and writers share a cancellable process lock.
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::runtime::OpResult;
use ply_libc::Fs;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_NOTES: usize = 128;
const MAX_CONTENT: usize = 16 * 1024;
const MAX_TOTAL: usize = 1024 * 1024;
static SERIAL: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Note {
    id: String,
    key: String,
    version: u64,
    created_at: u64,
    updated_at: u64,
    source: Value,
    content_hash: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Forgotten {
    id: String,
    forgotten_at: u64,
    version: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    format: u32,
    identity: String,
    revision: u64,
    notes: Vec<Note>,
    forgotten: Vec<Forgotten>,
}
struct Snapshot {
    index: Index,
    contents: BTreeMap<String, String>,
}
impl Snapshot {
    fn empty(identity: &str) -> Self {
        Self {
            index: Index {
                format: 1,
                identity: identity.into(),
                revision: 0,
                notes: vec![],
                forgotten: vec![],
            },
            contents: BTreeMap::new(),
        }
    }
}
#[derive(Default)]
pub(crate) struct MemoryStore {
    directory: Mutex<Option<PathBuf>>,
}

fn hash(bytes: &[u8]) -> String {
    let mut value = 0xcbf29ce484222325u64;
    for byte in bytes {
        value = (value ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    format!("{value:016x}")
}
fn now() -> Result<u64, String> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis() as u64)
}
fn identifier() -> String {
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )
}
fn safe_id(value: &str) -> bool {
    (8..=96).contains(&value.len()) && value.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')
}
fn key(value: &Value) -> Result<String, String> {
    let value = value.as_str().ok_or("memory key must be a string")?.trim();
    if value.is_empty() || value.chars().count() > 128 || value.chars().any(char::is_control) {
        return Err("memory key must contain 1..128 printable characters".into());
    }
    Ok(value.into())
}
fn stopped(cancel: &dyn Fn() -> bool) -> Result<(), String> {
    if cancel() {
        Err("memory operation cancelled before publication".into())
    } else {
        Ok(())
    }
}
fn private_dir(path: &Path, create: bool) -> Result<bool, String> {
    if create {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !create => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(format!(
            "memory directory must be user-owned, private (0700), and not a symlink: {}",
            path.display()
        ));
    }
    Ok(true)
}
fn read(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("memory files must be user-owned and private (0600)".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("memory file exceeds its size limit".into());
    }
    Ok(bytes)
}
fn write(path: &Path, data: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(data)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}
fn git_marker(path: &Path) -> Result<String, std::io::Error> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > 8192 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid project git marker",
        ));
    }
    let mut bytes = Vec::new();
    file.take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "project git marker exceeds 8192 bytes",
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "project git marker must be UTF-8",
        )
    })
}
fn lock(scope: &Path, cancel: &dyn Fn() -> bool) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(scope.join(".lock"))
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("memory lock must be user-owned and private".into());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        stopped(cancel)?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(file);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(error.to_string());
        }
        if Instant::now() >= deadline {
            return Err("memory store is busy; retry the operation".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn project(fs: &Fs) -> Result<Option<String>, String> {
    let host = match fs.io().host_path(fs.cwd()) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    let cwd = fs::canonicalize(host).map_err(|e| e.to_string())?;
    for parent in cwd.ancestors() {
        let marker = parent.join(".git");
        let metadata = match fs::metadata(&marker) {
            Ok(value) => value,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
        };
        let git = if metadata.is_dir() {
            marker
        } else {
            if !metadata.is_file() || metadata.len() > 8192 {
                return Err("invalid project gitdir marker".into());
            }
            let text = git_marker(&marker).map_err(|e| e.to_string())?;
            let target = text
                .trim()
                .strip_prefix("gitdir:")
                .ok_or("invalid project gitdir marker")?
                .trim();
            parent.join(target)
        };
        let common = git.join("commondir");
        let git = match git_marker(&common) {
            Ok(text) if text.len() <= 8192 => git.join(text.trim()),
            Ok(_) => return Err("invalid project commondir marker".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => git,
            Err(e) => return Err(e.to_string()),
        };
        return Ok(Some(format!(
            "git:{}",
            fs::canonicalize(git).map_err(|e| e.to_string())?.display()
        )));
    }
    Ok(Some(format!("directory:{}", cwd.display())))
}
fn load(scope: &Path, identity: &str, cancel: &dyn Fn() -> bool) -> Result<Snapshot, String> {
    stopped(cancel)?;
    let current = scope.join("CURRENT");
    match fs::symlink_metadata(&current) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Snapshot::empty(identity));
        }
        Err(error) => return Err(error.to_string()),
    }
    let name = String::from_utf8(read(&current, 100)?).map_err(|_| "invalid memory generation")?;
    if !name.starts_with("g-") || !safe_id(&name[2..]) {
        return Err("invalid memory generation".into());
    }
    let generations = scope.join("generations");
    private_dir(&generations, false)?;
    let generation = generations.join(name);
    private_dir(&generation, false)?;
    let mut index: Index =
        serde_json::from_slice(&read(&generation.join("index.json"), MAX_TOTAL)?)
            .map_err(|_| "invalid memory index")?;
    if index.format != 1 {
        return Err("unsupported memory format: expected 1".into());
    }
    if index.identity != identity {
        return Err("memory project identity mismatch".into());
    }
    if index.notes.len() > MAX_NOTES
        || index.forgotten.len() > 4096
        || index.revision > 9_007_199_254_740_991
    {
        return Err("invalid memory index limits".into());
    }
    let mut ids = HashSet::new();
    let mut keys = HashSet::new();
    let mut contents = BTreeMap::new();
    let mut total = 0;
    private_dir(&generation.join("notes"), false)?;
    for note in &index.notes {
        stopped(cancel)?;
        if !safe_id(&note.id)
            || !ids.insert(note.id.clone())
            || !keys.insert(note.key.clone())
            || key(&json!(note.key))? != note.key
            || note.version == 0
            || note.version > 9_007_199_254_740_991
            || !note.source.is_object()
            || serde_json::to_vec(&note.source)
                .map_err(|e| e.to_string())?
                .len()
                > 4096
        {
            return Err("invalid memory entry".into());
        }
        let bytes = read(
            &generation.join("notes").join(format!("{}.md", note.id)),
            MAX_CONTENT,
        )?;
        total += bytes.len();
        if total > MAX_TOTAL {
            return Err("memory content exceeds 1 MiB".into());
        }
        if hash(&bytes) != note.content_hash {
            return Err("memory content changed outside the store; restore the managed files before using this scope".into());
        }
        contents.insert(
            note.id.clone(),
            String::from_utf8(bytes).map_err(|_| "memory content must be UTF-8")?,
        );
    }
    for item in &index.forgotten {
        if !safe_id(&item.id) || ids.contains(&item.id) {
            return Err("invalid memory tombstone".into());
        }
    }
    index
        .notes
        .sort_by(|a, b| a.key.cmp(&b.key).then(a.id.cmp(&b.id)));
    Ok(Snapshot { index, contents })
}
fn cleanup(scope: &Path, keep: Option<&str>) -> bool {
    let generations = scope.join("generations");
    match private_dir(&generations, false) {
        Ok(false) => return false,
        Err(_) => return true,
        Ok(true) => {}
    }
    let entries = match fs::read_dir(&generations) {
        Ok(entries) => entries,
        Err(_) => return true,
    };
    let mut pending = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                pending = true;
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if Some(name.as_str()) == keep || !name.starts_with("g-") || !safe_id(&name[2..]) {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {
                if fs::remove_dir_all(entry.path()).is_err() {
                    pending = true
                }
            }
            Err(_) => pending = true,
            _ => {}
        }
    }
    if File::open(&generations).and_then(|f| f.sync_all()).is_err() {
        pending = true
    }
    pending
}
fn cleanup_existing(scope: &Path) -> bool {
    let current = scope.join("CURRENT");
    match fs::symlink_metadata(&current) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => cleanup(scope, None),
        Err(_) => true,
        Ok(_) => match read(&current, 100)
            .and_then(|bytes| String::from_utf8(bytes).map_err(|e| e.to_string()))
        {
            Ok(name) if name.starts_with("g-") && safe_id(&name[2..]) => {
                cleanup(scope, Some(&name))
            }
            _ => true,
        },
    }
}
fn publish(scope: &Path, snapshot: &Snapshot, cancel: &dyn Fn() -> bool) -> Result<bool, String> {
    let generations = scope.join("generations");
    private_dir(&generations, true)?;
    let name = format!("g-{}", identifier());
    let generation = generations.join(&name);
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&generation)
        .map_err(|e| e.to_string())?;
    private_dir(&generation, false)?;
    private_dir(&generation.join("notes"), true)?;
    let pointer = scope.join(format!("CURRENT.{}.tmp", identifier()));
    let mut published = false;
    let result = (|| {
        let mut summary = String::from(
            "# Persistent memory\n\nManaged view. Use the memory plugin to change entries.\n\n",
        );
        for note in &snapshot.index.notes {
            stopped(cancel)?;
            write(
                &generation.join("notes").join(format!("{}.md", note.id)),
                snapshot.contents[&note.id].as_bytes(),
            )?;
            summary.push_str(&format!(
                "- {} — [entry](notes/{}.md), version {}\n",
                note.key, note.id, note.version
            ));
        }
        let index = serde_json::to_vec_pretty(&snapshot.index).map_err(|e| e.to_string())?;
        if index.len() > MAX_TOTAL {
            return Err("memory index exceeds 1 MiB".into());
        }
        write(&generation.join("index.json"), &index)?;
        write(&generation.join("MEMORY.md"), summary.as_bytes())?;
        for path in [
            generation.join("notes"),
            generation.clone(),
            generations.clone(),
        ] {
            File::open(path)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        write(&pointer, name.as_bytes())?;
        stopped(cancel)?;
        fs::rename(&pointer, scope.join("CURRENT")).map_err(|e| e.to_string())?;
        published = true;
        File::open(scope)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&pointer);
        if !published {
            let _ = fs::remove_dir_all(&generation);
        }
        return Err(error);
    }
    Ok(cleanup(scope, Some(&name)))
}

impl MemoryStore {
    pub(crate) fn call(
        &self,
        fs: &Fs,
        op: &str,
        args: Value,
        origin: Value,
        cancel: &dyn Fn() -> bool,
    ) -> OpResult {
        match self.perform(fs, op, args, origin, cancel, None) {
            Ok(value) => OpResult::Json(value),
            Err(message) => OpResult::error("ERR_PLY_MEMORY", message),
        }
    }
    fn perform(
        &self,
        fs: &Fs,
        op: &str,
        args: Value,
        origin: Value,
        cancel: &dyn Fn() -> bool,
        resolved_scope: Option<&str>,
    ) -> Result<Value, String> {
        stopped(cancel)?;
        if op == "memory.configure" {
            let directory = match args.get("directory") {
                Some(Value::String(path)) => PathBuf::from(path),
                None | Some(Value::Null) => PathBuf::from(
                    std::env::var_os("HOME").ok_or("host HOME unavailable for memory")?,
                )
                .join(".ply/memory"),
                _ => return Err("memory directory must be a string".into()),
            };
            if !directory.is_absolute() {
                return Err("memory directory must be absolute".into());
            }
            let mut configured = self
                .directory
                .lock()
                .map_err(|_| "memory configuration poisoned")?;
            if configured.is_some() {
                return Err("memory store is already configured".into());
            }
            *configured = Some(directory.clone());
            return Ok(json!({"directory":directory,"format":1}));
        }
        if op == "memory.forget" && resolved_scope.is_none() {
            let fields = args.as_object().ok_or("forget requires an object")?;
            if fields
                .keys()
                .any(|key| !matches!(key.as_str(), "id" | "expected_version"))
            {
                return Err("forget accepts only id and expected_version".into());
            }
            let id = args
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| safe_id(id))
                .ok_or("forget requires a valid id from recall")?;
            args.get("expected_version")
                .and_then(Value::as_u64)
                .filter(|v| *v > 0 && *v <= 9_007_199_254_740_991)
                .ok_or("forget requires a positive expected_version from recall")?;
            let mut pending = false;
            for scope in ["user", "project"] {
                if scope == "project" && project(fs)?.is_none() {
                    continue;
                }
                let result =
                    self.perform(fs, op, args.clone(), origin.clone(), cancel, Some(scope))?;
                if result.get("changed") == Some(&Value::Bool(true)) {
                    return Ok(result);
                }
                pending |= result.get("cleanup_pending") == Some(&Value::Bool(true));
            }
            return Ok(
                json!({"changed":false,"id":id,"scope":Value::Null,"cleanup_pending":pending}),
            );
        }
        if op == "memory.remember" {
            let fields = args.as_object().ok_or("remember requires an object")?;
            if fields.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "scope" | "key" | "content" | "expected_version"
                )
            }) {
                return Err(
                    "remember accepts only key, content, scope and expected_version".into(),
                );
            }
        }
        let root = self
            .directory
            .lock()
            .map_err(|_| "memory configuration poisoned")?
            .clone()
            .ok_or("memory store is not configured")?;
        let mutation = matches!(op, "memory.remember" | "memory.forget");
        let scope = match (resolved_scope, args.get("scope")) {
            (Some(scope), _) => scope,
            (None, None) => {
                if mutation {
                    "project"
                } else {
                    "all"
                }
            }
            (None, Some(Value::String(scope))) => scope.as_str(),
            _ => return Err("memory scope must be a string".into()),
        };
        if !matches!(scope, "user" | "project" | "all") || mutation && scope == "all" {
            return Err("choose user or project for memory changes; recall may use all".into());
        }
        let project = if scope == "user" { None } else { project(fs)? };
        let project_id = project
            .as_ref()
            .map(|identity| format!("p-{}", hash(identity.as_bytes())));
        let root_exists = private_dir(&root, mutation)?;
        let scopes = if scope == "all" {
            vec!["project", "user"]
        } else {
            vec![scope]
        };
        let mut all = Vec::new();
        let mut revisions = BTreeMap::new();
        for scope in scopes {
            let (identity, path) = if scope == "user" {
                ("user".to_string(), root.join("user"))
            } else {
                let Some(identity) = project.as_ref() else {
                    if mutation {
                        return Err(
                            "project memory requires a disk workspace; use user scope".into()
                        );
                    } else {
                        continue;
                    }
                };
                let projects = root.join("projects");
                if root_exists {
                    private_dir(&projects, mutation)?;
                }
                (
                    identity.clone(),
                    projects.join(project_id.as_ref().unwrap()),
                )
            };
            let exists = root_exists && private_dir(&path, mutation)?;
            let _lock = if exists {
                Some(lock(&path, cancel)?)
            } else {
                None
            };
            let mut snapshot = if exists {
                load(&path, &identity, cancel)?
            } else {
                Snapshot::empty(&identity)
            };
            if !mutation {
                revisions.insert(scope.to_string(), snapshot.index.revision);
                for note in &snapshot.index.notes {
                    let mut entry = serde_json::to_value(note).map_err(|e| e.to_string())?;
                    entry["content"] = json!(snapshot.contents[&note.id]);
                    entry["scope"] = json!(scope);
                    all.push(entry);
                }
                continue;
            }
            let expected = match args.get("expected_version") {
                None => None,
                Some(value) => Some(
                    value
                        .as_u64()
                        .filter(|v| *v > 0)
                        .ok_or("expected_version must be a positive integer")?,
                ),
            };
            let affected: Note;
            if op == "memory.remember" {
                let key = key(args.get("key").ok_or("memory key is required")?)?;
                let content = args
                    .get("content")
                    .and_then(Value::as_str)
                    .filter(|text| {
                        !text.trim().is_empty() && !text.contains('\0') && text.len() <= MAX_CONTENT
                    })
                    .ok_or("memory content must contain 1..16384 UTF-8 bytes")?;
                if let Some(note) = snapshot.index.notes.iter_mut().find(|note| note.key == key) {
                    if expected.is_some_and(|version| version != note.version) {
                        return Err(
                            "memory version conflict; recall the current entry before changing it"
                                .into(),
                        );
                    }
                    if snapshot.contents[&note.id] == content {
                        return Ok(
                            json!({"changed":false,"entry":note,"scope":scope,"revision":snapshot.index.revision,"cleanup_pending":cleanup_existing(&path)}),
                        );
                    }
                    if expected.is_none() {
                        return Err("memory key exists; recall it and supply expected_version to replace it".into());
                    }
                    note.version = note
                        .version
                        .checked_add(1)
                        .filter(|v| *v <= 9_007_199_254_740_991)
                        .ok_or("memory version overflow")?;
                    note.updated_at = now()?;
                    note.source = origin;
                    note.content_hash = hash(content.as_bytes());
                    affected = note.clone();
                } else {
                    if expected.is_some() {
                        return Err("memory entry no longer exists; an explicit new remember must omit expected_version".into());
                    }
                    if snapshot.index.notes.len() >= MAX_NOTES {
                        return Err("memory scope exceeds 128 entries".into());
                    }
                    let time = now()?;
                    let note = Note {
                        id: identifier(),
                        key,
                        version: 1,
                        created_at: time,
                        updated_at: time,
                        source: origin,
                        content_hash: hash(content.as_bytes()),
                    };
                    snapshot.index.notes.push(note.clone());
                    affected = note;
                }
                snapshot
                    .contents
                    .insert(affected.id.clone(), content.into());
                if snapshot.contents.values().map(String::len).sum::<usize>() > MAX_TOTAL {
                    return Err("memory scope exceeds 1 MiB".into());
                }
            } else if op == "memory.forget" {
                let id = args.get("id").and_then(Value::as_str);
                if let Some(index) = snapshot
                    .index
                    .notes
                    .iter()
                    .position(|note| id == Some(note.id.as_str()))
                {
                    let note = &snapshot.index.notes[index];
                    if expected != Some(note.version) {
                        return Err(
                            "forget requires the current expected_version; recall the entry first"
                                .into(),
                        );
                    }
                    let note = snapshot.index.notes.remove(index);
                    snapshot.contents.remove(&note.id);
                    snapshot.index.forgotten.push(Forgotten {
                        id: note.id.clone(),
                        forgotten_at: now()?,
                        version: note.version,
                    });
                    if snapshot.index.forgotten.len() > 4096 {
                        snapshot.index.forgotten.remove(0);
                    }
                    affected = note;
                } else {
                    return Ok(
                        json!({"changed":false,"scope":scope,"revision":snapshot.index.revision,"cleanup_pending":cleanup_existing(&path)}),
                    );
                }
            } else {
                return Err("unknown memory operation".into());
            }
            snapshot.index.revision = snapshot
                .index
                .revision
                .checked_add(1)
                .filter(|v| *v <= 9_007_199_254_740_991)
                .ok_or("memory revision overflow")?;
            snapshot.index.notes.sort_by(|a, b| a.key.cmp(&b.key));
            let cleanup_pending = publish(&path, &snapshot, cancel)?;
            return Ok(
                json!({"changed":true,"scope":scope,"revision":snapshot.index.revision,"id":affected.id,"key":affected.key,"version":affected.version,"cleanup_pending":cleanup_pending}),
            );
        }
        if op != "memory.recall" {
            return Err("unknown memory operation".into());
        }
        Ok(json!({"format":1,"project_id":project_id,"revisions":revisions,"entries":all}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ply_libc::{DiskStorage, Io, MountMode};
    use std::sync::Arc;

    fn filesystem(work: &Path) -> Fs {
        let cwd = fs::canonicalize(work).unwrap();
        let io = Io::new();
        io.mount(
            cwd.to_str().unwrap(),
            Arc::new(DiskStorage::open(&cwd).unwrap()),
            MountMode::ReadWrite,
        )
        .unwrap();
        Fs::new(io, cwd.display().to_string())
    }
    fn configured(directory: &Path, fs: &Fs) -> MemoryStore {
        let store = MemoryStore::default();
        value(store.call(
            fs,
            "memory.configure",
            json!({"directory":directory}),
            json!({}),
            &|| false,
        ));
        store
    }
    fn value(result: OpResult) -> Value {
        match result {
            OpResult::Json(value) => value,
            OpResult::Error(error) => panic!("{error}"),
            _ => panic!("unexpected memory result"),
        }
    }
    fn operation(store: &MemoryStore, fs: &Fs, op: &str, args: Value) -> Value {
        value(store.call(
            fs,
            op,
            args,
            json!({"kind":"explicit","fiber":"test"}),
            &|| false,
        ))
    }

    #[test]
    fn generations_preserve_updates_reject_stale_writes_and_remove_forgotten_content() {
        let work = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let fs = filesystem(work.path());
        let directory = state.path().join("memory");
        let store = configured(&directory, &fs);
        let saved = operation(
            &store,
            &fs,
            "memory.remember",
            json!({"scope":"user","key":"language","content":"unique-first-note-body"}),
        );
        assert_eq!(saved["version"], 1);
        assert!(matches!(
            store.call(
                &fs,
                "memory.remember",
                json!({"scope":"user","key":"language","content":"replacement"}),
                json!({}),
                &|| false
            ),
            OpResult::Error(_)
        ));
        let changed = operation(
            &store,
            &fs,
            "memory.remember",
            json!({"scope":"user","key":"language","content":"unique-second-note-body","expected_version":1}),
        );
        assert_eq!(changed["version"], 2);
        let other = configured(&directory, &fs);
        let recalled = operation(&other, &fs, "memory.recall", json!({"scope":"user"}));
        assert_eq!(recalled["entries"][0]["content"], "unique-second-note-body");
        operation(
            &other,
            &fs,
            "memory.forget",
            json!({"id":saved["id"],"expected_version":2}),
        );
        assert_eq!(
            operation(&store, &fs, "memory.recall", json!({"scope":"user"}))["entries"],
            json!([])
        );
        let mut pending = vec![directory];
        while let Some(path) = pending.pop() {
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path)
                } else {
                    let body = fs::read(path).unwrap();
                    assert!(!String::from_utf8_lossy(&body).contains("unique-first-note-body"));
                    assert!(!String::from_utf8_lossy(&body).contains("unique-second-note-body"));
                }
            }
        }
    }

    #[test]
    fn independent_writers_do_not_lose_distinct_entries() {
        let work = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let fs = filesystem(work.path());
        let directory = state.path().join("memory");
        let a = configured(&directory, &fs);
        let b = configured(&directory, &fs);
        let second = fs.clone();
        let one = std::thread::spawn(move || {
            operation(
                &a,
                &second,
                "memory.remember",
                json!({"scope":"user","key":"one","content":"first"}),
            )
        });
        operation(
            &b,
            &fs,
            "memory.remember",
            json!({"scope":"user","key":"two","content":"second"}),
        );
        one.join().unwrap();
        let recalled = operation(&b, &fs, "memory.recall", json!({"scope":"user"}));
        assert_eq!(recalled["entries"].as_array().unwrap().len(), 2);
        assert_eq!(recalled["revisions"]["user"], 2);
    }

    #[test]
    fn worktrees_share_project_identity_but_other_repositories_do_not() {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let primary = root.path().join("main");
        let linked = root.path().join("worktree");
        let unrelated = root.path().join("other");
        fs::create_dir_all(primary.join(".git/worktrees/linked")).unwrap();
        fs::create_dir_all(&linked).unwrap();
        fs::create_dir_all(unrelated.join(".git")).unwrap();
        fs::write(primary.join(".git/worktrees/linked/commondir"), "../..\n").unwrap();
        fs::write(
            linked.join(".git"),
            format!(
                "gitdir: {}\n",
                primary.join(".git/worktrees/linked").display()
            ),
        )
        .unwrap();
        let main = filesystem(&primary);
        let other = filesystem(&linked);
        let separate = filesystem(&unrelated);
        let store = configured(&state.path().join("memory"), &main);
        operation(
            &store,
            &main,
            "memory.remember",
            json!({"key":"project","content":"shared repository note"}),
        );
        assert_eq!(
            operation(&store, &other, "memory.recall", json!({"scope":"project"}))["entries"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            operation(
                &store,
                &separate,
                "memory.recall",
                json!({"scope":"project"})
            )["entries"],
            json!([])
        );
    }
}
