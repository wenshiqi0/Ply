//! wendy-kernel: plugins as tool wrappers, every tool call an IPO operation.
//!
//! Important decisions:
//! - Loading a plugin starts its realm (its own JS thread) and asks it what
//!   it defines (`definePlugin`): a name and a set of tools. Tool names are
//!   global; a second plugin defining the same tool is refused.
//! - Every tool call is an operation with an id and, when another
//!   operation started it, a parent. It is traced as `ipo.start`, one
//!   `ipo.phase` per phase (reported by the realm), and `ipo.end` with the
//!   output or error. Operations run concurrently: the host calls tools,
//!   plugin code awaits other tools (`ctx.call`) or starts them without
//!   waiting (`ctx.wake`). Nesting is bounded.
//! - Asking for permission is an operation too: [`Kernel::approver`] wraps
//!   whatever the host asks with (a terminal prompt, a dialog, a rule), so
//!   each approval is traced as an op whose parent is the op that did the IO.
//! - Plugin code is read through its own read-only namespace (wendy-io),
//!   separate from the workspace the tools work on, sharing its trace.
//! - wendy's own tools are a plugin like any other ([`PluginSource::builtin`]),
//!   shipped inside the binary.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Instant;

use serde_json::{Value, json};
use wendy_core::{PluginSpec, ToolSpec};
use wendy_io::{AllowAll, Answer, Approver, Io, IoRequest, MountMode};
use wendy_jsc::{Caller, HostBridge, JsRuntime, Limits, ModuleSource, Output, RealmConfig, RealmId};
use wendy_storage::{DiskStorage, MemoryStorage};
use wendy_trace::{Scope, Tracer, Verdict};

/// wendy's built-in tools: bash, read, edit, create, delete.
pub const BUILTIN_TOOLS: &str = include_str!("../../../plugins/builtin/tools.ts");

/// Strings longer than this are shortened in trace events.
const TRACE_STRING: usize = 4096;

pub struct KernelConfig {
    /// The workspace the tools work on.
    pub io: Io,
    /// Where tools start (relative paths resolve here).
    pub cwd: String,
    /// `process.env` of every plugin.
    pub env: BTreeMap<String, String>,
    pub limits: Limits,
    /// How deep tool calls may nest.
    pub max_depth: usize,
}

impl KernelConfig {
    pub fn new(io: Io, cwd: impl Into<String>) -> Self {
        let cwd = cwd.into();
        let env = BTreeMap::from([("HOME".to_string(), cwd.clone()), ("USER".to_string(), "wendy".to_string())]);
        KernelConfig { io, cwd, env, limits: Limits::default(), max_depth: 16 }
    }
}

pub enum PluginSource {
    /// A directory (entry `main.ts`, `index.ts`, `main.js` or `index.js`)
    /// or an entry file; its directory is the plugin root.
    Path(PathBuf),
    /// Source shipped inside the binary: (path in the plugin, source).
    Embedded { name: String, files: Vec<(String, String)>, entry: String },
}

impl PluginSource {
    pub fn builtin() -> Self {
        PluginSource::Embedded {
            name: "builtin".into(),
            files: vec![("tools.ts".into(), BUILTIN_TOOLS.into())],
            entry: "tools.ts".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum KernelError {
    UnknownTool(String),
    Load(String),
    /// The tool ran and failed: it threw, an IO was refused, its script was
    /// terminated.
    Tool(String),
    TooDeep(usize),
}

impl std::fmt::Display for KernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KernelError::UnknownTool(name) => write!(f, "no tool named `{name}`"),
            KernelError::Load(message) | KernelError::Tool(message) => f.write_str(message),
            KernelError::TooDeep(depth) => write!(f, "tool calls nested deeper than {depth}"),
        }
    }
}

impl std::error::Error for KernelError {}

struct Loaded {
    spec: PluginSpec,
    realm: RealmId,
    entry: String,
}

