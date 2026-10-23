//! wendy-kernel: every plugin is one function, every execution a fiber.
//!
//! Important decisions:
//! - Loading a plugin reads its entry module (`main.ts`, ...) for what it
//!   is ([`PluginSpec`]: name = directory, description and input/output
//!   types from `handle`) and starts its realm. Plugin names are unique.
//! - A fiber is one run of a plugin's `handle`. The host starts fibers
//!   ([`Kernel::call`], or [`Kernel::wake`] without waiting); plugin code
//!   starts one by awaiting `tools.<name>(input)`, which makes it a child
//!   of the fiber that awaited. Fibers of one plugin share its realm and
//!   its closures; fibers of different plugins run in parallel. Nesting is
//!   bounded.
//! - What is recorded is fibers: every spawn and exit is a [`FiberEvent`]
//!   handed to the registered [`FiberHook`]s, as core defines it.
//! - Plugin code is read through its own read-only namespace, separate from
//!   the workspace the plugins work on.
//! - The five default plugins (bash, read, edit, create, delete) are plain
//!   plugins shipped inside the binary.

mod signature;

use std::collections::{BTreeMap, HashMap};
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use wendy_jsc::{Caller, HostBridge, JsRuntime, ModuleSource, RealmConfig, RealmId};

pub use wendy_core::{Exit, Fiber, FiberEvent, FiberHook, Field, Payload, PluginSpec, Type};
pub use wendy_jsc::{
    DiskStorage, Io, JsError, Limits, MemoryStorage, MountInfo, MountMode, Output, SqliteStorage, Storage, StorageInfo,
    StorageSpec,
};

/// The plugins every wendy ships: (name, entry source).
const DEFAULT_PLUGINS: [(&str, &str); 5] = [
    ("bash", include_str!("../../../plugins/bash/main.ts")),
    ("read", include_str!("../../../plugins/read/main.ts")),
    ("edit", include_str!("../../../plugins/edit/main.ts")),
    ("create", include_str!("../../../plugins/create/main.ts")),
    ("delete", include_str!("../../../plugins/delete/main.ts")),
];

pub struct KernelConfig {
    /// The workspace the plugins work on.
    pub io: Io,
    /// Where plugins start (relative paths resolve here).
    pub cwd: String,
    /// `process.env` of every plugin.
    pub env: BTreeMap<String, String>,
    pub limits: Limits,
    /// How deep fibers may nest (a fiber awaiting a fiber awaiting ...).
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
    /// A plugin directory (entry `main.ts`, `index.ts`, `main.js` or
    /// `index.js`); its name is the directory's name.
    Dir(PathBuf),
    /// A plugin shipped inside the binary: its name and entry source.
    Embedded { name: String, source: String },
}

impl PluginSource {
    /// bash, read, edit, create, delete.
    pub fn defaults() -> Vec<PluginSource> {
        DEFAULT_PLUGINS.iter().map(|(name, source)| PluginSource::Embedded { name: (*name).into(), source: (*source).into() }).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum KernelError {
    UnknownPlugin(String),
    Load(String),
    /// The fiber ran and failed: its handle threw, or its script was
    /// terminated.
    Fiber(String),
    TooDeep(usize),
}

impl std::fmt::Display for KernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KernelError::UnknownPlugin(name) => write!(f, "no plugin named `{name}`"),
            KernelError::Load(message) | KernelError::Fiber(message) => f.write_str(message),
            KernelError::TooDeep(depth) => write!(f, "fibers nested deeper than {depth}"),
        }
    }
}

impl std::error::Error for KernelError {}

/// What a script run with [`Kernel::run_script`] left behind.
pub struct ScriptOutcome {
    pub result: Result<(), JsError>,
    pub output: Output,
}

struct Loaded {
    spec: PluginSpec,
    realm: RealmId,
    entry: String,
}

struct Inner {
    js: JsRuntime,
    io: Io,
    code: Io,
    cwd: String,
    env: BTreeMap<String, String>,
    limits: Limits,
    max_depth: usize,
    plugins: RwLock<Vec<Loaded>>,
    hooks: RwLock<Vec<Arc<dyn FiberHook>>>,
    /// Fibers in flight → nesting depth.
    fibers: Mutex<HashMap<String, usize>>,
    /// Fiber ids are `<run>-<n>`, so records of many runs do not collide.
    run: String,
    next_fiber: AtomicU64,
}

