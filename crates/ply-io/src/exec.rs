//! Real processes: [`Io::exec`].
//!
//! The working directory must be on a disk mount (storage that *is* a host
//! directory); memory and SQLite mounts do not exist for a real process.
//! The process inherits the host environment plus what the caller adds,
//! runs in its own process group, and that group is killed when the main
//! process exits or the timeout hits, so nothing it started outlives the
//! call.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use ply_storage::normalize;

use crate::Io;
use crate::handle::outside_mounts;
use crate::namespace::Resolved;

#[derive(Debug, Clone, Default)]
pub struct ExecSpec {
    /// Program and arguments; the program is looked up on `PATH`.
    pub argv: Vec<String>,
    /// Working directory in the namespace; must be on a disk mount.
    pub cwd: String,
    /// Environment overrides; the entire environment when `clear_env` is set.
    pub env: BTreeMap<String, String>,
    /// Start with only `env`, rather than inheriting the host environment.
    pub clear_env: bool,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Option<Duration>,
    /// Bytes kept per output stream; 0 means 1 MiB.
    pub max_output: usize,
}

/// Serializes with camelCase names and the output streams as base64, the
/// form runtimes hand to their code.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecResult {
    pub code: Option<i32>,
    /// Signal that ended the process, if any.
    pub signal: Option<i32>,
    #[serde(serialize_with = "base64")]
    pub stdout: Vec<u8>,
    #[serde(serialize_with = "base64")]
    pub stderr: Vec<u8>,
    /// Output beyond `max_output` was dropped.
    pub truncated: bool,
    pub timed_out: bool,
    pub duration_ms: u64,
}

const DEFAULT_MAX_OUTPUT: usize = 1 << 20;

pub type ExecSink = Arc<dyn Fn(&[u8]) + Send + Sync>;

struct ProcessGroup(i32);

impl ProcessGroup {
    fn kill(&mut self) {
        let pid = std::mem::take(&mut self.0);
        if pid > 0 { kill_group(pid); }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) { self.kill(); }
}

#[derive(Default)]
struct ExecJobs(Vec<tokio::task::AbortHandle>);

impl Drop for ExecJobs {
    fn drop(&mut self) { for job in &self.0 { job.abort(); } }
}

impl Io {
    /// Start a real process, wait for it, and return what it printed.
    pub async fn exec(&self, spec: ExecSpec) -> io::Result<ExecResult> {
        self.exec_with_sinks(spec, None, None).await
    }

    /// Map a namespace path for a native process. Memory storage has no host path.
    pub fn host_path(&self, path: &str) -> io::Result<std::path::PathBuf> {
        let path = normalize(Path::new(path))?;
        match self.ns.resolve(&path) {
            Resolved::Mounted { mount, inner } => match mount.storage.host_root() {
                Some(root) => Ok(root.join(inner.trim_start_matches('/'))),
                None => Err(io::Error::new(io::ErrorKind::Unsupported, format!("{path}: processes require disk storage"))),
            },
            _ => Err(outside_mounts(&path)),
        }
    }

