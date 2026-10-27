//! ply-kernel: plugins with three hooks, every `onHandle` run a fiber.
//!
//! Important decisions:
//! - Loading a plugin reads its entry module (`main.ts`, ...) for what it
//!   is ([`PluginSpec`]: name = directory, description and input/output
//!   types from `onHandle`), starts its realm, evaluates the module, and
//!   then runs its `onLoad` in the background, after the plugins it imports
//!   by name (`import { context } from "ply:plugins"`) have loaded.
//!   [`Kernel::ready`] waits for every `onLoad`; after it, waiting for a
//!   plugin that is not loaded fails at once.
//! - What `onLoad` returns is mounted on that plugin's handle: plain members
//!   as a copy, functions as calls into the plugin's realm.
//! - A fiber is one run of a plugin's `onHandle`, event callback or command.
//!   An onHandle call is started by the host ([`Kernel::call`], or
//!   [`Kernel::wake`] without waiting) or by plugin code awaiting
//!   `plugins.<name>(input)`, which makes it a child of the fiber that
//!   awaited. Fibers of one plugin share its realm and its closures; fibers
//!   of different plugins run in parallel. Nesting is bounded. Fibers are
//!   the kernel's: plugin code never sees one, and plugins relate through
//!   inputs, return values and handles, not through who called them.
//! - A cancelled fiber exits at once; its catch/finally then run within a
//!   bounded cleanup. [`Kernel::close`] cancels what runs, then calls each
//!   plugin's `onDispose` before retiring its realm.
//! - What is recorded is fibers: every spawn, plugin event and exit is a
//!   [`FiberEvent`] handed to the registered [`FiberHook`]s, as core defines it.
//! - Plugin code is read through its own read-only namespace, separate from
//!   the workspace the plugins work on.
//! - The default plugins are plain plugins shipped inside the binary.

mod signature;
mod signature_modules;
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
use ply_jsc::{FailureKind, Caller, HostBridge, JsRuntime, ModuleSource, RealmConfig, RealmId};

pub use ply_core::{CleanupStatus, BudgetResource, Exit, Fiber, FiberEvent, FiberHook, Field, Mounts, Payload, PluginSpec, ToolSchema, Type};
pub use ply_jsc::{
    DiskStorage, Io, JsError, Limits, MemoryStorage, MountInfo, MountMode, Output, SqliteStorage, Storage, StorageInfo,
    StorageSpec,
};

// Build-time embedding preserves each plugin's relative module layout.
include!(concat!(env!("OUT_DIR"), "/plugins.rs"));

pub struct KernelConfig {
    /// The workspace the plugins work on.
    pub io: Io,
    /// Where plugins start (relative paths resolve here).
    pub cwd: String,
    /// `process.env` of every plugin.
    pub env: BTreeMap<String, String>,
    /// JSON passed to each named plugin's onLoad; interpreted by that plugin.
    pub plugin_options: BTreeMap<String, Value>,
    pub limits: Limits,
    /// How deep fibers may nest (a fiber awaiting a fiber awaiting ...).
    pub max_depth: usize,
}

impl KernelConfig {
    pub fn new(io: Io, cwd: impl Into<String>) -> Self {
        let cwd = cwd.into();
        let env = BTreeMap::from([("HOME".to_string(), cwd.clone()), ("USER".to_string(), "ply".to_string())]);
        KernelConfig { io, cwd, env, plugin_options: BTreeMap::new(), limits: Limits::default(), max_depth: 16 }
    }
}

pub enum PluginSource {
    /// A plugin directory (entry `main.ts`, `index.ts`, `main.js` or
    /// `index.js`); its name is the directory's name.
    Dir(PathBuf),
    /// A plugin shipped inside the binary: its name and entry source.
    Embedded { name: String, source: String },
    /// A complete read-only plugin module directory, including main.ts.
    Bundled { name: String, files: Vec<(String, String)> },
}

impl PluginSource {
    /// agent, bash, context, create, deepseek, edit, memory, read, remove, skill.
    pub fn defaults() -> Vec<PluginSource> {
        BUILTIN_PLUGINS.iter().filter(|(name, _)| !matches!(*name, "mimo" | "jev" | "approval" | "tui"))
            .map(|(name, _)| Self::builtin(name).expect("known built-in")).collect()
    }