/// Cheap to clone; clones share everything.
#[derive(Clone)]
pub struct Kernel {
    inner: Arc<Inner>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Kernel {
    /// Must be called inside a tokio runtime.
    pub fn new(config: KernelConfig) -> Kernel {
        Kernel {
            inner: Arc::new(Inner {
                js: JsRuntime::start(),
                io: config.io,
                code: Io::new(),
                cwd: config.cwd,
                env: config.env,
                limits: config.limits,
                max_depth: config.max_depth,
                plugins: RwLock::new(Vec::new()),
                hooks: RwLock::new(Vec::new()),
                fibers: Mutex::new(HashMap::new()),
                run: format!("{:x}", now_ms()),
                next_fiber: AtomicU64::new(0),
            }),
        }
    }

    pub fn io(&self) -> &Io {
        &self.inner.io
    }

    pub fn add_hook(&self, hook: Arc<dyn FiberHook>) {
        self.inner.hooks.write().expect("hooks poisoned").push(hook);
    }

    pub async fn load(&self, source: PluginSource) -> Result<PluginSpec, KernelError> {
        let inner = &self.inner;
        let (name, root, entry) = self.mount_code(source)?;
        if !PluginSpec::valid_name(&name) {
            return Err(KernelError::Load(format!("plugin name `{name}` must match [a-zA-Z0-9_-]{{1,64}}")));
        }
        if self.lookup(&name).is_ok() {
            return Err(KernelError::Load(format!("a plugin named `{name}` is already loaded")));
        }
        let source = inner.code.read(&entry).await.map_err(|e| KernelError::Load(format!("{entry}: {e}")))?;
        let signature = signature::read(&String::from_utf8_lossy(&source), Path::new(&entry)).map_err(KernelError::Load)?;
        let spec = PluginSpec { name: name.clone(), description: signature.description, input: signature.input, output: signature.output };

        let realm = self.realm(&name, &root, vec![entry.clone()]).await?;
        inner.js.run_main(realm, &entry).await.map_err(|e| KernelError::Load(e.message))?;
        let mut plugins = inner.plugins.write().expect("plugin table poisoned");
        if plugins.iter().any(|p| p.spec.name == name) {
            return Err(KernelError::Load(format!("a plugin named `{name}` is already loaded")));
        }
        plugins.push(Loaded { spec: spec.clone(), realm, entry });
        Ok(spec)
    }

    async fn realm(&self, name: &str, root: &str, argv: Vec<String>) -> Result<RealmId, KernelError> {
        let inner = &self.inner;
        let bridge: Arc<dyn HostBridge> = Arc::new(Bridge(Arc::downgrade(inner)));
        inner
            .js
            .create_realm(RealmConfig {
                name: name.into(),
                io: inner.io.clone(),
                cwd: inner.cwd.clone(),
                env: inner.env.clone(),
                argv,
                modules: ModuleSource { io: inner.code.clone(), root: root.into() },
                limits: inner.limits,
                bridge: Some(bridge),
            })
            .await
            .map_err(|e| KernelError::Load(e.message))
    }

    /// Mount a plugin's code in the code namespace: (name, root, entry).
    fn mount_code(&self, source: PluginSource) -> Result<(String, String, String), KernelError> {
        let load_err = KernelError::Load;
        match source {
            PluginSource::Dir(dir) => {
                let dir = std::fs::canonicalize(&dir).map_err(|e| load_err(format!("{}: {e}", dir.display())))?;
                let entry = ["main.ts", "index.ts", "main.js", "index.js"]
                    .iter()
                    .map(|f| dir.join(f))
                    .find(|f| f.is_file())
                    .ok_or_else(|| load_err(format!("{}: no main.ts, index.ts, main.js or index.js", dir.display())))?;
                let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let root = self.mount_dir(&dir)?;
                Ok((name, root, entry.display().to_string()))
            }
            PluginSource::Embedded { name, source } => {
                let root = format!("/wendy/plugins/{name}");
                let storage = MemoryStorage::with_files([("main.ts", source.as_bytes())]);
                self.inner.code.mount(&root, Arc::new(storage), MountMode::ReadOnly).map_err(|e| load_err(e.to_string()))?;
                Ok((name, root.clone(), format!("{root}/main.ts")))
            }
        }
    }

    /// Mount a host directory, read-only, at its own path in the code
    /// namespace (once).
    fn mount_dir(&self, dir: &Path) -> Result<String, KernelError> {
        let root = dir.display().to_string();
        let code = &self.inner.code;
        if !code.mounts().iter().any(|m| m.at == root) {
            let storage = DiskStorage::open(dir).map_err(|e| KernelError::Load(format!("{root}: {e}")))?;
            code.mount(&root, Arc::new(storage), MountMode::ReadOnly).map_err(|e| KernelError::Load(e.to_string()))?;
        }
        Ok(root)
    }

    pub fn plugins(&self) -> Vec<PluginSpec> {
        let mut specs: Vec<PluginSpec> =
            self.inner.plugins.read().expect("plugin table poisoned").iter().map(|p| p.spec.clone()).collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    /// Run a plugin as a top-level fiber and wait for its output.
    pub async fn call(&self, plugin: &str, input: Value) -> Result<Value, KernelError> {
        self.run(plugin, input, None, false).await
    }

    /// Start a plugin as a top-level fiber nobody awaits; returns its id.
    pub fn wake(&self, plugin: &str, input: Value) -> Result<String, KernelError> {
        self.lookup(plugin)?;
        let id = self.next_fiber_id();
        let (kernel, plugin, fiber) = (self.clone(), plugin.to_string(), id.clone());
        tokio::spawn(async move {
            let _ = kernel.start(&plugin, input, None, true, fiber).await;
        });
        Ok(id)
    }

    /// What a plugin printed (console, process.stdout/stderr) since last asked.
    pub async fn take_output(&self, plugin: &str) -> Option<Output> {
        let (realm, _) = self.lookup(plugin).ok()?;
        Some(self.inner.js.take_output(realm).await)
    }

    /// Run a TS/JS file like node would (its directory is the root it may
    /// import from), with `tools` available, until nothing is scheduled.
    pub async fn run_script(&self, file: &Path, args: Vec<String>) -> Result<ScriptOutcome, KernelError> {
        let file = std::fs::canonicalize(file).map_err(|e| KernelError::Load(format!("{}: {e}", file.display())))?;
        let dir = file.parent().ok_or_else(|| KernelError::Load("the script has no directory".into()))?;
        let root = self.mount_dir(dir)?;
        let entry = file.display().to_string();
        let name = file.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "script".into());
        let realm = self.realm(&name, &root, std::iter::once(entry.clone()).chain(args).collect()).await?;
        let js = &self.inner.js;
        let result = js.run_main(realm, &entry).await.map(|_| ());
        // Like node: stay until nothing is scheduled (unless it already failed).
        if result.is_ok() {
            js.idle(realm).await;
        }
        Ok(ScriptOutcome { result, output: js.take_output(realm).await })
    }

    fn lookup(&self, plugin: &str) -> Result<(RealmId, String), KernelError> {
        let plugins = self.inner.plugins.read().expect("plugin table poisoned");
        plugins
            .iter()
            .find(|p| p.spec.name == plugin)
            .map(|p| (p.realm, p.entry.clone()))
            .ok_or_else(|| KernelError::UnknownPlugin(plugin.to_string()))
    }

    fn next_fiber_id(&self) -> String {
        format!("{}-{}", self.inner.run, self.inner.next_fiber.fetch_add(1, Ordering::Relaxed) + 1)
    }

    async fn run(&self, plugin: &str, input: Value, parent: Option<String>, detached: bool) -> Result<Value, KernelError> {
        let id = self.next_fiber_id();
        self.start(plugin, input, parent, detached, id).await
    }

    async fn start(&self, plugin: &str, input: Value, parent: Option<String>, detached: bool, id: String) -> Result<Value, KernelError> {
        let inner = &self.inner;
        let (realm, entry) = self.lookup(plugin)?;
        {
            let mut fibers = inner.fibers.lock().expect("fiber table poisoned");
            let depth = parent.as_ref().and_then(|p| fibers.get(p)).map_or(0, |d| d + 1);
            if depth > inner.max_depth {
                return Err(KernelError::TooDeep(inner.max_depth));
            }
            fibers.insert(id.clone(), depth);
        }
        let fiber = Fiber { id: id.clone(), parent, plugin: plugin.to_string(), detached, input };
        self.emit(&FiberEvent::Spawn { at_ms: now_ms(), fiber: fiber.clone() });
        let started = Instant::now();
        let result = inner.js.run_fiber(realm, &entry, &fiber.input, &id).await;
        inner.fibers.lock().expect("fiber table poisoned").remove(&id);
        let exit = match &result {
            Ok(output) => Exit::Done { output: output.clone() },
            Err(e) => Exit::Failed { error: e.message.clone(), stack: e.stack.clone() },
        };
        self.emit(&FiberEvent::Exit { at_ms: now_ms(), id, exit, ms: started.elapsed().as_millis() as u64 });
        result.map_err(|e| KernelError::Fiber(e.message))
    }

    fn emit(&self, event: &FiberEvent) {
        for hook in self.inner.hooks.read().expect("hooks poisoned").iter() {
            hook.on_event(event);
        }
    }
}

/// `tools.<name>(input)` awaited in plugin code.
struct Bridge(Weak<Inner>);

#[async_trait::async_trait]
impl HostBridge for Bridge {
    async fn call(&self, caller: Caller, op: &str, args: Value) -> Result<Value, String> {
        let kernel = Kernel { inner: self.0.upgrade().ok_or("the kernel has stopped")? };
        if op != "kernel.spawn" {
            return Err(format!("unknown kernel op {op}"));
        }
        let plugin = args.get("plugin").and_then(Value::as_str).ok_or("missing plugin name")?;
        let input = args.get("input").cloned().unwrap_or(Value::Null);
        kernel.run(plugin, input, caller.fiber, false).await.map_err(|e| e.to_string())
    }
}

/// Appends every fiber event to a file, one JSON object per line, flushed
/// as it is written.
pub struct JsonlRecorder {
    writer: Mutex<BufWriter<std::fs::File>>,
}

impl JsonlRecorder {
    pub fn open(path: &Path) -> std::io::Result<Arc<Self>> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Arc::new(JsonlRecorder { writer: Mutex::new(BufWriter::new(file)) }))
    }
}

impl FiberHook for JsonlRecorder {
    fn on_event(&self, event: &FiberEvent) {
        let Ok(line) = serde_json::to_string(event) else { return };
        let Ok(mut writer) = self.writer.lock() else { return };
        // A record sink must never take a run down; a failed write is dropped.
        let _ = writeln!(writer, "{line}").and_then(|_| writer.flush());
    }
}
