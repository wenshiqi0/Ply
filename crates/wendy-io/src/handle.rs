//! [`Io`]: the handle runtimes hold. Every method is one gated, traced call.

use std::io;
use std::path::Path;
use std::sync::{Arc, RwLock};

use serde_json::{Value, json};
use wendy_storage::{DirItem, Entry, Storage, normalize};
use wendy_trace::{Scope, Tracer, Verdict};

use crate::namespace::{Mount, Resolved, synthetic_entry, synthetic_items};
use crate::{Actor, Decision, ExecPolicy, Gate, IoRequest, MountInfo, MountMode, Namespace, OpKind};

pub(crate) struct Shared {
    pub ns: Namespace,
    pub gate: RwLock<Arc<dyn Gate>>,
    pub tracer: Tracer,
    pub exec_policy: RwLock<ExecPolicy>,
}

/// A view of the shared IO layer on behalf of one [`Actor`]. Cheap to clone;
/// all clones share mounts, gate, exec policy and tracer.
#[derive(Clone)]
pub struct Io {
    pub(crate) shared: Arc<Shared>,
    actor: Actor,
}

impl Io {
    pub fn new(tracer: Tracer, gate: Arc<dyn Gate>) -> Self {
        Io {
            shared: Arc::new(Shared {
                ns: Namespace::new(),
                gate: RwLock::new(gate),
                tracer,
                exec_policy: RwLock::new(ExecPolicy::default()),
            }),
            actor: Actor::host(),
        }
    }

    /// Same layer, acting as someone else.
    pub fn as_actor(&self, actor: Actor) -> Io {
        Io { shared: self.shared.clone(), actor }
    }

    /// Same actor, different API/program label.
    pub fn labeled(&self, label: &str) -> Io {
        let mut actor = self.actor.clone();
        actor.label = label.to_string();
        self.as_actor(actor)
    }

    /// Same actor, attributed to IPO operation `op`.
    pub fn in_op(&self, op: Option<String>) -> Io {
        let mut actor = self.actor.clone();
        actor.op = op;
        self.as_actor(actor)
    }

    pub fn actor(&self) -> &Actor {
        &self.actor
    }

    pub fn tracer(&self) -> &Tracer {
        &self.shared.tracer
    }

    pub fn set_gate(&self, gate: Arc<dyn Gate>) {
        *self.shared.gate.write().expect("io gate poisoned") = gate;
    }

    // ---- mounts (host operations; traced, not gated) ----

    pub fn mount(&self, at: &str, storage: Arc<dyn Storage>, mode: MountMode) -> io::Result<MountInfo> {
        let result = self.shared.ns.mount(at, storage, mode);
        self.trace_mount("mount", at, &result);
        result
    }

    pub fn unmount(&self, at: &str) -> io::Result<MountInfo> {
        let result = self.shared.ns.unmount(at);
        self.trace_mount("unmount", at, &result);
        result
    }

    /// Point `at` at a different storage (disk ↔ memory ↔ database) in one step.
    pub fn swap(&self, at: &str, storage: Arc<dyn Storage>, mode: MountMode) -> io::Result<MountInfo> {
        let result = self.shared.ns.swap(at, storage, mode);
        self.trace_mount("swap", at, &result);
        result
    }

    pub fn mounts(&self) -> Vec<MountInfo> {
        self.shared.ns.list()
    }

    fn trace_mount(&self, kind: &str, at: &str, result: &io::Result<MountInfo>) {
        let (verdict, data) = match result {
            Ok(info) => (Verdict::Info, json!({ "at": info.at, "mode": info.mode, "storage": info.storage })),
            Err(err) => (Verdict::Error, json!({ "at": at, "error": err.to_string() })),
        };
        self.shared.tracer.emit(Scope::Mount, &format!("mount.{kind}"), verdict, data);
    }

    // ---- file operations ----

