//! Real processes as IO: [`Io::exec`].
//!
//! A process is a black box; what it can do to the machine has to come out
//! through channels, and wendy holds every channel:
//! - start: gated like a write (`OpKind::Exec`, path = its cwd, argv shown
//!   to the gate), traced with argv, cwd and pid;
//! - stdio: stdin is given (or `/dev/null`), stdout / stderr are captured;
//! - files: Seatbelt lets it write only below read-write disk mounts and
//!   wendy's temp space; FSEvents watches those mounts while it runs, and
//!   every change becomes an `io.*` trace event with `observed: true` and an
//!   `exec` actor (runtime `exec`, label = program name);
//! - network: closed unless the [`ExecPolicy`] allows it;
//! - lifetime: it runs in its own process group, which is killed when the
//!   main process exits or the timeout hits, so nothing outlives the call.
//!
//! Writes are confined and recorded, not individually gated: the gate
//! decided once, when the process was started. Only storage that *is* a host
//! directory (disk mounts) exists for a real process; memory and SQLite
//! mounts are invisible to it.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use wendy_trace::{Scope, Verdict};

use crate::namespace::{Mount, MountMode, Resolved};
use crate::{Actor, Io, IoRequest, OpKind, Runtime};

/// Host-wide rules for every process started through [`Io::exec`].
#[derive(Debug, Clone)]
pub struct ExecPolicy {
    /// Let processes open sockets. Off: every connection is refused.
    pub allow_net: bool,
    /// Host directories processes may not read (reads are otherwise open).
    pub deny_read: Vec<PathBuf>,
    /// Host environment variables passed through when set.
    pub inherit_env: Vec<String>,
}

impl Default for ExecPolicy {
    fn default() -> Self {
        ExecPolicy {
            allow_net: false,
            deny_read: Vec::new(),
            inherit_env: ["PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL", "LC_CTYPE", "TERM"]
                .map(String::from)
                .to_vec(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExecSpec {
    /// Program and arguments; the program is looked up on `PATH`.
    pub argv: Vec<String>,
    /// Working directory in the namespace; must be on a disk mount.
    pub cwd: String,
    /// Set on top of the inherited environment.
    pub env: BTreeMap<String, String>,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Option<Duration>,
    /// Bytes kept per output stream; 0 means 1 MiB.
    pub max_output: usize,
}

/// One change a process made, as observed on the mounts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    /// `write`, `mkdir` or `remove`.
    pub op: OpKind,
    /// Namespace path.
    pub path: String,
    pub dir: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ExecResult {
    pub code: Option<i32>,
    /// Signal that ended the process, if any.
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Output beyond `max_output` was dropped.
    pub truncated: bool,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub changes: Vec<Change>,
    /// False when the OS dropped file events, so `changes` may be partial.
    pub changes_complete: bool,
}

const DEFAULT_MAX_OUTPUT: usize = 1 << 20;
const TRACE_TEXT: usize = 8 << 10;
const MAX_CHANGES: usize = 10_000;

impl Io {
    pub fn set_exec_policy(&self, policy: ExecPolicy) {
        *self.shared.exec_policy.write().expect("exec policy poisoned") = policy;
    }

    pub fn exec_policy(&self) -> ExecPolicy {
        self.shared.exec_policy.read().expect("exec policy poisoned").clone()
    }

    /// Start a real process, wait for it, and report what it did.
    pub async fn exec(&self, spec: ExecSpec) -> io::Result<ExecResult> {
        if spec.argv.is_empty() || spec.argv[0].is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "exec needs a program"));
        }
        let req = self.admit(OpKind::Exec, &spec.cwd, None, Some(spec.argv.clone())).await?;
        let (mount, result) = self.run_process(&req, &spec).await;
        self.done(&req, mount.as_ref(), &result, |r| {
            json!({
                "code": r.code,
                "signal": r.signal,
                "timed_out": r.timed_out,
                "duration_ms": r.duration_ms,
                "stdout_bytes": r.stdout.len(),
                "stderr_bytes": r.stderr.len(),
                "stdout": clip(&r.stdout),
                "stderr": clip(&r.stderr),
                "truncated": r.truncated,
                "changes": r.changes.len(),
                "changes_complete": r.changes_complete,
            })
        });
        result
    }

    #[cfg(not(target_os = "macos"))]
    async fn run_process(&self, req: &IoRequest, _spec: &ExecSpec) -> (Option<Mount>, io::Result<ExecResult>) {
        let _ = req;
        (None, Err(io::Error::new(io::ErrorKind::Unsupported, "exec is only implemented on macOS")))
    }

    #[cfg(target_os = "macos")]
    async fn run_process(&self, req: &IoRequest, spec: &ExecSpec) -> (Option<Mount>, io::Result<ExecResult>) {
        let (mount, inner) = match self.shared.ns.resolve(&req.path) {
            Resolved::Mounted { mount, inner } => (mount, inner),
            _ => return (None, Err(io::Error::new(io::ErrorKind::NotFound, format!("{}: not on any mount", req.path)))),
        };
        let Some(root) = mount.storage.host_root() else {
            let err = io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{}: processes can only run in a directory on a disk mount", req.path),
            );
            return (Some(mount), Err(err));
        };
        let host_cwd = root.join(inner.trim_start_matches('/'));
        let result = self.spawn_confined(req, spec, &host_cwd).await;
        (Some(mount), result)
    }