    /// An explicitly selected built-in plugin, including all relative modules.
    pub fn builtin(name: &str) -> Option<PluginSource> {
        BUILTIN_PLUGINS.iter().find(|(entry, _)| *entry == name).map(|(name, files)| PluginSource::Bundled {
            name: (*name).into(), files: files.iter().map(|(path, source)| ((*path).into(), (*source).into())).collect(),
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
    cancel: watch::Sender<bool>,
}

struct Inner {
    js: JsRuntime,
    io: Io,
    code: Io,
    cwd: String,
    env: BTreeMap<String, String>,
    plugin_options: BTreeMap<String, Value>,
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
    /// Active fibers, for depth checks and cancellation.
    fibers: Mutex<HashMap<String, ActiveFiber>>,
    /// Plugins whose onLoad waits for the plugins they import by name.
    waiting: Mutex<HashMap<String, Vec<String>>>,
    /// Set by `close`: no new fiber starts.
    closing: AtomicBool,
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

/// A path from `plugin` back to itself through plugins still waiting on
/// their imports, if `plugin` waiting on `dependencies` would close one.
fn import_cycle(waiting: &HashMap<String, Vec<String>>, plugin: &str, dependencies: &[String]) -> Option<String> {
    let mut paths: Vec<Vec<&str>> = dependencies.iter().map(|dependency| vec![plugin, dependency.as_str()]).collect();
    let mut seen = HashSet::new();
    while let Some(path) = paths.pop() {
        let last = *path.last().expect("paths are never empty");
        if last == plugin {
            return Some(path.join(" -> "));
        }
        if !seen.insert(last) {
            continue;
        }
        for next in waiting.get(last).into_iter().flatten() {
            let mut longer = path.clone();
            longer.push(next);
            paths.push(longer);
        }
    }
    None
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
                plugin_options: config.plugin_options,
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
                waiting: Mutex::new(HashMap::new()),
                closing: AtomicBool::new(false),
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
        let signature = signature_modules::read(&inner.code, &entry, &root).await.map_err(KernelError::Load)?;
        let dependencies: Vec<String> = signature_modules::plugin_dependencies(&inner.code, &entry, &root).await
            .map_err(KernelError::Load)?.into_iter().filter(|dependency| *dependency != name).collect();
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
            // Its dependencies may load after it.
            let kernel = self.clone();
            tokio::spawn(async move {
                let ordered = kernel.wait_dependencies(&name, &dependencies).await;
                let started = ordered.is_ok();
                let result = match ordered {
                    Ok(()) => {
                        let options = kernel.inner.plugin_options.get(&name).cloned().unwrap_or_else(|| serde_json::json!({}));
                        kernel.inner.js.run_load(realm, &entry, &options).await.map_err(|e| e.message)
                    }
                    Err(message) => Err(message),
                };
                let mounts = result.and_then(|v| serde_json::from_value::<Mounts>(v).map_err(|e| e.to_string()));
                if let Err(error) = mounts {
                    kernel.finish_load(&name, Err(error));
                    // onDispose cleans up after an onLoad that ran.
                    kernel.dispose_realm(realm, started.then_some(entry.as_str())).await;
                } else {
                    kernel.finish_load(&name, mounts);
                }
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

    /// Wait for the plugins `plugin` imports by name to load. An import
    /// cycle, or a dependency that failed, fails this load.
    async fn wait_dependencies(&self, plugin: &str, dependencies: &[String]) -> Result<(), String> {
        {
            let mut waiting = self.inner.waiting.lock().expect("load waits poisoned");
            if let Some(cycle) = import_cycle(&waiting, plugin, dependencies) {
                return Err(format!("plugin import cycle: {cycle}"));
            }
            waiting.insert(plugin.into(), dependencies.to_vec());
        }
        let mut result = Ok(());
        for dependency in dependencies {
            if let Err(error) = self.wait_loaded(dependency).await {
                result = Err(format!("imported plugin {dependency} did not load: {error}"));
                break;
            }
        }
        self.inner.waiting.lock().expect("load waits poisoned").remove(plugin);
        result
    }

    /// Run a plugin's `onDispose` (given its entry), then retire its realm.
    async fn dispose_realm(&self, realm: RealmId, entry: Option<&str>) {
        if let Some(entry) = entry { let _ = self.inner.js.dispose(realm, entry).await; }
        self.inner.realms.lock().expect("realm list poisoned").remove(&realm);
        let _ = self.inner.js.close_realm(realm).await;
    }

    /// Cancel every running fiber and give it its cleanup, then call each
    /// plugin's `onDispose`, last loaded first, and retire every realm.
    /// No fiber starts once closing has begun.
    pub async fn close(&self) {
        let inner = &self.inner;
        inner.closing.store(true, Ordering::SeqCst);
        for fiber in inner.fibers.lock().expect("fiber table poisoned").values() {
            fiber.cancel.send_replace(true);
        }
        let realms: Vec<RealmId> = inner.realms.lock().expect("realm list poisoned").iter().copied().collect();
        // Cleanups have two seconds; allow for their messages to arrive.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        for realm in &realms {
            let _ = tokio::time::timeout_at(deadline, inner.js.quiet(*realm)).await;
        }
        let mut plugins: Vec<(u64, RealmId, String)> = inner.plugins.read().expect("plugin table poisoned")
            .iter().map(|p| (p.mounted, p.realm, p.entry.clone())).collect();
        plugins.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, realm, entry) in plugins {
            if inner.realms.lock().expect("realm list poisoned").contains(&realm) {
                self.dispose_realm(realm, Some(&entry)).await;
            }
        }
        let rest: Vec<RealmId> = inner.realms.lock().expect("realm list poisoned").drain().collect();
        for realm in rest {
            let _ = inner.js.close_realm(realm).await;
        }
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
            PluginSource::Bundled { name, files } => {
                if !PluginSpec::valid_name(&name) || !files.iter().any(|(path, _)| path == "main.ts") {
                    return Err(load_err("invalid bundled plugin name or missing main.ts".into()));
                }
                let mut paths = HashSet::new();
                for (path, _) in &files {
                    if !paths.insert(path) || Path::new(path).components().any(|part| !matches!(part, std::path::Component::Normal(_))) {
                        return Err(load_err(format!("invalid or duplicate bundled module path: {path}")));
                    }
                }
                let root = format!("/ply/plugins/{name}");
                let storage = MemoryStorage::with_files(files.iter().map(|(path, source)| (path.as_str(), source.as_bytes())));
                self.inner.code.mount(&root, Arc::new(storage), MountMode::ReadOnly).map_err(|e| load_err(e.to_string()))?;
                Ok((name, root.clone(), format!("{root}/main.ts")))
            }
            PluginSource::Embedded { name, source } => {
                let root = format!("/ply/plugins/{name}");
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
        if self.inner.closing.load(Ordering::SeqCst) {
            return Err(KernelError::Cancelled("the kernel is closing".into()));
        }
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

    /// Call a function one plugin's `onLoad` returned, as the host.
    pub async fn call_plugin_method(&self, plugin: &str, method: &str, args: Vec<Value>) -> Result<Value, KernelError> {
        self.method(plugin, method, args, None).await
    }

    /// A mounted method runs in its plugin's realm on behalf of the calling
    /// fiber, whose `plugins.x` awaits inside it become its children.
    async fn method(&self, plugin: &str, method: &str, args: Vec<Value>, caller: Option<String>) -> Result<Value, KernelError> {
        self.wait_loaded(plugin).await?;
        let target = self.find(plugin).ok_or_else(|| KernelError::UnknownPlugin(plugin.into()))?;
        if !target.methods.iter().any(|m| m == method) {
            return Err(KernelError::Fiber(format!("{method} is not a function {plugin} mounted")));
        }
        self.inner.js.call_method(target.realm, &target.entry, method, &args, caller).await.map_err(|e| KernelError::Fiber(e.message))
    }

    /// What a plugin printed (console, process.stdout/stderr) since last asked.
    pub async fn take_output(&self, plugin: &str) -> Option<Output> {
        let target = self.find(plugin)?;
        Some(self.inner.js.take_output(target.realm).await)
    }

    /// Run a TS/JS file like node would (its directory is the root it may
    /// import from), with ply:plugins available, until nothing is scheduled.
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
        if inner.closing.load(Ordering::SeqCst) {
            return Err(KernelError::Cancelled("the kernel is closing".into()));
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
            fibers.insert(id.clone(), ActiveFiber { depth, cancel });
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
                eprintln!("ply: a fiber event hook panicked; other hooks will still receive this event");
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

/// `kernel.*` ops from plugin code: `plugins.<name>(input)` awaited, a
/// method `plugins.<name>` mounted, events and commands.
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
        let plugin = args.get("plugin").and_then(Value::as_str).ok_or("missing plugin name")?;
        match op {
            "kernel.spawn" => {
                let input = args.get("input").cloned().unwrap_or(Value::Null);
                kernel.run(plugin, input, caller.fiber, false).await.map_err(|e| e.to_string())
            }
            "kernel.method" => {
                let method = args.get("method").and_then(Value::as_str).ok_or("missing method name")?;
                let call_args = args.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
                kernel.method(plugin, method, call_args, caller.fiber.clone()).await.map_err(|e| e.to_string())
            }
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
