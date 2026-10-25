//! wendy-kernel: plugins with two hooks, every `onHandle` run a fiber.
//!
//! Important decisions:
//! - Loading a plugin reads its entry module (`main.ts`, ...) for what it
//!   is ([`PluginSpec`]: name = directory, description and input/output
//!   types from `onHandle`), starts its realm, evaluates the module, and
//!   then runs its `onLoad` in the background: one plugin's `onLoad` may
//!   wait for another (`await plugins.context.loaded`) whatever the order they
//!   were loaded in. [`Kernel::ready`] waits for every `onLoad`; after it,
//!   waiting for a plugin that is not loaded fails at once.
//! - What `onLoad` returns is mounted, flat, on the global `ctx` in every
//!   realm: plain members as a copy, functions as calls into the realm of
//!   the plugin that mounted them. When two plugins mount the same name, the
//!   one whose `onLoad` returned last wins: overriding is deliberate.
//!   `plugins.<name>.loaded` resolves when that plugin's `onLoad` has
//!   returned.
//! - A fiber is one run of a plugin's `onHandle` or an event callback.
//!   An onHandle call is started by the host
//!   ([`Kernel::call`], or [`Kernel::wake`] without waiting) or by plugin
//!   code awaiting `plugins.<name>(input)`, which makes it a child of the
//!   fiber that awaited. Fibers of one plugin share its realm and its
//!   closures; fibers of different plugins run in parallel. Nesting is
//!   bounded.
//! - What is recorded is fibers: every spawn, structured record and exit is
//!   a [`FiberEvent`] handed to the registered [`FiberHook`]s, as core defines it.
//! - Plugin code is read through its own read-only namespace, separate from
//!   the workspace the plugins work on.
//! - The default plugins are plain plugins shipped inside the binary.

mod signature;
mod events;
mod commands;
pub use commands::CommandSpec;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::watch;
use wendy_jsc::{FailureKind, Caller, HostBridge, JsRuntime, ModuleSource, RealmConfig, RealmId};

pub use wendy_core::{CleanupStatus, BudgetResource, Exit, Fiber, FiberEvent, FiberHook, Field, Mounts, Payload, PluginSpec, ToolSchema, Type};
pub use wendy_jsc::{
    DiskStorage, Io, JsError, Limits, MemoryStorage, MountInfo, MountMode, Output, SqliteStorage, Storage, StorageInfo,
    StorageSpec,
};