    #[cfg(target_os = "macos")]
    async fn spawn_confined(&self, req: &IoRequest, spec: &ExecSpec, host_cwd: &Path) -> io::Result<ExecResult> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        if !host_cwd.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, format!("{}: not a directory", req.path)));
        }
        let policy = self.exec_policy();
        let mounts = self.shared.ns.snapshot();
        let writable: Vec<(String, PathBuf)> = mounts
            .iter()
            .filter(|m| m.mode == MountMode::ReadWrite)
            .filter_map(|m| m.storage.host_root().map(|r| (m.at.clone(), r.to_path_buf())))
            .collect();
        let read_only: Vec<PathBuf> = mounts
            .iter()
            .filter(|m| m.mode == MountMode::ReadOnly)
            .filter_map(|m| m.storage.host_root().map(Path::to_path_buf))
            .collect();
        let temp = TempSpace::create()?;
        let profile = crate::seatbelt::Profile {
            writable: writable.iter().map(|(_, r)| r.clone()).chain([temp.user_temp.clone()]).collect(),
            read_only,
            deny_read: policy.deny_read.iter().map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone())).collect(),
            allow_net: policy.allow_net,
        };
        let (text, params) = profile.render();

        let mut cmd = tokio::process::Command::new("/usr/bin/sandbox-exec");
        cmd.arg("-p").arg(&text);
        for (name, value) in &params {
            cmd.arg("-D").arg(format!("{name}={value}"));
        }
        cmd.arg("--").args(&spec.argv).current_dir(host_cwd).env_clear();
        for key in &policy.inherit_env {
            if let Ok(value) = std::env::var(key) {
                cmd.env(key, value);
            }
        }
        cmd.env("TMPDIR", &temp.dir).envs(&spec.env);
        cmd.stdin(if spec.stdin.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);

        let watched: Vec<PathBuf> = writable.iter().map(|(_, r)| r.clone()).chain([temp.dir.clone()]).collect();
        let watcher = crate::fsevents::Watcher::start(&watched);
        let started = Instant::now();
        let mut child = cmd.spawn()?;
        let pid = child.id().unwrap_or(0) as i32;
        let program = Path::new(&spec.argv[0]).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.shared.tracer.emit(
            Scope::Exec,
            "exec.spawn",
            Verdict::Info,
            json!({
                "argv": spec.argv,
                "cwd": req.path,
                "host_cwd": host_cwd.display().to_string(),
                "pid": pid,
                "actor": req.actor,
                "net": if policy.allow_net { "allow" } else { "deny" },
                "writable": writable.iter().map(|(at, _)| at).collect::<Vec<_>>(),
            }),
        );

        if let (Some(data), Some(mut stdin)) = (spec.stdin.clone(), child.stdin.take()) {
            tokio::spawn(async move {
                let _ = stdin.write_all(&data).await;
            });
        }
        let cap = if spec.max_output == 0 { DEFAULT_MAX_OUTPUT } else { spec.max_output };
        let read_capped = |stream: Option<Box<dyn tokio::io::AsyncRead + Send + Unpin>>| {
            tokio::spawn(async move {
                let (mut kept, mut dropped) = (Vec::new(), false);
                let Some(mut stream) = stream else { return (kept, dropped) };
                let mut buf = [0u8; 16 << 10];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let room = cap.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                    dropped |= n > room;
                }
                (kept, dropped)
            })
        };
        let out = read_capped(child.stdout.take().map(|s| Box::new(s) as _));
        let err = read_capped(child.stderr.take().map(|s| Box::new(s) as _));

        let mut timed_out = false;
        let status = match spec.timeout {
            Some(limit) => match tokio::time::timeout(limit, child.wait()).await {
                Ok(status) => status,
                Err(_) => {
                    timed_out = true;
                    kill_group(pid);
                    child.wait().await
                }
            },
            None => child.wait().await,
        };
        // Whatever the process left running in its group ends with it.
        kill_group(pid);
        let status = status?;
        let grace = Duration::from_secs(2);
        let (stdout, out_dropped) = tokio::time::timeout(grace, out).await.ok().and_then(Result::ok).unwrap_or_default();
        let (stderr, err_dropped) = tokio::time::timeout(grace, err).await.ok().and_then(Result::ok).unwrap_or_default();
        let duration_ms = started.elapsed().as_millis() as u64;

        let barrier = temp.dir.join(".wendy-barrier");
        let (events, delivered) = match watcher {
            Some(w) => tokio::task::spawn_blocking(move || w.finish(&barrier, Duration::from_secs(5)))
                .await
                .unwrap_or_default(),
            None => (Vec::new(), false),
        };
        let (changes, classified) = classify(events, &writable);
        let changes_complete = delivered && classified;
        let actor = Actor { runtime: Runtime::Exec, label: program, ..req.actor.clone() };
        for change in &changes {
            let mount = writable.iter().filter(|(at, _)| crate::gate::under(&change.path, at)).max_by_key(|(at, _)| at.len());
            self.shared.tracer.emit(
                Scope::Io,
                &format!("io.{}", change.op.as_str()),
                Verdict::Info,
                json!({
                    "op": change.op.as_str(),
                    "access": "write",
                    "path": change.path,
                    "actor": actor,
                    "mount": mount.map(|(at, _)| at),
                    "dir": change.dir,
                    "observed": true,
                    "pid": pid,
                }),
            );
        }
        if !changes_complete {
            self.shared.tracer.emit(
                Scope::Io,
                "io.observe_incomplete",
                Verdict::Error,
                json!({ "actor": actor, "pid": pid, "reason": "the OS dropped file events; changes may be partial" }),
            );
        }

        use std::os::unix::process::ExitStatusExt;
        Ok(ExecResult {
            code: status.code(),
            signal: status.signal(),
            stdout,
            stderr,
            truncated: out_dropped || err_dropped,
            timed_out,
            duration_ms,
            changes,
            changes_complete,
        })
    }
}