struct Inner {
    js: JsRuntime,
    io: Io,
    code: Io,
    tracer: Tracer,
    cwd: String,
    env: BTreeMap<String, String>,
    limits: Limits,
    max_depth: usize,
    plugins: RwLock<Vec<Loaded>>,
    /// Operations in flight → nesting depth.
    ops: Mutex<HashMap<String, usize>>,
}

/// Cheap to clone; clones share everything.
#[derive(Clone)]
pub struct Kernel {
    inner: Arc<Inner>,
}

static NEXT_OP: AtomicU64 = AtomicU64::new(0);

fn next_op() -> String {
    format!("op-{}", NEXT_OP.fetch_add(1, Ordering::Relaxed) + 1)
}

impl Kernel {
    /// Must be called inside a tokio runtime.
    pub fn new(config: KernelConfig) -> Kernel {
        let tracer = config.io.tracer().clone();
        let code = Io::new(tracer.clone(), Arc::new(AllowAll));
        Kernel {
            inner: Arc::new(Inner {
                js: JsRuntime::start(),
                io: config.io,
                code,
                tracer,
                cwd: config.cwd,
                env: config.env,
                limits: config.limits,
                max_depth: config.max_depth,
                plugins: RwLock::new(Vec::new()),
                ops: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn io(&self) -> &Io {
        &self.inner.io
    }

    /// Wrap the host's way of asking so every approval is traced as an IPO
    /// operation. Usable before the kernel exists (the gate is built first).
    pub fn approver(tracer: &Tracer, inner: Arc<dyn Approver>) -> Arc<dyn Approver> {
        Arc::new(IpoApprover { tracer: tracer.clone(), inner })
    }

    pub async fn load(&self, source: PluginSource) -> Result<PluginSpec, KernelError> {
        let inner = &self.inner;
        let (hint, root, entry) = self.mount_code(source)?;
        let bridge: Arc<dyn HostBridge> = Arc::new(Bridge(Arc::downgrade(inner)));
        let realm = inner
            .js
            .create_realm(RealmConfig {
                name: hint,
                io: inner.io.clone(),
                cwd: inner.cwd.clone(),
                env: inner.env.clone(),
                argv: vec![entry.clone()],
                modules: ModuleSource { io: inner.code.clone(), root },
                limits: inner.limits,
                bridge: Some(bridge),
            })
            .await
            .map_err(|e| KernelError::Load(e.message))?;
        let described = inner.js.describe_plugin(realm, &entry).await.map_err(|e| KernelError::Load(e.message))?;
        let mut spec: PluginSpec =
            serde_json::from_value(described).map_err(|e| KernelError::Load(format!("{entry}: {e}")))?;
        spec.validate().map_err(KernelError::Load)?;
        for tool in &mut spec.tools {
            tool.plugin = spec.name.clone();
        }
        let mut plugins = inner.plugins.write().expect("plugin table poisoned");
        if plugins.iter().any(|p| p.spec.name == spec.name) {
            return Err(KernelError::Load(format!("a plugin named `{}` is already loaded", spec.name)));
        }
        for tool in &spec.tools {
            if let Some(owner) = plugins.iter().find(|p| p.spec.tool(&tool.name).is_some()) {
                return Err(KernelError::Load(format!(
                    "{}: tool `{}` is already provided by {}",
                    spec.name, tool.name, owner.spec.name
                )));
            }
        }
        inner.js.rename(realm, &spec.name);
        inner.tracer.emit(
            Scope::Ipo,
            "plugin.load",
            Verdict::Info,
            json!({
                "plugin": spec.name,
                "version": spec.version,
                "entry": entry,
                "tools": spec.tools.iter().map(|t| &t.name).collect::<Vec<_>>(),
            }),
        );
        plugins.push(Loaded { spec: spec.clone(), realm, entry });
        Ok(spec)
    }

    /// Mount a plugin's code in the code namespace: (realm name, root, entry).
    fn mount_code(&self, source: PluginSource) -> Result<(String, String, String), KernelError> {
        let code = &self.inner.code;
        let load_err = |what: String| KernelError::Load(what);
        match source {
            PluginSource::Path(path) => {
                let path = std::fs::canonicalize(&path).map_err(|e| load_err(format!("{}: {e}", path.display())))?;
                let (dir, entry) = if path.is_dir() {
                    let entry = ["main.ts", "index.ts", "main.js", "index.js"]
                        .iter()
                        .map(|f| path.join(f))
                        .find(|f| f.is_file())
                        .ok_or_else(|| load_err(format!("{}: no main.ts, index.ts, main.js or index.js", path.display())))?;
                    (path.clone(), entry)
                } else {
                    (path.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/")), path.clone())
                };
                let root = dir.display().to_string();
                if !code.mounts().iter().any(|m| m.at == root) {
                    let storage = DiskStorage::open(&dir).map_err(|e| load_err(format!("{root}: {e}")))?;
                    code.mount(&root, Arc::new(storage), MountMode::ReadOnly).map_err(|e| load_err(e.to_string()))?;
                }
                let hint = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "plugin".into());
                Ok((hint, root, entry.display().to_string()))
            }
            PluginSource::Embedded { name, files, entry } => {
                let root = format!("/wendy/plugins/{name}");
                let storage = MemoryStorage::with_files(files.iter().map(|(p, s)| (p.as_str(), s.as_bytes())));
                code.mount(&root, Arc::new(storage), MountMode::ReadOnly).map_err(|e| load_err(e.to_string()))?;
                Ok((name, root.clone(), format!("{root}/{entry}")))
            }
        }
    }

    pub fn plugins(&self) -> Vec<PluginSpec> {
        self.inner.plugins.read().expect("plugin table poisoned").iter().map(|p| p.spec.clone()).collect()
    }

    /// Every tool of every loaded plugin, by name.
    pub fn tools(&self) -> Vec<ToolSpec> {
        let mut tools: Vec<ToolSpec> = self.plugins().into_iter().flat_map(|p| p.tools).collect();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        tools
    }

    /// Run a tool as a top-level operation and wait for its output.
    pub async fn call(&self, tool: &str, input: Value) -> Result<Value, KernelError> {
        self.run(tool, input, None, next_op(), false).await
    }

    /// Start a tool as a top-level operation without waiting; returns its id.
    pub fn wake(&self, tool: &str, input: Value) -> Result<String, KernelError> {
        self.wake_from(tool, input, None)
    }

    /// What a plugin printed (console, process.stdout/stderr) since last asked.
    pub async fn take_output(&self, plugin: &str) -> Option<Output> {
        let realm = self.inner.plugins.read().expect("plugin table poisoned").iter().find(|p| p.spec.name == plugin)?.realm;
        Some(self.inner.js.take_output(realm).await)
    }

    fn lookup(&self, tool: &str) -> Result<(RealmId, String, String), KernelError> {
        let plugins = self.inner.plugins.read().expect("plugin table poisoned");
        plugins
            .iter()
            .find(|p| p.spec.tool(tool).is_some())
            .map(|p| (p.realm, p.entry.clone(), p.spec.name.clone()))
            .ok_or_else(|| KernelError::UnknownTool(tool.to_string()))
    }

    fn wake_from(&self, tool: &str, input: Value, parent: Option<String>) -> Result<String, KernelError> {
        self.lookup(tool)?;
        let id = next_op();
        let (kernel, tool, op) = (self.clone(), tool.to_string(), id.clone());
        tokio::spawn(async move {
            let _ = kernel.run(&tool, input, parent, op, true).await;
        });
        Ok(id)
    }

    async fn run(&self, tool: &str, input: Value, parent: Option<String>, id: String, detached: bool) -> Result<Value, KernelError> {
        let inner = &self.inner;
        let (realm, entry, plugin) = self.lookup(tool)?;
        let depth = {
            let mut ops = inner.ops.lock().expect("op table poisoned");
            let depth = parent.as_ref().and_then(|p| ops.get(p)).map_or(0, |d| d + 1);
            if depth > inner.max_depth {
                return Err(KernelError::TooDeep(inner.max_depth));
            }
            ops.insert(id.clone(), depth);
            depth
        };
        inner.tracer.emit(
            Scope::Ipo,
            "ipo.start",
            Verdict::Info,
            json!({
                "op": id,
                "parent": parent,
                "kind": "tool",
                "tool": tool,
                "plugin": plugin,
                "depth": depth,
                "detached": detached,
                "input": clip(&input),
            }),
        );
        let started = Instant::now();
        let result = inner.js.invoke_tool(realm, &entry, tool, &input, &id).await;
        inner.ops.lock().expect("op table poisoned").remove(&id);
        let ms = started.elapsed().as_millis() as u64;
        match &result {
            Ok(output) => inner.tracer.emit(
                Scope::Ipo,
                "ipo.end",
                Verdict::Info,
                json!({ "op": id, "tool": tool, "ok": true, "ms": ms, "output": clip(output) }),
            ),
            Err(e) => inner.tracer.emit(
                Scope::Ipo,
                "ipo.end",
                Verdict::Error,
                json!({ "op": id, "tool": tool, "ok": false, "ms": ms, "error": e.message, "stack": e.stack }),
            ),
        }
        result.map_err(|e| KernelError::Tool(e.message))
    }
}

/// `kernel.*` host ops from plugin code.
struct Bridge(Weak<Inner>);

#[async_trait::async_trait]
impl HostBridge for Bridge {
    async fn call(&self, caller: Caller, op: &str, args: Value) -> Result<Value, String> {
        let kernel = Kernel { inner: self.0.upgrade().ok_or("the kernel has stopped")? };
        let tool = || args.get("tool").and_then(Value::as_str).map(str::to_string).ok_or_else(|| "missing tool name".to_string());
        let input = args.get("input").cloned().unwrap_or(Value::Null);
        match op {
            "kernel.call" => kernel.run(&tool()?, input, caller.op, next_op(), false).await.map_err(|e| e.to_string()),
            "kernel.wake" => kernel.wake_from(&tool()?, input, caller.op).map(Value::String).map_err(|e| e.to_string()),
            "kernel.tools" => serde_json::to_value(kernel.tools()).map_err(|e| e.to_string()),
            other => Err(format!("unknown kernel op {other}")),
        }
    }
}

/// An approval as an IPO operation: input = the IO request, process = the
/// host's approver, output = the answer.
struct IpoApprover {
    tracer: Tracer,
    inner: Arc<dyn Approver>,
}

#[async_trait::async_trait]
impl Approver for IpoApprover {
    async fn approve(&self, req: &IoRequest) -> Answer {
        let id = next_op();
        self.tracer.emit(
            Scope::Ipo,
            "ipo.start",
            Verdict::Info,
            json!({ "op": id, "parent": req.actor.op, "kind": "approval", "input": req }),
        );
        let started = Instant::now();
        let answer = self.inner.approve(req).await;
        let output = match &answer {
            Answer::Once => json!("once"),
            Answer::Always => json!("always"),
            Answer::Deny(reason) => json!({ "deny": reason }),
        };
        self.tracer.emit(
            Scope::Ipo,
            "ipo.end",
            Verdict::Info,
            json!({ "op": id, "kind": "approval", "ok": true, "ms": started.elapsed().as_millis() as u64, "output": output }),
        );
        answer
    }
}

/// A copy of `value` with long strings shortened, for trace events.
fn clip(value: &Value) -> Value {
    match value {
        Value::String(s) if s.len() > TRACE_STRING => {
            let mut end = TRACE_STRING;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            Value::String(format!("{}…(+{} bytes)", &s[..end], s.len() - end))
        }
        Value::Array(items) => Value::Array(items.iter().map(clip).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), clip(v))).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_shortens_only_long_strings_on_char_boundaries() {
        let long = "é".repeat(TRACE_STRING);
        let clipped = clip(&json!({ "a": [long.clone(), "short"], "n": 1 }));
        let s = clipped["a"][0].as_str().unwrap();
        assert!(s.len() < long.len() && s.contains("…(+"));
        assert_eq!(clipped["a"][1], "short");
        assert_eq!(clipped["n"], 1);
    }
}
