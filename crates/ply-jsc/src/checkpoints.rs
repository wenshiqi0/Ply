//! Private, atomic recovery records, independent of workspace mount modes.
//! Claims hold a nonblocking flock, including across Ply processes.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::runtime::OpResult;

const MAX_BYTES: u64 = 16 << 20;
static SERIAL: AtomicU64 = AtomicU64::new(1);

struct Claim {
    path: PathBuf,
    _lock: File,
    closed: Mutex<bool>,
}

#[derive(Default)]
pub(crate) struct Checkpoints {
    claims: Mutex<HashMap<u64, Arc<Claim>>>,
}

fn directory() -> Result<PathBuf, String> {
    let root = match std::env::var_os("PLY_CHECKPOINT_DIR") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(
            std::env::var_os("HOME").ok_or("host HOME is unavailable for checkpoints")?,
        )
        .join(".ply/checkpoints"),
    };
    if !root.is_absolute() {
        return Err("PLY_CHECKPOINT_DIR must be absolute".into());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .map_err(|e| e.to_string())?;
    let metadata = fs::symlink_metadata(&root).map_err(|e| e.to_string())?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        // Never chmod an arbitrary pre-existing path supplied by the user.
        return Err("checkpoint directory must be owned by this user and private (0700)".into());
    }
    Ok(root)
}

fn path(id: &str) -> Result<PathBuf, String> {
    if !(8..=96).contains(&id.len()) || !id.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-') {
        return Err("invalid checkpoint ID".into());
    }
    Ok(directory()?.join(format!("{id}.json")))
}

fn write_atomic(path: &PathBuf, snapshot: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(snapshot).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("checkpoint exceeds 16 MiB".into());
    }
    let temp = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temp, path).map_err(|e| e.to_string())?;
        File::open(path.parent().ok_or("invalid checkpoint directory")?)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

impl Checkpoints {
    pub(crate) fn call(&self, op: &str, args: Value) -> OpResult {
        match self.perform(op, args) {
            Ok(value) => OpResult::Json(value),
            Err(message) => OpResult::error("ERR_PLY_CHECKPOINT", message),
        }
    }

    fn perform(&self, op: &str, args: Value) -> Result<Value, String> {
        match op {
            "checkpoint.create" => {
                let time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_nanos();
                let id = format!(
                    "{:x}-{time:x}-{:x}",
                    std::process::id(),
                    SERIAL.fetch_add(1, Ordering::Relaxed)
                );
                write_atomic(
                    &path(&id)?,
                    args.get("snapshot").ok_or("missing checkpoint snapshot")?,
                )?;
                Ok(json!(id))
            }
            "checkpoint.claim" => {
                let file = path(
                    args.get("id")
                        .and_then(Value::as_str)
                        .ok_or("missing checkpoint ID")?,
                )?;
                let lock = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(file.with_extension("lock"))
                    .map_err(|e| e.to_string())?;
                if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                    return Err("checkpoint is already being resumed".into());
                }
                let reader = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&file)
                    .map_err(|e| e.to_string())?;
                let mut bytes = Vec::new();
                reader
                    .take(MAX_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                if bytes.len() as u64 > MAX_BYTES {
                    return Err("checkpoint exceeds 16 MiB".into());
                }
                let snapshot: Value =
                    serde_json::from_slice(&bytes).map_err(|_| "invalid checkpoint JSON")?;
                let lease = SERIAL.fetch_add(1, Ordering::Relaxed);
                self.claims
                    .lock()
                    .expect("checkpoint table poisoned")
                    .insert(
                        lease,
                        Arc::new(Claim {
                            path: file,
                            _lock: lock,
                            closed: Mutex::new(false),
                        }),
                    );
                Ok(json!({ "lease": lease, "snapshot": snapshot }))
            }
            "checkpoint.update" | "checkpoint.finish" => {
                let lease = args
                    .get("lease")
                    .and_then(Value::as_u64)
                    .ok_or("invalid checkpoint lease")?;
                let claim = self
                    .claims
                    .lock()
                    .expect("checkpoint table poisoned")
                    .get(&lease)
                    .cloned()
                    .ok_or("checkpoint lease is closed")?;
                let mut closed = claim.closed.lock().expect("checkpoint lease poisoned");
                if *closed {
                    return Err("checkpoint lease is closed".into());
                }
                if op == "checkpoint.update" {
                    write_atomic(
                        &claim.path,
                        args.get("snapshot").ok_or("missing checkpoint snapshot")?,
                    )?;
                } else {
                    // Keep the lock inode stable as long as a resumable
                    // record exists. Successful consumption never reuses IDs.
                    if args.get("remove").and_then(Value::as_bool) == Some(true) {
                        fs::remove_file(&claim.path).map_err(|e| e.to_string())?;
                        let _ = fs::remove_file(claim.path.with_extension("lock"));
                    }
                    *closed = true;
                    self.claims
                        .lock()
                        .expect("checkpoint table poisoned")
                        .remove(&lease);
                }
                Ok(Value::Null)
            }
            _ => Err("unknown checkpoint operation".into()),
        }
    }
}