#[cfg(target_os = "macos")]
fn kill_group(pid: i32) {
    if pid > 0 {
        // ESRCH (already gone) is the common case and fine.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
}

/// Turn raw FSEvents into net changes per path, in first-seen order.
#[cfg(target_os = "macos")]
fn classify(events: Vec<(PathBuf, u32)>, writable: &[(String, PathBuf)]) -> (Vec<Change>, bool) {
    use crate::fsevents::*;
    let mut order: Vec<PathBuf> = Vec::new();
    let mut flags: HashMap<PathBuf, u32> = HashMap::new();
    let mut complete = true;
    for (path, flag) in events {
        if flag & OWN_EVENT != 0 {
            continue;
        }
        if flag & (MUST_SCAN_SUBDIRS | USER_DROPPED | KERNEL_DROPPED) != 0 {
            complete = false;
        }
        if flag & (CREATED | REMOVED | INODE_META | RENAMED | MODIFIED) == 0 {
            continue;
        }
        let entry = flags.entry(path.clone()).or_insert_with(|| {
            order.push(path);
            0
        });
        *entry |= flag;
    }
    let mut changes = Vec::new();
    for path in order {
        if changes.len() >= MAX_CHANGES {
            complete = false;
            break;
        }
        let flag = flags[&path];
        let Some(ns) = to_namespace(&path, writable) else { continue };
        let change = match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => {
                if flag & (CREATED | RENAMED) == 0 {
                    continue;
                }
                Change { op: OpKind::Mkdir, path: ns, dir: true }
            }
            Ok(_) => Change { op: OpKind::Write, path: ns, dir: false },
            // Created during the run and gone again: no net change.
            Err(_) if flag & CREATED != 0 => continue,
            Err(_) => Change { op: OpKind::Remove, path: ns, dir: flag & IS_DIR != 0 },
        };
        changes.push(change);
    }
    (changes, complete)
}

/// Host path → namespace path through the read-write disk mounts.
#[cfg(target_os = "macos")]
fn to_namespace(path: &Path, writable: &[(String, PathBuf)]) -> Option<String> {
    let (at, root) = writable.iter().filter(|(_, root)| path.starts_with(root)).max_by_key(|(_, root)| root.as_os_str().len())?;
    let rest = path.strip_prefix(root).ok()?;
    if rest.as_os_str().is_empty() {
        return None;
    }
    let rest = rest.to_str()?;
    Some(if at == "/" { format!("/{rest}") } else { format!("{at}/{rest}") })
}

/// The per-user temp directory (where `mktemp` writes on macOS) and a
/// private `TMPDIR` inside it that is removed afterwards.
#[cfg(target_os = "macos")]
struct TempSpace {
    user_temp: PathBuf,
    dir: PathBuf,
}

#[cfg(target_os = "macos")]
impl TempSpace {
    fn create() -> io::Result<TempSpace> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let user_temp = std::fs::canonicalize(darwin_user_temp().unwrap_or_else(std::env::temp_dir))?;
        let dir = user_temp.join(format!("wendy-exec-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir)?;
        Ok(TempSpace { user_temp, dir })
    }
}

#[cfg(target_os = "macos")]
impl Drop for TempSpace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(target_os = "macos")]
fn darwin_user_temp() -> Option<PathBuf> {
    let mut buf = vec![0u8; 1024];
    let n = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr().cast(), buf.len()) };
    if n == 0 || n > buf.len() {
        return None;
    }
    buf.truncate(n - 1);
    String::from_utf8(buf).ok().map(PathBuf::from)
}

fn clip(bytes: &[u8]) -> Value {
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(TRACE_TEXT)]);
    Value::String(text.into_owned())
}
