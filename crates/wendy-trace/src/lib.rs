//! One trace stream for everything a run does.
//!
//! Important decisions:
//! - Every decision point (an IPO operation and its phases, each IO call,
//!   each external process, each mount change) emits exactly one
//!   [`TraceEvent`] carrying its verdict, so a denied action is as visible as
//!   an allowed one.
//! - Emission is synchronous and cheap: events are emitted from the JS
//!   threads and from inside IO calls, so hooks must not need an async
//!   runtime.
//! - Persistence is a hook like any other. [`JsonlHook`] writes one JSON line
//!   per event and flushes it immediately, so a crash loses at most the event
//!   being written. Other sinks (a database, a remote collector) implement
//!   [`TraceHook`].

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which layer produced an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// IPO operations (tool calls, approvals) and their phases.
    Ipo,
    /// Code execution: module loads, external processes, script limits.
    Exec,
    /// One IO call on the mounted namespace, gated or observed.
    Io,
    /// Mount, unmount and swap.
    Mount,
    /// Free-form log lines from plugin code.
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Allow,
    Deny,
    Info,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    /// Monotonic per tracer; gaps never happen, so missing lines are detectable.
    pub seq: u64,
    pub ts_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub scope: Scope,
    /// Dotted name such as `io.write`, `ipo.start`, `exec.spawn`.
    pub kind: String,
    pub verdict: Verdict,
    #[serde(default)]
    pub data: Value,
}

/// Receives every event. Implement this to persist traces anywhere.
pub trait TraceHook: Send + Sync {
    fn on_event(&self, event: &TraceEvent);
}

/// Fan-out point shared by the sandbox and the runtime. Cheap to clone.
#[derive(Clone, Default)]
pub struct Tracer {
    inner: Arc<TracerInner>,
}

#[derive(Default)]
struct TracerInner {
    seq: AtomicU64,
    run_id: RwLock<Option<String>>,
    hooks: RwLock<Vec<Arc<dyn TraceHook>>>,
}

impl Tracer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_hook(&self, hook: Arc<dyn TraceHook>) {
        self.inner.hooks.write().expect("trace hooks poisoned").push(hook);
    }

    /// Tag every following event with this run id (or clear it with `None`).
    pub fn set_run(&self, run_id: Option<String>) {
        *self.inner.run_id.write().expect("trace run id poisoned") = run_id;
    }

    pub fn emit(&self, scope: Scope, kind: &str, verdict: Verdict, data: Value) {
        let event = TraceEvent {
            seq: self.inner.seq.fetch_add(1, Ordering::SeqCst),
            ts_ms: now_ms(),
            run_id: self.inner.run_id.read().expect("trace run id poisoned").clone(),
            scope,
            kind: kind.to_string(),
            verdict,
            data,
        };
        for hook in self.inner.hooks.read().expect("trace hooks poisoned").iter() {
            hook.on_event(&event);
        }
    }
}

/// Keeps events in memory; handy for tests and for returning a run's trace.
#[derive(Default)]
pub struct MemoryHook {
    events: Mutex<Vec<TraceEvent>>,
}

impl MemoryHook {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn events(&self) -> Vec<TraceEvent> {
        self.events.lock().expect("memory hook poisoned").clone()
    }
}

impl TraceHook for MemoryHook {
    fn on_event(&self, event: &TraceEvent) {
        self.events.lock().expect("memory hook poisoned").push(event.clone());
    }
}

/// Appends one JSON object per line to a file on the host disk.
pub struct JsonlHook {
    path: PathBuf,
    writer: Mutex<BufWriter<File>>,
}

impl JsonlHook {
    /// Opens (creating parent directories) and appends to `path`.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Arc<Self>> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Arc::new(Self { path, writer: Mutex::new(BufWriter::new(file)) }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl TraceHook for JsonlHook {
    fn on_event(&self, event: &TraceEvent) {
        let Ok(line) = serde_json::to_string(event) else { return };
        let mut writer = self.writer.lock().expect("jsonl hook poisoned");
        // A trace sink must never take the run down; a failed write is dropped.
        let _ = writeln!(writer, "{line}").and_then(|_| writer.flush());
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fans_out_to_every_hook_with_gapless_sequence() {
        let tracer = Tracer::new();
        let a = MemoryHook::new();
        let b = MemoryHook::new();
        tracer.add_hook(a.clone());
        tracer.add_hook(b.clone());
        tracer.set_run(Some("run-1".into()));
        tracer.emit(Scope::Io, "io.read", Verdict::Allow, json!({"path": "/a"}));
        tracer.emit(Scope::Io, "io.write", Verdict::Deny, json!({"path": "/b"}));

        let events = a.events();
        assert_eq!(events.len(), 2);
        assert_eq!(b.events().len(), 2);
        assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(events[1].verdict, Verdict::Deny);
        assert_eq!(events[0].run_id.as_deref(), Some("run-1"));
    }

    #[test]
    fn jsonl_hook_persists_each_event_as_a_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/trace.jsonl");
        let tracer = Tracer::new();
        tracer.add_hook(JsonlHook::open(&path).unwrap());
        tracer.emit(Scope::Ipo, "ipo.start", Verdict::Info, json!({"tool": "edit"}));
        tracer.emit(Scope::Ipo, "ipo.end", Verdict::Info, json!({"ok": true}));

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<TraceEvent> =
            text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].kind, "ipo.start");
    }
}