/// The plugins every wendy ships: (name, entry source). MiMo is opt-in.
const BUILTIN_PLUGINS: [(&str, &str); 12] = [
    ("agent", include_str!("../../../plugins/agent/main.ts")),
    ("bash", include_str!("../../../plugins/bash/main.ts")),
    ("context", include_str!("../../../plugins/context/main.ts")),
    ("create", include_str!("../../../plugins/create/main.ts")),
    ("deepseek", include_str!("../../../plugins/deepseek/main.ts")),
    ("edit", include_str!("../../../plugins/edit/main.ts")),
    ("mimo", include_str!("../../../plugins/mimo/main.ts")),
    ("jev", include_str!("../../../plugins/jev/main.ts")),
    ("approval", include_str!("../../../plugins/approval/main.ts")),
    ("tui", include_str!("../../../plugins/tui/main.ts")),
    ("read", include_str!("../../../plugins/read/main.ts")),
    ("remove", include_str!("../../../plugins/remove/main.ts")),
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
    /// agent, bash, context, create, deepseek, edit, read, remove.
    pub fn defaults() -> Vec<PluginSource> {
        BUILTIN_PLUGINS
            .iter()
            .filter(|(name, _)| !matches!(*name, "mimo" | "jev" | "approval" | "tui"))
            .map(|(name, source)| PluginSource::Embedded { name: (*name).into(), source: (*source).into() })
            .collect()
    }

    /// An explicitly selected built-in plugin, including opt-in integrations.
    pub fn builtin(name: &str) -> Option<PluginSource> {
        BUILTIN_PLUGINS.iter().find(|(entry, _)| *entry == name).map(|(name, source)| PluginSource::Embedded {
            name: (*name).into(),
            source: (*source).into(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum KernelError {
    UnknownPlugin(String),
    Load(String),
    /// The fiber ran and failed: its `onHandle` threw, or its script was
    /// terminated.
    Fiber(String),
    TooDeep(usize),
    ChildLimit { plugin: String, limit: u64, started: u64 },
    Cancelled(String),
    BudgetExceeded { resource: BudgetResource, error: String },
}

impl std::fmt::Display for KernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KernelError::UnknownPlugin(name) => write!(f, "no plugin named `{name}`"),
            KernelError::Load(message) | KernelError::Fiber(message) | KernelError::Cancelled(message) => f.write_str(message),
            KernelError::BudgetExceeded { error, .. } => f.write_str(error),
            KernelError::TooDeep(depth) => write!(f, "fibers nested deeper than {depth}"),
            KernelError::ChildLimit { plugin, limit, started } => write!(f, "child limit reached for {plugin}: {started} started, limit {limit}; replacements and resumes also consume this budget"),
        }
    }
}

impl std::error::Error for KernelError {}

/// What a script run with [`Kernel::run_script`] left behind.
pub struct ScriptOutcome {
    pub result: Result<(), JsError>,
    pub output: Output,
}

#[derive(Debug, Clone, PartialEq)]
enum LoadState {
    Loading,
    Loaded,
    Failed(String),
}

struct Loaded {
    spec: PluginSpec,
    realm: RealmId,
    entry: String,
    state: watch::Sender<LoadState>,
    mounts: Mounts,
    /// When its mounts went onto `ctx` (later wins); 0 while loading.
    mounted: u64,
}

/// One plugin as every realm's imported directory and `ctx` see it.
#[derive(Serialize)]
struct DirectoryEntry<'a> {
    name: &'a str,
    description: &'a str,
    tool: Option<ToolSchema>,
    mounts: &'a Mounts,
    mounted: u64,
    state: &'static str,
}

enum FiberInvocation { Tool, Event { callback: u64, name: String }, Command { callback: u64, name: String } }

struct ActiveFiber {
    depth: usize,
    parent: Option<String>,
    cancel: watch::Sender<bool>,
    child_budgets: HashMap<String, ChildBudget>,
}

#[derive(Clone, Copy)]
struct ChildBudget { limit: u64, started: u64 }

impl ChildBudget {
    fn value(self) -> Value {
        serde_json::json!({ "limit": self.limit, "started": self.started, "remaining": self.limit.saturating_sub(self.started) })
    }
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
    /// Ticks whenever a plugin is added or finishes loading.
    generation: watch::Sender<u64>,
    /// Set by `ready`: no plugin that is not loaded will appear.
    sealed: AtomicBool,
    /// Orders mounts on `ctx`.
    next_mount: AtomicU64,
    /// Every realm this kernel started (plugins and scripts).
    realms: Mutex<HashSet<RealmId>>,
    hooks: RwLock<Vec<Arc<dyn FiberHook>>>,
    events: Mutex<events::Events>,
    commands: Mutex<commands::Commands>,
    next_event: AtomicU64,
    /// Active fibers and their ancestry, for depth checks and fiber.info().
    fibers: Mutex<HashMap<String, ActiveFiber>>,
    /// Fiber ids are `<run>-<n>`, so records of many runs do not collide.
    run: String,
    next_fiber: AtomicU64,
}

/// Cheap to clone; clones share everything.
#[derive(Clone)]
pub struct Kernel {
    inner: Arc<Inner>,
}

/// Owns an unpublished/plugin or script realm until ownership is handed
/// to the plugin registry, including when its load/run future is cancelled.
struct TemporaryRealm {
    kernel: Kernel,
    realm: Option<RealmId>,
}

/// Paired Spawn/Exit accounting also when the awaiting host op is aborted
/// as its script realm is retired.
struct RunningFiber {
    kernel: Kernel,
    id: Option<String>,
    started: Instant,
}

impl RunningFiber {
    fn finish(&mut self, exit: Exit) {
        if let Some(id) = self.id.take() {
            self.kernel.inner.fibers.lock().expect("fiber table poisoned").remove(&id);
            self.kernel.emit(&FiberEvent::Exit { at_ms: now_ms(), id, exit, ms: self.started.elapsed().as_millis() as u64 });
        }
    }
}

impl Drop for RunningFiber {
    fn drop(&mut self) {
        self.finish(Exit::Cancelled { reason: "the awaiting fiber call was cancelled".into() });
    }
}

impl TemporaryRealm {
    fn retire(&mut self) -> Option<tokio::task::JoinHandle<Result<(), JsError>>> {
        let realm = self.realm.take()?;
        self.kernel.inner.realms.lock().expect("realm list poisoned").remove(&realm);
        Some(self.kernel.inner.js.close_realm(realm))
    }

    async fn close(mut self) -> Result<(), JsError> {
        if let Some(job) = self.retire() {
            job.await.map_err(|_| JsError { message: "realm cleanup task was cancelled".into(), ..JsError::default() })??;
        }
        Ok(())
    }
}

impl Drop for TemporaryRealm {
    fn drop(&mut self) { drop(self.retire()); }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Kernel {
    /// Must be called inside a tokio runtime.
    pub fn new(config: KernelConfig) -> Kernel {
        static NEXT_RUN: AtomicU64 = AtomicU64::new(0);
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
                generation: watch::channel(0).0,
                sealed: AtomicBool::new(false),
                next_mount: AtomicU64::new(0),
                realms: Mutex::new(HashSet::new()),
                hooks: RwLock::new(Vec::new()),
                events: Mutex::new(events::Events::default()),
                commands: Mutex::new(commands::Commands::default()),
                next_event: AtomicU64::new(0),
                fibers: Mutex::new(HashMap::new()),
                run: format!("{:x}-{:x}-{:x}", now_ms(), std::process::id(), NEXT_RUN.fetch_add(1, Ordering::Relaxed)),
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

    /// Load a plugin: read it, start its realm, evaluate it. Its `onLoad`
    /// then runs in the background; see [`Kernel::ready`].
    pub async fn load(&self, source: PluginSource) -> Result<PluginSpec, KernelError> {
        let inner = &self.inner;
        let (name, root, entry) = self.mount_code(source)?;
        if !PluginSpec::valid_name(&name) {
            return Err(KernelError::Load(format!("plugin name `{name}` must match [a-zA-Z0-9_-]{{1,64}}")));
        }
        if self.find(&name).is_some() {
            return Err(KernelError::Load(format!("a plugin named `{name}` is already loaded")));
        }
        let source = inner.code.read(&entry).await.map_err(|e| KernelError::Load(format!("{entry}: {e}")))?;
        let signature = signature::read(&String::from_utf8_lossy(&source), Path::new(&entry)).map_err(KernelError::Load)?;
        let spec = PluginSpec {
            name: name.clone(),
            description: signature.description,
            callable: signature.callable,
            input: signature.input,
            output: signature.output,
        };

        let realm = self.realm(&name, &root, vec![entry.clone()]).await?;
        let mut unpublished = TemporaryRealm { kernel: self.clone(), realm: Some(realm) };
        inner.js.run_main(realm, &entry).await.map_err(|e| KernelError::Load(e.message))?;
        {
            let mut plugins = inner.plugins.write().expect("plugin table poisoned");
            if plugins.iter().any(|p| p.spec.name == name) {
                return Err(KernelError::Load(format!("a plugin named `{name}` is already loaded")));
            }
            let state = watch::channel(LoadState::Loading).0;
            plugins.push(Loaded { spec: spec.clone(), realm, entry: entry.clone(), state, mounts: Mounts::default(), mounted: 0 });
        }
        unpublished.realm = None;
        self.changed();
        if signature.loads {
            // It may wait for plugins that load after it.
            let kernel = self.clone();
            tokio::spawn(async move {
                let result = kernel.inner.js.run_load(realm, &entry).await;
                let mounts = result
                    .map_err(|e| e.message)
                    .and_then(|v| serde_json::from_value::<Mounts>(v).map_err(|e| e.to_string()));
                kernel.finish_load(&name, mounts);
            });
        } else {
            self.finish_load(&name, Ok(Mounts::default()));
        }
        Ok(spec)
    }

    fn finish_load(&self, name: &str, mounts: Result<Mounts, String>) {
        {
            let mut plugins = self.inner.plugins.write().expect("plugin table poisoned");
            let Some(index) = plugins.iter().position(|p| p.spec.name == name) else { return };
            let state = match mounts {
                Ok(mounts) => {
                    let plugin = &mut plugins[index];
                    plugin.mounts = mounts;
                    plugin.mounted = self.inner.next_mount.fetch_add(1, Ordering::SeqCst) + 1;
                    LoadState::Loaded
                }
                Err(e) => {
                    self.inner.events.lock().expect("event table poisoned").remove_plugin(name);
                    self.inner.commands.lock().expect("command table poisoned").remove_plugin(name);
                    LoadState::Failed(format!("{name}: onLoad failed: {e}"))
                },
            };
            // Queue the mounted ctx members before waking `plugins.x.loaded`.
            // Hold the plugin table until publication completes, so another
            // publisher cannot later overwrite this with an older snapshot.
            let directory = Self::directory_entries(&plugins, Some((name, &state)));
            self.send_directory(directory);
            plugins[index].state.send_replace(state);
        }
        self.inner.generation.send_modify(|g| *g += 1);
    }

    /// Wait until every loaded plugin has finished its `onLoad`. From here
    /// on the set of plugins is taken as complete: waiting for one that is
    /// not loaded fails at once instead of waiting. Fails with every
    /// plugin whose `onLoad` failed.
    pub async fn ready(&self) -> Result<(), KernelError> {
        self.inner.sealed.store(true, Ordering::SeqCst);
        self.changed();
        let names: Vec<String> = self.inner.plugins.read().expect("plugin table poisoned").iter().map(|p| p.spec.name.clone()).collect();
        let mut failures = Vec::new();
        for name in names {
            if let Err(e) = self.wait_loaded(&name).await {
                failures.push(e.to_string());
            }
        }
        if failures.is_empty() { Ok(()) } else { Err(KernelError::Load(failures.join("; "))) }
    }

    /// Resolves when `plugin`'s `onLoad` has returned.
    pub async fn wait_loaded(&self, plugin: &str) -> Result<(), KernelError> {
        let mut generation = self.inner.generation.subscribe();
        loop {
            let state = self.find(plugin).map(|p| p.state.subscribe());
            match state {
                Some(mut state) => loop {
                    match state.borrow_and_update().clone() {
                        LoadState::Loaded => return Ok(()),
                        LoadState::Failed(message) => return Err(KernelError::Load(message)),
                        LoadState::Loading => {}
                    }
                    if state.changed().await.is_err() {
                        return Err(KernelError::Load(format!("{plugin}: unloaded")));
                    }
                },
                None if self.inner.sealed.load(Ordering::SeqCst) => return Err(KernelError::UnknownPlugin(plugin.into())),
                None => {
                    if generation.changed().await.is_err() {
                        return Err(KernelError::UnknownPlugin(plugin.into()));
                    }
                }
            }
        }
    }

    fn changed(&self) {
        self.publish_directory();
        self.inner.generation.send_modify(|g| *g += 1);
    }

    fn find(&self, plugin: &str) -> Option<LoadedRef> {
        let plugins = self.inner.plugins.read().expect("plugin table poisoned");
        plugins.iter().find(|p| p.spec.name == plugin).map(|p| LoadedRef {
            realm: p.realm,
            entry: p.entry.clone(),
            callable: p.spec.callable,
            methods: p.mounts.methods.clone(),
            state: p.state.clone(),
        })
    }

    fn directory_entries(plugins: &[Loaded], state_override: Option<(&str, &LoadState)>) -> Value {
        let mut entries: Vec<DirectoryEntry> = plugins
            .iter()
            .map(|p| {
                let current = p.state.borrow();
                let state = state_override.filter(|(name, _)| *name == p.spec.name).map(|(_, state)| state).unwrap_or(&current);
                DirectoryEntry {
                    name: &p.spec.name,
                    description: &p.spec.description,
                    tool: p.spec.tool_schema(),
                    mounts: &p.mounts,
                    mounted: p.mounted,
                    state: match state {
                        LoadState::Loading => "loading",
                        LoadState::Loaded => "loaded",
                        LoadState::Failed(_) => "failed",
                    },
                }
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(b.name));
        serde_json::to_value(entries).unwrap_or_default()
    }

    fn publish_directory(&self) {
        let plugins = self.inner.plugins.read().expect("plugin table poisoned");
        self.send_directory(Self::directory_entries(&plugins, None));
    }

    fn send_directory(&self, directory: Value) {
        for realm in self.inner.realms.lock().expect("realm list poisoned").iter() {
            self.inner.js.set_directory(*realm, directory.clone());
        }
    }

    async fn realm(&self, name: &str, root: &str, argv: Vec<String>) -> Result<RealmId, KernelError> {
        let inner = &self.inner;
        let bridge: Arc<dyn HostBridge> = Arc::new(Bridge(Arc::downgrade(inner)));
        let realm = inner
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
            .map_err(|e| KernelError::Load(e.message))?;
        let plugins = inner.plugins.read().expect("plugin table poisoned");
        let mut realms = inner.realms.lock().expect("realm list poisoned");
        realms.insert(realm);
        inner.js.set_directory(realm, Self::directory_entries(&plugins, None));
        Ok(realm)
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

    /// The plugins that can be called (have `onHandle`), as tools: what MCP
    /// clients see.
    pub fn catalog(&self) -> Vec<ToolSchema> {
        self.plugins().iter().filter_map(PluginSpec::tool_schema).collect()
    }

    /// Run a plugin as a top-level fiber and wait for its output.
    pub async fn call(&self, plugin: &str, input: Value) -> Result<Value, KernelError> {
        self.run(plugin, input, None, false).await
    }

    /// Request cancellation of an active fiber. Its awaited children and
    /// host resources are cancelled as their owning futures are dropped.
    pub fn cancel_fiber(&self, id: &str) -> bool {
        let fibers = self.inner.fibers.lock().expect("fiber table poisoned");
        if let Some(fiber) = fibers.get(id) { fiber.cancel.send_replace(true); true } else { false }
    }

    /// Start a plugin as a top-level fiber nobody awaits; returns its id.
    pub fn wake(&self, plugin: &str, input: Value) -> Result<String, KernelError> {
        if self.find(plugin).is_none() {
            return Err(KernelError::UnknownPlugin(plugin.into()));
        }
        let id = self.next_fiber_id();
        let (kernel, plugin, fiber) = (self.clone(), plugin.to_string(), id.clone());
        tokio::spawn(async move {
            let _ = kernel.start(&plugin, input, None, true, fiber).await;
        });
        Ok(id)
    }

    /// Call a function on `ctx` (one a plugin's `onLoad` mounted), as the
    /// host.
    pub async fn call_ctx(&self, name: &str, args: Vec<Value>) -> Result<Value, KernelError> {
        let owner = self
            .inner
            .plugins
            .read()
            .expect("plugin table poisoned")
            .iter()
            .filter(|p| p.mounts.names().any(|m| m == name))
            .max_by_key(|p| p.mounted)
            .filter(|p| p.mounts.methods.iter().any(|m| m == name))
            .map(|p| p.spec.name.clone())
            .ok_or_else(|| KernelError::Fiber(format!("ctx.{name} is not a mounted function")))?;
        self.method(&owner, name, args, None).await
    }

    async fn method(&self, plugin: &str, method: &str, args: Vec<Value>, caller: Option<String>) -> Result<Value, KernelError> {
        self.wait_loaded(plugin).await?;
        let target = self.find(plugin).ok_or_else(|| KernelError::UnknownPlugin(plugin.into()))?;
        if !target.methods.iter().any(|m| m == method) {
            return Err(KernelError::Fiber(format!("ctx.{method} is not a function {plugin} mounted")));
        }
        self.inner.js.call_method(target.realm, &target.entry, method, &args, caller).await.map_err(|e| KernelError::Fiber(e.message))
    }

    /// What a plugin printed (console, process.stdout/stderr) since last asked.
    pub async fn take_output(&self, plugin: &str) -> Option<Output> {
        let target = self.find(plugin)?;
        Some(self.inner.js.take_output(target.realm).await)
    }

    /// Run a TS/JS file like node would (its directory is the root it may
    /// import from), with wendy:plugins available, until nothing is scheduled.
    pub async fn run_script(&self, file: &Path, args: Vec<String>) -> Result<ScriptOutcome, KernelError> {
        let file = std::fs::canonicalize(file).map_err(|e| KernelError::Load(format!("{}: {e}", file.display())))?;
        let dir = file.parent().ok_or_else(|| KernelError::Load("the script has no directory".into()))?;
        let root = self.mount_dir(dir)?;
        let entry = file.display().to_string();
        let name = file.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "script".into());
        let realm = self.realm(&name, &root, std::iter::once(entry.clone()).chain(args).collect()).await?;
        let temporary = TemporaryRealm { kernel: self.clone(), realm: Some(realm) };
        let js = &self.inner.js;
        let result = js.run_main(realm, &entry).await.map(|_| ());
        // Like node: stay until nothing is scheduled (unless it already failed).
        if result.is_ok() {
            js.idle(realm).await;
        }
        let output = js.take_output(realm).await;
        let cleanup = temporary.close().await;
        Ok(ScriptOutcome { result: result.and(cleanup), output })
    }

    fn next_fiber_id(&self) -> String {
        format!("{}-{}", self.inner.run, self.inner.next_fiber.fetch_add(1, Ordering::Relaxed) + 1)
    }

    async fn run(&self, plugin: &str, input: Value, parent: Option<String>, detached: bool) -> Result<Value, KernelError> {
        let id = self.next_fiber_id();
        self.start(plugin, input, parent, detached, id).await
    }

    async fn start(&self, plugin: &str, input: Value, parent: Option<String>, detached: bool, id: String) -> Result<Value, KernelError> {
        self.start_invocation(plugin, input, parent, detached, id, FiberInvocation::Tool).await
    }

    async fn start_invocation(&self, plugin: &str, input: Value, parent: Option<String>, detached: bool, id: String, invocation: FiberInvocation) -> Result<Value, KernelError> {
        let is_tool = matches!(invocation, FiberInvocation::Tool);
        let inner = &self.inner;
        self.wait_loaded(plugin).await?;
        let target = self.find(plugin).ok_or_else(|| KernelError::UnknownPlugin(plugin.into()))?;
        if is_tool && !target.callable {
            return Err(KernelError::Fiber(format!("{plugin} has no onHandle; it cannot be called")));
        }
        let (cancel, mut cancelled) = watch::channel(false);
        {
            let mut fibers = inner.fibers.lock().expect("fiber table poisoned");
            let depth = match &parent {
                Some(parent) => fibers.get(parent).ok_or_else(|| KernelError::Fiber("the parent fiber has finished".into()))?.depth + 1,
                None => 0,
            };
            if depth > inner.max_depth {
                return Err(KernelError::TooDeep(inner.max_depth));
            }
            // Admission and reservation share the fiber-table lock. A
            // concurrent third call is refused before Spawn or plugin JS.
            if is_tool && let Some(parent) = &parent
                && let Some(budget) = fibers.get_mut(parent).and_then(|f| f.child_budgets.get_mut(plugin))
            {
                if budget.started >= budget.limit {
                    return Err(KernelError::ChildLimit { plugin: plugin.into(), limit: budget.limit, started: budget.started });
                }
                budget.started += 1;
            }
            fibers.insert(id.clone(), ActiveFiber { depth, parent: parent.clone(), cancel, child_budgets: HashMap::new() });
        }
        let event = match &invocation { FiberInvocation::Tool => None, FiberInvocation::Event { name, .. } => Some(name.clone()), FiberInvocation::Command { name, .. } => Some(format!("command:{name}")) };
        let fiber = Fiber { id: id.clone(), parent, plugin: plugin.to_string(), detached, input, event };
        self.emit(&FiberEvent::Spawn { at_ms: now_ms(), fiber: fiber.clone() });
        let mut running = RunningFiber { kernel: self.clone(), id: Some(id.clone()), started: Instant::now() };
        let invoke = async {
            match invocation {
                FiberInvocation::Tool => inner.js.run_fiber(target.realm, &target.entry, &fiber.input, &id).await,
                FiberInvocation::Event { callback, .. } => inner.js.call_event(target.realm, callback, &fiber.input, &id).await,
                FiberInvocation::Command { callback, .. } => inner.js.call_command(target.realm, callback, &fiber.input, &id).await,
            }
        };
        let result = tokio::select! {
            biased;
            _ = cancelled.wait_for(|value| *value) => Err(JsError { kind: FailureKind::Cancelled, message: "fiber cancelled by host".into(), ..JsError::default() }),
            result = invoke => result,
        };
        let exit = match &result {
            Ok(output) => Exit::Done { output: output.clone() },
            Err(e) => match e.kind {
                FailureKind::Failed => Exit::Failed { error: e.message.clone(), stack: e.stack.clone() },
                FailureKind::Cancelled => Exit::Cancelled { reason: e.message.clone() },
                FailureKind::CpuLimit | FailureKind::MemoryLimit => Exit::BudgetExceeded {
                    resource: if e.kind == FailureKind::CpuLimit { BudgetResource::Cpu } else { BudgetResource::Memory }, error: e.message.clone(),
                },
            },
        };
        running.finish(exit);
        result.map_err(|e| match e.kind {
            FailureKind::Failed => KernelError::Fiber(e.message),
            FailureKind::Cancelled => KernelError::Cancelled(e.message),
            FailureKind::CpuLimit | FailureKind::MemoryLimit => KernelError::BudgetExceeded {
                resource: if e.kind == FailureKind::CpuLimit { BudgetResource::Cpu } else { BudgetResource::Memory }, error: e.message,
            },
        })
    }

    fn emit(&self, event: &FiberEvent) {
        let hooks = self.inner.hooks.read().expect("hooks poisoned").clone();
        for hook in hooks {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hook.on_event(event))).is_err() {
                eprintln!("wendy: a fiber event hook panicked; other hooks will still receive this event");
            }
        }
    }
}

/// A loaded plugin, copied out of the table.
struct LoadedRef {
    realm: RealmId,
    entry: String,
    callable: bool,
    methods: Vec<String>,
    state: watch::Sender<LoadState>,
}

/// `kernel.*` ops from plugin code: `plugins.<name>(input)` awaited,
/// `ctx.<method>(...)` (owned by `plugin`), `plugins.<name>.loaded`, and
/// the current fiber's info and structured records.
struct Bridge(Weak<Inner>);

#[async_trait::async_trait]
impl HostBridge for Bridge {
    fn on_event(&self, event: FiberEvent) {
        if let Some(inner) = self.0.upgrade() { Kernel { inner }.emit(&event); }
    }

    async fn call(&self, caller: Caller, op: &str, args: Value) -> Result<Value, String> {
        let kernel = Kernel { inner: self.0.upgrade().ok_or("the kernel has stopped")? };
        if op.starts_with("kernel.event_") { return kernel.event_op(caller, op, args).await; }
        if op.starts_with("kernel.command_") { return kernel.command_op(caller, op, args).await; }
        if op == "kernel.fiber_info" {
            let id = caller.fiber.as_deref().ok_or("fiber.info() requires a running fiber")?;
            let fibers = kernel.inner.fibers.lock().expect("fiber table poisoned");
            let active = fibers.get(id).ok_or("the fiber has finished")?;
            let parent = active.parent.clone();
            let mut ancestors = Vec::new();
            let mut cursor = parent.clone();
            while let Some(ancestor) = cursor {
                cursor = fibers.get(&ancestor).and_then(|f| f.parent.clone());
                ancestors.push(ancestor);
            }
            return Ok(serde_json::json!({ "id": id, "parent": parent, "ancestors": ancestors }));
        }
        if op == "kernel.record" {
            let id = caller.fiber.as_ref().ok_or("fiber.record() requires a running fiber")?;
            if !kernel.inner.fibers.lock().expect("fiber table poisoned").contains_key(id) {
                return Err("the fiber has finished".into());
            }
            let data = args.get("data").cloned().ok_or("missing record data")?;
            kernel.emit(&FiberEvent::Record { at_ms: now_ms(), id: id.clone(), data });
            return Ok(Value::Null);
        }
        let plugin = args.get("plugin").and_then(Value::as_str).ok_or("missing plugin name")?;
        match op {
            "kernel.limit_children" => {
                let id = caller.fiber.as_deref().ok_or("child budgets require a running fiber")?;
                let limit = args.get("limit").and_then(Value::as_u64).filter(|n| *n <= 9_007_199_254_740_991).ok_or("limit must be a nonnegative safe integer")?;
                let restored = args.get("started").and_then(Value::as_u64).filter(|n| *n <= 9_007_199_254_740_991).ok_or("started must be a nonnegative safe integer")?;
                if kernel.find(plugin).is_none() { return Err(format!("no plugin named {plugin}")); }
                let mut fibers = kernel.inner.fibers.lock().expect("fiber table poisoned");
                let active = fibers.get_mut(id).ok_or("the fiber has finished")?;
                let budget = active.child_budgets.entry(plugin.into()).or_insert(ChildBudget { limit, started: restored });
                if limit > budget.limit { return Err("a child budget cannot be increased".into()); }
                budget.limit = limit;
                budget.started = budget.started.max(restored);
                Ok(budget.value())
            }
            "kernel.child_budget" => {
                let id = caller.fiber.as_deref().ok_or("child budgets require a running fiber")?;
                let fibers = kernel.inner.fibers.lock().expect("fiber table poisoned");
                let active = fibers.get(id).ok_or("the fiber has finished")?;
                Ok(active.child_budgets.get(plugin).map(|budget| budget.value()).unwrap_or(Value::Null))
            }
            "kernel.spawn" => {
                let input = args.get("input").cloned().unwrap_or(Value::Null);
                kernel.run(plugin, input, caller.fiber, false).await.map_err(|e| e.to_string())
            }
            "kernel.method" => {
                let method = args.get("method").and_then(Value::as_str).ok_or("missing method name")?;
                let call_args = args.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
                kernel.method(plugin, method, call_args, caller.fiber).await.map_err(|e| e.to_string())
            }
            "kernel.loaded" => kernel.wait_loaded(plugin).await.map(|_| Value::Null).map_err(|e| e.to_string()),
            other => Err(format!("unknown kernel op {other}")),
        }
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

#[cfg(test)]
mod identity_tests {
    use super::*;
    #[tokio::test]
    async fn kernels_created_together_never_share_fiber_ids() {
        let mut ids = HashSet::new();
        for _ in 0..100 {
            let kernel = Kernel::new(KernelConfig::new(Io::new(), "/"));
            assert!(ids.insert(kernel.next_fiber_id()));
            assert!(ids.insert(kernel.next_fiber_id()));
        }
    }
}