    /// Stream output into the owning shell, including before cancellation.
    pub async fn exec_with_sinks(&self, spec: ExecSpec, stdout: Option<ExecSink>, stderr: Option<ExecSink>) -> io::Result<ExecResult> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        if spec.argv.is_empty() || spec.argv[0].is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "exec needs a program"));
        }
        let cwd = normalize(Path::new(&spec.cwd))?;
        let host_cwd = match self.ns.resolve(&cwd) {
            Resolved::Mounted { mount, inner } => match mount.storage.host_root() {
                Some(root) => root.join(inner.trim_start_matches('/')),
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!("{cwd}: processes can only run in a directory on a disk mount"),
                    ));
                }
            },
            _ => return Err(outside_mounts(&cwd)),
        };
        if !host_cwd.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, format!("{cwd}: not a directory")));
        }

        let mut cmd = tokio::process::Command::new(&spec.argv[0]);
        if spec.clear_env { cmd.env_clear(); }
        cmd.args(&spec.argv[1..])
            .current_dir(&host_cwd)
            .envs(&spec.env)
            .stdin(if spec.stdin.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);

        let started = Instant::now();
        let mut child = cmd.spawn()?;
        let pid = child.id().unwrap_or(0) as i32;
        // Also clean up the group if the exec future is cancelled while a
        // realm is being retired, before the normal wait/timeout path runs.
        let mut group = ProcessGroup(pid);
        let mut jobs = ExecJobs::default();
        if let (Some(data), Some(mut stdin)) = (spec.stdin.clone(), child.stdin.take()) {
            let task = tokio::spawn(async move {
                let _ = stdin.write_all(&data).await;
            });
            jobs.0.push(task.abort_handle());
        }
        let cap = if spec.max_output == 0 { DEFAULT_MAX_OUTPUT } else { spec.max_output };
        // Readers fill a shared buffer, so what was read survives even if
        // the pipe never reaches EOF (another process may have inherited its
        // write end; macOS sets close-on-exec only after creating a pipe).
        let read_capped = |stream: Option<Box<dyn tokio::io::AsyncRead + Send + Unpin>>, output_callback: Option<ExecSink>| {
            let buffer = Arc::new(Mutex::new((Vec::new(), false)));
            let sink = buffer.clone();
            let task = tokio::spawn(async move {
                let Some(mut stream) = stream else { return };
                let mut chunk = [0u8; 16 << 10];
                while let Ok(n) = stream.read(&mut chunk).await {
                    if n == 0 {
                        break;
                    }
                    if let Some(callback) = &output_callback { callback(&chunk[..n]); }
                    let mut kept = sink.lock().expect("output buffer poisoned");
                    let room = cap.saturating_sub(kept.0.len());
                    kept.0.extend_from_slice(&chunk[..n.min(room)]);
                    kept.1 |= n > room;
                }
            });
            (task, buffer)
        };
        let (out_task, out) = read_capped(child.stdout.take().map(|s| Box::new(s) as _), stdout);
        let (err_task, err) = read_capped(child.stderr.take().map(|s| Box::new(s) as _), stderr);
        jobs.0.extend([out_task.abort_handle(), err_task.abort_handle()]);

        let mut timed_out = false;
        let status = match spec.timeout {
            Some(limit) => match tokio::time::timeout(limit, child.wait()).await {
                Ok(status) => status,
                Err(_) => {
                    timed_out = true;
                    group.kill();
                    child.wait().await
                }
            },
            None => child.wait().await,
        };
        // Whatever the process left running in its group ends with it.
        group.kill();
        let status = status?;
        let grace = Duration::from_millis(500);
        for task in [out_task, err_task] {
            let abort = task.abort_handle();
            if tokio::time::timeout(grace, task).await.is_err() {
                abort.abort();
            }
        }
        let (stdout, out_dropped) = std::mem::take(&mut *out.lock().expect("output buffer poisoned"));
        let (stderr, err_dropped) = std::mem::take(&mut *err.lock().expect("output buffer poisoned"));

        #[cfg(unix)]
        let signal = std::os::unix::process::ExitStatusExt::signal(&status);
        #[cfg(not(unix))]
        let signal = None;
        Ok(ExecResult {
            code: status.code(),
            signal,
            stdout,
            stderr,
            truncated: out_dropped || err_dropped,
            timed_out,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }
}

fn kill_group(pid: i32) {
    #[cfg(unix)]
    if pid > 0 {
        // ESRCH (already gone) is the common case and fine.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = pid;
}

fn base64<S: serde::Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    serializer.serialize_str(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_serialize_for_runtimes_with_base64_output() {
        let r = ExecResult { code: Some(0), stdout: b"foobar".to_vec(), stderr: vec![0xff, 0xfe], ..ExecResult::default() };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["stdout"], "Zm9vYmFy");
        assert_eq!(json["stderr"], "//4=");
        assert_eq!(json["timedOut"], false);
        assert_eq!(json["durationMs"], 0);
    }
}