    pub async fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        let req = self.admit(OpKind::Read, path, None, None).await?;
        let (mount, result) = match self.shared.ns.resolve(&req.path) {
            Resolved::Mounted { mount, inner } => {
                let r = mount.storage.read(Path::new(&inner)).await;
                (Some(mount), r)
            }
            Resolved::Synthetic { .. } => (None, Err(is_a_dir(&req.path))),
            Resolved::Missing => (None, Err(not_found(&req.path))),
        };
        self.done(&req, mount.as_ref(), &result, |d| json!({ "bytes": d.len() }));
        result
    }

    pub async fn write(&self, path: &str, data: &[u8]) -> io::Result<()> {
        let req = self.admit(OpKind::Write, path, None, None).await?;
        let (mount, inner) = self.writable(&req)?;
        let result = mount.storage.write(Path::new(&inner), data).await;
        self.done(&req, Some(&mount), &result, |_| json!({ "bytes": data.len() }));
        result
    }

    pub async fn append(&self, path: &str, data: &[u8]) -> io::Result<()> {
        let req = self.admit(OpKind::Append, path, None, None).await?;
        let (mount, inner) = self.writable(&req)?;
        let result = mount.storage.append(Path::new(&inner), data).await;
        self.done(&req, Some(&mount), &result, |_| json!({ "bytes": data.len() }));
        result
    }

    pub async fn mkdir(&self, path: &str, recursive: bool) -> io::Result<()> {
        let req = self.admit(OpKind::Mkdir, path, None, None).await?;
        let (mount, inner) = self.writable(&req)?;
        let result = mount.storage.mkdir(Path::new(&inner), recursive).await;
        self.done(&req, Some(&mount), &result, |_| json!({ "recursive": recursive }));
        result
    }

    pub async fn remove(&self, path: &str, recursive: bool) -> io::Result<()> {
        let req = self.admit(OpKind::Remove, path, None, None).await?;
        let (mount, inner) = self.writable(&req)?;
        let result = if inner == "/" {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{}: is a mount point", req.path)))
        } else {
            mount.storage.remove(Path::new(&inner), recursive).await
        };
        self.done(&req, Some(&mount), &result, |_| json!({ "recursive": recursive }));
        result
    }

    pub async fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        let req = self.admit(OpKind::Rename, from, Some(to), None).await?;
        let to_path = req.to.clone().unwrap_or_default();
        let (mount, inner_from) = self.writable(&req)?;
        let result = match self.shared.ns.resolve(&to_path) {
            Resolved::Mounted { mount: dst, inner } if dst.at == mount.at => {
                if dst.mode == MountMode::ReadOnly {
                    Err(read_only(&to_path))
                } else if inner_from == "/" || inner == "/" {
                    Err(io::Error::new(io::ErrorKind::PermissionDenied, "cannot rename a mount point"))
                } else {
                    mount.storage.rename(Path::new(&inner_from), Path::new(&inner)).await
                }
            }
            Resolved::Mounted { .. } => Err(io::Error::new(
                io::ErrorKind::CrossesDevices,
                format!("{} -> {to_path}: crosses mounts", req.path),
            )),
            _ => Err(outside_mounts(&to_path)),
        };
        self.done(&req, Some(&mount), &result, |_| Value::Null);
        result
    }

    pub async fn stat(&self, path: &str) -> io::Result<Entry> {
        let req = self.admit(OpKind::Stat, path, None, None).await?;
        let (mount, result) = match self.shared.ns.resolve(&req.path) {
            Resolved::Mounted { mount, inner } => {
                let r = mount.storage.stat(Path::new(&inner)).await;
                (Some(mount), r)
            }
            Resolved::Synthetic { .. } => (None, Ok(synthetic_entry())),
            Resolved::Missing => (None, Err(not_found(&req.path))),
        };
        self.done(&req, mount.as_ref(), &result, |_| Value::Null);
        result
    }

    pub async fn list(&self, path: &str) -> io::Result<Vec<DirItem>> {
        let req = self.admit(OpKind::List, path, None, None).await?;
        let (mount, result) = match self.shared.ns.resolve(&req.path) {
            Resolved::Mounted { mount, inner } => {
                let r = mount.storage.list(Path::new(&inner)).await.map(|mut items| {
                    // Mount points below this directory show up even if the
                    // storage itself has no such entry.
                    for name in self.shared.ns.mount_children(&req.path) {
                        if !items.iter().any(|i| i.name == name) {
                            items.extend(synthetic_items(vec![name]));
                        }
                    }
                    items.sort_by(|a, b| a.name.cmp(&b.name));
                    items
                });
                (Some(mount), r)
            }
            Resolved::Synthetic { children } => (None, Ok(synthetic_items(children))),
            Resolved::Missing => (None, Err(not_found(&req.path))),
        };
        self.done(&req, mount.as_ref(), &result, |items| json!({ "entries": items.len() }));
        result
    }

    /// `stat` that answers yes/no. Still a traced read.
    pub async fn exists(&self, path: &str) -> bool {
        self.stat(path).await.is_ok()
    }

    // ---- internals ----

    /// Normalize, ask the gate, trace a denial. Returns the request to run.
    pub(crate) async fn admit(
        &self,
        op: OpKind,
        path: &str,
        to: Option<&str>,
        argv: Option<Vec<String>>,
    ) -> io::Result<IoRequest> {
        let req = IoRequest {
            op,
            access: op.access(),
            path: normalize(Path::new(path))?,
            to: to.map(|t| normalize(Path::new(t))).transpose()?,
            argv,
            actor: self.actor.clone(),
        };
        let gate = self.shared.gate.read().expect("io gate poisoned").clone();
        match gate.decide(&req).await {
            Decision::Allow => Ok(req),
            Decision::Deny(reason) => {
                self.emit(&req, None, Verdict::Deny, json!({ "reason": reason }));
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{}: permission denied ({reason})", req.path),
                ))
            }
        }
    }

    /// The mount a write-class request lands on, refusing read-only mounts and
    /// paths outside every mount.
    fn writable(&self, req: &IoRequest) -> io::Result<(Mount, String)> {
        let denial = match self.shared.ns.resolve(&req.path) {
            Resolved::Mounted { mount, inner } if mount.mode == MountMode::ReadWrite => {
                return Ok((mount, inner));
            }
            Resolved::Mounted { mount, .. } => {
                self.emit(req, Some(&mount), Verdict::Deny, json!({ "reason": "read-only mount" }));
                read_only(&req.path)
            }
            _ => {
                self.emit(req, None, Verdict::Deny, json!({ "reason": "outside every mount" }));
                outside_mounts(&req.path)
            }
        };
        Err(denial)
    }

    pub(crate) fn done<T>(&self, req: &IoRequest, mount: Option<&Mount>, result: &io::Result<T>, extra: impl Fn(&T) -> Value) {
        let data = match result {
            Ok(value) => json!({ "ok": true, "detail": extra(value) }),
            Err(err) => json!({ "ok": false, "error": err.to_string() }),
        };
        self.emit(req, mount, Verdict::Allow, data);
    }

    fn emit(&self, req: &IoRequest, mount: Option<&Mount>, verdict: Verdict, data: Value) {
        let mut event = json!({
            "op": req.op.as_str(),
            "access": req.access,
            "path": req.path,
            "actor": req.actor,
            "mount": mount.map(|m| m.at.clone()),
        });
        if let Some(to) = &req.to {
            event["to"] = json!(to);
        }
        if let Some(argv) = &req.argv {
            event["argv"] = json!(argv);
        }
        if let (Value::Object(target), Value::Object(extra)) = (&mut event, data) {
            target.extend(extra);
        }
        self.shared.tracer.emit(Scope::Io, &format!("io.{}", req.op.as_str()), verdict, event);
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

fn outside_mounts(path: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, format!("{path}: not on any mounted storage"))
}
