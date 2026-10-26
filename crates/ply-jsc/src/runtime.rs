//! Realms: one plugin, one JSC context group, one OS thread; many fibers.
//!
//! Important decisions:
//! - JSC is single-threaded per context group, so every realm gets its own
//!   group on its own thread. A plugin that blocks its thread (a sync fs
//!   call, a long computation) holds up only itself. Callers talk to a realm
//!   through messages; every reply is a oneshot.
//! - A realm runs every fiber of its plugin. They share the module's
//!   closures and interleave at `await`, like requests in one node process.
//! - OXC lowers async/await (including async generators) to Promise
//!   continuations. Registering a reaction captures its consumer's scope
//!   in a native callable; Rust queues and executes the continuation with
//!   that identity. Sharing a Promise does not share its producer's scope.
//! - Host IO runs on tokio; completion returns to the owning realm. User
//!   continuations run as separate messages, so terminating one JS turn
//!   does not discard other fibers' continuations in JSC's microtask queue.
//!   Sync host ops still block this realm, but observe cancellation.
//! - Nothing that arrives from JS is trusted. The prelude shares the realm
//!   with plugin code, which can replace any global the prelude uses, so
//!   every host op re-validates here: a module loads only if ply itself
//!   resolved it inside the plugin root; settles and timers must belong to
//!   this realm.
//! - Each entry into JS gets a CPU budget and the process a memory ceiling
//!   (a watchdog checks both every 100 ms of CPU). A turn over either is
//!   terminated and its owning invocation fails; siblings remain runnable.
//!   The memory ceiling measures the process, not individual fibers.
//! - Plugin source is read through the IO layer (a namespace of its own),
//!   so plugins can live on any storage.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::c_void;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};
use ply_libc::{EntryKind, Errno, ExecSpec, Fs, Io, SysError, path as lpath};
use ply_core::{CleanupStatus, FiberEvent};

use crate::ffi::*;
use crate::http::Http;
use crate::checkpoints::Checkpoints;
use crate::transpile::{lower_async, transpile};

const PRELUDE: &str = include_str!("prelude.js");
const ASYNC: &str = include_str!("async.js");
const PI_AI: &str = include_str!("vendor/pi-ai.js");
const BUILTINS: &[&str] = &[
    "fs",
    "fs/promises",
    "path",
    "path/posix",
    "process",
    "buffer",
    "events",
    "util",
    "os",
    "timers",
    "timers/promises",
    "child_process",
];
const WATCHDOG_TICK: f64 = 0.1;

/// Where a realm's own code comes from: a directory in a (read-only)
/// namespace of its own.
#[derive(Clone)]
pub struct ModuleSource {
    pub io: Io,
    pub root: String,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// CPU time one entry into JS may use before it is terminated.
    pub turn_cpu: Duration,
    /// Process footprint above which a running turn is terminated.
    pub memory_mb: Option<u64>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { turn_cpu: Duration::from_secs(10), memory_mb: Some(4096) }
    }
}

/// Who started a bridged op. Filled in by ply, never by plugin code.
#[derive(Debug, Clone)]
pub struct Caller {
    pub realm: String,
    /// Native identity, distinct even when a script uses a plugin's label.
    pub realm_id: RealmId,
    /// This operation originated in the plugin's onLoad invocation.
    pub loading: bool,
    /// The fiber owning the running continuation, if any.
    pub fiber: Option<String>,
}

/// The host side of `kernel.*` ops: awaiting another plugin, calling a
/// function on `ctx`, waiting for a plugin to load.
#[async_trait::async_trait]
pub trait HostBridge: Send + Sync {
    async fn call(&self, caller: Caller, op: &str, args: Value) -> Result<Value, String>;
    /// Runtime lifecycle observations do not require an active JS fiber.
    fn on_event(&self, _event: FiberEvent) {}
}

pub struct RealmConfig {
    pub name: String,
    /// The namespace the realm's code works on.
    pub io: Io,
    /// Starting directory inside the namespace.
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub argv: Vec<String>,
    pub modules: ModuleSource,
    pub limits: Limits,
    pub bridge: Option<Arc<dyn HostBridge>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RealmId(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailureKind { #[default] Failed, Cancelled, CpuLimit, MemoryLimit }

#[derive(Debug, Clone, PartialEq, Default)]
pub struct JsError {
    pub kind: FailureKind,
    /// `Name: message` of what was thrown.
    pub message: String,
    /// Set when the code called `process.exit(code)`.
    pub exit_code: Option<i32>,
    /// The JS stack, when the thrown value had one.
    pub stack: Option<String>,
}

impl std::fmt::Display for JsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JsError {}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

pub(crate) enum OpResult {
    Json(Value),
    Bytes(Vec<u8>),
    Error(Value),
}

impl OpResult {
    pub(crate) fn error(code: &str, message: impl Into<String>) -> OpResult {
        OpResult::Error(json!({ "code": code, "message": message.into() }))
    }
}

type Reply<T> = oneshot::Sender<T>;

enum Msg {
    /// `api[method](callId, ...args)`, as an entry tagged with `fiber`; the
    /// prelude reports back via `settle`.
    Api { id: u64, method: &'static str, args: Vec<String>, fiber: Option<String>, cancel: Arc<CallCancellation>, reply: Reply<Result<Value, JsError>> },
    TakeOutput(Reply<Output>),
    Idle(Reply<()>),
    Directory(Value),
    RawHostCall(String, Value, Reply<Value>),
    Completion(u64, OpResult),
    Timer(u64),
    Continuation(u64),
    CancelCall(u64),
    DiscardDisposals(Vec<u64>),
    Shutdown,
}

struct RealmThread {
    tx: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
    cancel: watch::Sender<bool>,
    next_call: AtomicU64,
}

/// Owns the realm threads. Dropping it stops them.
pub struct JsRuntime {
    handle: tokio::runtime::Handle,
    realms: Mutex<HashMap<RealmId, RealmThread>>,
    next_realm: AtomicUsize,
}

struct StartingRealm<'a> {
    runtime: &'a JsRuntime,
    id: Option<RealmId>,
}

impl Drop for StartingRealm<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            // close_realm starts cleanup immediately, even if its returned
            // join handle is dropped while this create future is cancelled.
            drop(self.runtime.close_realm(id));
        }
    }
}

fn gone() -> JsError {
    JsError { message: "the realm has stopped".into(), ..JsError::default() }
}

impl JsRuntime {
    /// Async work runs on the current tokio runtime.
    pub fn start() -> JsRuntime {
        Self::start_with(tokio::runtime::Handle::current())
    }

    pub fn start_with(handle: tokio::runtime::Handle) -> JsRuntime {
        JsRuntime { handle, realms: Mutex::new(HashMap::new()), next_realm: AtomicUsize::new(0) }
    }

    pub async fn create_realm(&self, config: RealmConfig) -> Result<RealmId, JsError> {
        // Never reuse IDs: late messages for a retired realm cannot reach a
        // new realm occupying the same registry slot.
        let id = RealmId(self.next_realm.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| JsError { message: "realm ID space exhausted".into(), ..JsError::default() })?);
        let (tx, rx) = channel();
        let (ready, ready_rx) = oneshot::channel();
        let loop_tx = tx.clone();
        let handle = self.handle.clone();
        let (cancel, cancelled) = watch::channel(false);
        let thread = std::thread::Builder::new()
            .name(format!("ply-js-{}", config.name))
            .spawn(move || engine_main(id, config, rx, loop_tx, handle, ready, cancelled))
            .map_err(|e| JsError { message: format!("starting realm thread: {e}"), ..JsError::default() })?;
        self.realms.lock().expect("realm table poisoned").insert(id, RealmThread { tx, thread: Some(thread), cancel, next_call: AtomicU64::new(0) });
        let mut starting = StartingRealm { runtime: self, id: Some(id) };
        ready_rx.await.unwrap_or_else(|_| Err(gone()))?;
        starting.id = None;
        Ok(id)
    }

    /// Retire a realm immediately; await the returned handle to wait for
    /// its thread and JSC context to finish. Dropping the handle still reaps
    /// the thread. Queued/future calls observe a stopped realm.
    pub fn close_realm(&self, id: RealmId) -> tokio::task::JoinHandle<Result<(), JsError>> {
        let realm = self.realms.lock().expect("realm table poisoned").remove(&id);
        if let Some(realm) = &realm {
            realm.cancel.send_replace(true);
            let _ = realm.tx.send(Msg::Shutdown);
        }
        self.handle.spawn_blocking(move || {
            if let Some(mut realm) = realm
                && let Some(thread) = realm.thread.take()
            {
                thread.join().map_err(|_| JsError { message: "realm thread panicked while closing".into(), ..JsError::default() })?;
            }
            Ok(())
        })
    }

    fn send(&self, realm: RealmId, msg: Msg) -> bool {
        let realms = self.realms.lock().expect("realm table poisoned");
        realms.get(&realm).is_some_and(|r| r.tx.send(msg).is_ok())
    }

    async fn api(&self, realm: RealmId, method: &'static str, args: Vec<String>, fiber: Option<String>) -> Result<Value, JsError> {
        let (reply, rx) = oneshot::channel();
        let (id, tx) = {
            let realms = self.realms.lock().expect("realm table poisoned");
            let realm = realms.get(&realm).ok_or_else(gone)?;
            (realm.next_call.fetch_add(1, Ordering::Relaxed) + 1, realm.tx.clone())
        };
        let args = if method == "eval" {
            vec![lower_async(&args[0], Path::new("ply:eval")).map_err(|message| JsError { message, ..JsError::default() })?]
        } else { args };
        let cancel = Arc::new(CallCancellation::new());
        let mut guard = CallGuard { tx: tx.clone(), id: Some(id), cancel: cancel.clone() };
        tx.send(Msg::Api { id, method, args, fiber, cancel, reply }).map_err(|_| gone())?;
        let result = rx.await.unwrap_or_else(|_| Err(gone()));
        guard.id = None;
        result
    }

    /// Load `module` (a path in the realm's module namespace) and wait for
    /// it to finish evaluating. Returns the names it exports.
    pub async fn run_main(&self, realm: RealmId, module: &str) -> Result<Value, JsError> {
        self.api(realm, "main", vec![module.into()], None).await
    }

    /// Call an export of a module (`"a.b"` walks properties), awaiting the
    /// result if it is a promise.
    pub async fn call(&self, realm: RealmId, module: &str, export: &str, args: Vec<Value>) -> Result<Value, JsError> {
        self.api(realm, "call", vec![module.into(), export.into(), Value::Array(args).to_string()], None).await
    }

    /// Run `onHandle(input)` of a module as fiber `fiber`.
    pub async fn run_fiber(&self, realm: RealmId, module: &str, input: &Value, fiber: &str) -> Result<Value, JsError> {
        self.api(realm, "run", vec![module.into(), input.to_string()], Some(fiber.to_string())).await
    }

    /// Invoke a callback registered in this realm, with its own fiber identity.
    pub async fn call_event(&self, realm: RealmId, callback: u64, input: &Value, fiber: &str) -> Result<Value, JsError> {
        self.api(realm, "event", vec![callback.to_string(), input.to_string()], Some(fiber.into())).await
    }

    /// Invoke a plugin-owned command callback as its own fiber.
    pub async fn call_command(&self, realm: RealmId, callback: u64, input: &Value, fiber: &str) -> Result<Value, JsError> {
        self.api(realm, "command", vec![callback.to_string(), input.to_string()], Some(fiber.into())).await
    }

    /// Run the module's `onLoad` (if it has one) and keep what it returns.
    /// Returns `{ data, methods }`: its plain members, and the names of its
    /// function members.
    pub async fn run_load(&self, realm: RealmId, module: &str, options: &Value) -> Result<Value, JsError> {
        self.api(realm, "load", vec![module.into(), options.to_string()], None).await
    }

    /// Call a function member of what the module's `onLoad` returned, on
    /// behalf of fiber `fiber` (whose `plugins.x` awaits it makes children).
    pub async fn call_method(
        &self,
        realm: RealmId,
        module: &str,
        method: &str,
        args: &[Value],
        fiber: Option<String>,
        origin: Value,
    ) -> Result<Value, JsError> {
        self.api(realm, "method", vec![module.into(), method.into(), Value::Array(args.to_vec()).to_string(), origin.to_string()], fiber).await
    }

    /// Evaluate a plain-JS script in the realm (awaiting a promise result).
    pub async fn eval(&self, realm: RealmId, code: &str) -> Result<Value, JsError> {
        self.api(realm, "eval", vec![code.into()], None).await
    }

    /// Everything the realm wrote to stdout/stderr since the last call.
    pub async fn take_output(&self, realm: RealmId) -> Output {
        let (reply, rx) = oneshot::channel();
        if !self.send(realm, Msg::TakeOutput(reply)) {
            return Output::default();
        }
        rx.await.unwrap_or_default()
    }

    /// What the imported plugin directory and `ctx` hold: every plugin, as the
    /// kernel describes it (name, description, tool schema, what its
    /// `onLoad` mounted, load state).
    pub fn set_directory(&self, realm: RealmId, directory: Value) {
        self.send(realm, Msg::Directory(directory));
    }

    /// Send one host call exactly as given, the way a prelude that plugin
    /// code had tampered with could. For testing the host-side checks.
    #[doc(hidden)]
    pub async fn raw_host_call(&self, realm: RealmId, op: &str, args: Value) -> Value {
        let (reply, rx) = oneshot::channel();
        if !self.send(realm, Msg::RawHostCall(op.into(), args, reply)) {
            return Value::Null;
        }
        rx.await.unwrap_or(Value::Null)
    }

    /// Resolves once the realm has nothing left to do: no timers armed, no
    /// host ops in flight, no calls pending (node's "event loop is empty").
    pub async fn idle(&self, realm: RealmId) {
        let (reply, rx) = oneshot::channel();
        if self.send(realm, Msg::Idle(reply)) {
            let _ = rx.await;
        }
    }
}

impl Drop for JsRuntime {
    fn drop(&mut self) {
        let realms: Vec<_> = self.realms.lock().expect("realm table poisoned").drain().map(|(_, realm)| realm).collect();
        for r in &realms {
            r.cancel.send_replace(true);
            let _ = r.tx.send(Msg::Shutdown);
        }
        for mut r in realms {
            if let Some(t) = r.thread.take() {
                let _ = t.join();
            }
        }
    }
}

// ---------------------------------------------------------------- engine

// Dropping a panicking/aborted host task must settle its owner too.
struct HostCompletion { tx: Sender<Msg>, id: u64, completed: bool }
impl HostCompletion {
    fn finish(mut self, result: OpResult) { self.completed = true; let _ = self.tx.send(Msg::Completion(self.id, result)); }
}
impl Drop for HostCompletion {
    fn drop(&mut self) {
        if !self.completed { let _ = self.tx.send(Msg::Completion(self.id, OpResult::error("ERR_PLY_HOST_TASK", "host operation panicked or was cancelled"))); }
    }
}

#[derive(Debug)]
struct CallCancellation { flag: AtomicBool, signal: watch::Sender<bool>, timed_out: AtomicBool, deadline: Option<Instant> }
impl CallCancellation {
    fn new() -> Self { Self { flag: AtomicBool::new(false), signal: watch::channel(false).0, timed_out: AtomicBool::new(false), deadline: None } }
    fn stop(&self) { self.flag.store(true, Ordering::Relaxed); self.signal.send_replace(true); }
    fn expire(&self) { if !self.timed_out.swap(true, Ordering::Relaxed) { self.stop(); } }
    fn with_deadline(deadline: Instant) -> Self { Self { deadline: Some(deadline), ..Self::new() } }
    fn is_stopped(&self) -> bool {
        if self.deadline.is_some_and(|deadline| Instant::now() >= deadline) { self.expire(); }
        self.flag.load(Ordering::Relaxed)
    }
}
struct CallGuard { tx: Sender<Msg>, id: Option<u64>, cancel: Arc<CallCancellation> }
impl Drop for CallGuard {
    fn drop(&mut self) { if let Some(id) = self.id { self.cancel.stop(); let _ = self.tx.send(Msg::CancelCall(id)); } }
}

#[derive(Debug)]
struct Invocation { id: u64, fiber: Option<String>, cancel: Arc<CallCancellation>, origin: Value }
type Scope = Option<Arc<Invocation>>;
struct ActiveCall { reply: Reply<Result<Value, JsError>>, scope: Arc<Invocation>, disposals: Vec<u64>, method: &'static str }
struct CleanupRun { owner: Option<String>, watches: Vec<u64>, deadline: tokio::task::AbortHandle }
struct Binding { scope: Scope, fatal: bool }
struct Continuation {
    scope: Scope, fatal: bool, callback: RootedValue, receiver: RootedValue,
    args: Vec<RootedValue>,
}

struct Engine {
    id: RealmId,
    group: JSContextGroupRef,
    ctx: JSGlobalContextRef,
    api: JSObjectRef,
    name: String,
    fs: Fs,
    code: Fs,
    root: String,
    bridge: Option<Arc<dyn HostBridge>>,
    http: Arc<Http>,
    checkpoints: Arc<Checkpoints>,
    terminal: Arc<crate::terminal::Terminal>,
    terminal_owner: Option<u64>,
    /// The plugins the imported directory and `ctx` reflect, as last described
    /// them.
    directory: Value,
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    /// The fiber the running entry belongs to.
    current: Scope,
    binding_class: JSClassRef,
    continuations: HashMap<u64, Continuation>,
    http_owners: HashMap<u64, Scope>,
    cancel_handlers: HashMap<u64, HashMap<u64, RootedValue>>,
    /// Module ids ply resolved for this realm, and those it loaded.
    resolved: HashSet<String>,
    loaded: HashSet<String>,
    /// Armed timers: id → (repeat interval in ms, fiber that armed it).
    timers: HashMap<u64, (Option<f64>, Scope)>,
    /// Async host ops in flight: id → (resolve function, fiber).
    pending_ops: HashMap<u64, (JSObjectRef, Scope)>,
    pending_jobs: HashMap<u64, tokio::task::AbortHandle>,
    timer_jobs: HashMap<u64, tokio::task::AbortHandle>,
    calls: HashMap<u64, ActiveCall>,
    cleanups: HashMap<u64, CleanupRun>,
    idle_waiters: Vec<Reply<()>>,
    next_id: u64,
    tx: Sender<Msg>,
    handle: tokio::runtime::Handle,
    started: Instant,
    cancelled: watch::Receiver<bool>,
}

impl Engine {
    fn next(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn is_idle(&self) -> bool {
        self.timers.is_empty() && self.pending_ops.is_empty() && self.calls.is_empty() && self.cleanups.is_empty() && self.continuations.is_empty()
    }
}

thread_local! {
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
    // Watchdog state; read from inside the watchdog callback, so kept apart
    // from the engine to never collide with a borrow of it.
    static GROUP: Cell<JSContextGroupRef> = const { Cell::new(std::ptr::null()) };
    static TICKS: Cell<u32> = const { Cell::new(0) };
    static BUDGET_TICKS: Cell<u32> = const { Cell::new(u32::MAX) };
    static MEMORY_CEILING: Cell<u64> = const { Cell::new(0) };
    static TERMINATED: RefCell<Option<(FailureKind, String, Scope)>> = const { RefCell::new(None) };
}

/// Short, non-reentrant access to the engine. Never call into JS inside.
fn with<R>(f: impl FnOnce(&mut Engine) -> R) -> R {
    ENGINE.with(|e| f(e.borrow_mut().as_mut().expect("JS engine is running")))
}

unsafe extern "C" fn should_terminate(_ctx: JSContextRef, _context: *mut c_void) -> bool {
    let ticks = TICKS.get() + 1;
    TICKS.set(ticks);
    let stopping = with(|e| *e.cancelled.borrow() || e.current.as_ref().is_some_and(|scope| scope.cancel.is_stopped()));
    let reason = if stopping {
        Some((FailureKind::Cancelled, "fiber cancelled by host".into()))
    } else if ticks >= BUDGET_TICKS.get() {
        Some((FailureKind::CpuLimit, format!("used more than {:.1}s of CPU in one turn", f64::from(BUDGET_TICKS.get()) * WATCHDOG_TICK)))
    } else {
        let ceiling = MEMORY_CEILING.get();
        (ceiling > 0 && footprint() > ceiling).then(|| (FailureKind::MemoryLimit, format!("process memory passed {} MiB", ceiling >> 20)))
    };
    match reason {
        Some((kind, reason)) => {
            let scope = with(|e| e.current.clone());
            TERMINATED.with(|t| *t.borrow_mut() = Some((kind, reason, scope)));
            true
        }
        None => {
            // JSC does not re-arm by itself after the callback says "go on".
            unsafe { JSContextGroupSetExecutionTimeLimit(GROUP.get(), WATCHDOG_TICK, Some(should_terminate), null_mut()) };
            false
        }
    }
}

/// Physical footprint of the whole process (what Activity Monitor shows).
fn footprint() -> u64 {
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V2, (&mut info as *mut libc::rusage_info_v2).cast())
    };
    if ok == 0 { info.ri_phys_footprint } else { 0 }
}

/// Run `f`, which enters JS, as fiber `fiber` with a fresh CPU budget.
fn enter<T>(scope: Scope, f: impl FnOnce() -> T) -> T {
    let previous = with(|e| std::mem::replace(&mut e.current, scope));
    TICKS.set(0);
    let result = f();
    with(|e| e.current = previous);
    result
}

fn engine_main(
    id: RealmId,
    config: RealmConfig,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    handle: tokio::runtime::Handle,
    ready: Reply<Result<(), JsError>>,
    cancelled: watch::Receiver<bool>,
) {
    match init(id, config, tx, handle, cancelled) {
        Ok(()) => { let _ = ready.send(Ok(())); }
        Err(error) => {
            let _ = ready.send(Err(error));
            teardown();
            return;
        }
    }
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Shutdown => break,
            Msg::Api { id, method, args, fiber, cancel, reply } => start_call(id, method, args, fiber, cancel, reply),
            Msg::Continuation(id) => run_continuation(id),
            Msg::CancelCall(id) => fail_call(id, FailureKind::Cancelled, "the awaiting fiber call was cancelled".into()),
            Msg::DiscardDisposals(ids) => unsafe {
                let (ctx, api) = with(|e| (e.ctx, e.api));
                let callback = get_property(ctx, api, "discardDisposals");
                let ids = RootedValue::new(ctx, json_value(ctx, &json!(ids).to_string()));
                let _ = enter(None, || call(ctx, callback as JSObjectRef, &[ids.get()]));
            },
            Msg::TakeOutput(reply) => {
                let out = with(|e| Output {
                    stdout: std::mem::take(&mut e.stdout),
                    stderr: std::mem::take(&mut e.stderr),
                    exit_code: e.exit_code,
                });
                let _ = reply.send(out);
            }
            Msg::Idle(reply) => with(|e| e.idle_waiters.push(reply)),
            Msg::Directory(directory) => with(|e| e.directory = directory),
            Msg::RawHostCall(op, args, reply) => {
                let value = match sync_op(&op, args, None) {
                    OpResult::Json(v) => v,
                    OpResult::Bytes(b) => json!({ "bytes": b.len() }),
                    OpResult::Error(e) => json!({ "__error": e }),
                };
                let _ = reply.send(value);
            }
            Msg::Completion(id, result) => complete(id, result),
            Msg::Timer(id) => fire_timer(id),
        }
        after_turn();
    }
    teardown();
}

fn init(id: RealmId, config: RealmConfig, tx: Sender<Msg>, handle: tokio::runtime::Handle, cancelled: watch::Receiver<bool>) -> Result<(), JsError> {
    let fail = |message: String| JsError { message, ..JsError::default() };
    let root = lpath::normalize(&config.modules.root);
    unsafe {
        let group = JSContextGroupCreate();
        GROUP.set(group);
        let ticks = (config.limits.turn_cpu.as_secs_f64() / WATCHDOG_TICK).ceil().max(1.0) as u32;
        BUDGET_TICKS.set(ticks);
        MEMORY_CEILING.set(config.limits.memory_mb.map(|mb| mb << 20).unwrap_or(0));
        JSContextGroupSetExecutionTimeLimit(group, WATCHDOG_TICK, Some(should_terminate), null_mut());
        let ctx = JSGlobalContextCreateInGroup(group, null_mut());
        let name = JsStr::new(&config.name);
        JSGlobalContextSetName(ctx, name.0);
        ENGINE.with(|e| {
            *e.borrow_mut() = Some(Engine {
                id,
                group,
                ctx,
                api: null_mut(),
                name: config.name.clone(),
                fs: Fs::new(config.io, config.cwd.clone()),
                code: Fs::new(config.modules.io, root.clone()),
                root,
                bridge: config.bridge,
                http: Arc::new(Http::default()),
                checkpoints: Arc::new(Checkpoints::default()),
                terminal: Arc::new(crate::terminal::Terminal::default()),
                terminal_owner: None,
                directory: json!([]),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                current: None,
                binding_class: binding_class(),
                continuations: HashMap::new(),
                http_owners: HashMap::new(),
                cancel_handlers: HashMap::new(),
                resolved: HashSet::new(),
                loaded: HashSet::new(),
                timers: HashMap::new(),
                pending_ops: HashMap::new(),
                pending_jobs: HashMap::new(),
                timer_jobs: HashMap::new(),
                calls: HashMap::new(),
                cleanups: HashMap::new(),
                idle_waiters: Vec::new(),
                next_id: 0,
                tx,
                handle,
                started: Instant::now(),
                cancelled,
            })
        });
        let binder = RootedValue::new(ctx, JSObjectMakeFunctionWithCallback(ctx, JsStr::new("bindContinuation").0, bind_continuation) as JSValueRef);
        let cancel_hook = RootedValue::new(ctx, JSObjectMakeFunctionWithCallback(ctx, JsStr::new("registerPromiseCancel").0, register_promise_cancel) as JSValueRef);
        let unregister_hook = RootedValue::new(ctx, JSObjectMakeFunctionWithCallback(ctx, JsStr::new("unregisterPromiseCancel").0, unregister_promise_cancel) as JSValueRef);
        let bootstrap = RootedValue::new(ctx, eval(ctx, ASYNC, "ply:async").map_err(fail)?);
        let helpers = RootedValue::new(ctx, call(ctx, bootstrap.get() as JSObjectRef, &[binder.get(), cancel_hook.get(), unregister_hook.get()]).map_err(fail)?);
        let lowered = lower_async(PRELUDE, Path::new("ply:prelude")).map_err(fail)?;
        let factory = RootedValue::new(ctx, eval(ctx, "(function(eval, babelHelpers, code) { return eval(code); })", "ply:prelude").map_err(fail)?);
        let intrinsic_eval = RootedValue::new(ctx, eval(ctx, "eval", "ply:prelude").map_err(fail)?);
        let lowered = RootedValue::new(ctx, string_value(ctx, &(lowered + "\n//# sourceURL=ply:prelude")));
        let prelude = RootedValue::new(ctx, call(ctx, factory.get() as JSObjectRef, &[intrinsic_eval.get(), helpers.get(), lowered.get()]).map_err(fail)?);
        let host = RootedValue::new(ctx, JSObjectMakeFunctionWithCallback(ctx, JsStr::new("host").0, host_sync) as JSValueRef);
        let host_async_fn = RootedValue::new(ctx, JSObjectMakeFunctionWithCallback(ctx, JsStr::new("hostAsync").0, host_async) as JSValueRef);
        let settings = json!({ "env": config.env, "argv": config.argv, "version": env!("CARGO_PKG_VERSION") }).to_string();
        let settings = RootedValue::new(ctx, string_value(ctx, &settings));
        let prelude_fn = JSValueToObject(ctx, prelude.get(), null_mut());
        let api = enter(None, || call(ctx, prelude_fn, &[host.get(), host_async_fn.get(), settings.get(), helpers.get()])).map_err(fail)?;
        if !JSValueIsObject(ctx, api) { return Err(fail("prelude did not return its API".into())); }
        JSValueProtect(ctx, api);
        let api = api as JSObjectRef;
        with(|e| e.api = api);
        let on_rejection = JSValueToObject(ctx, get_property(ctx, api, "onUnhandledRejection"), null_mut());
        if !JSObjectIsFunction(ctx, on_rejection) { return Err(fail("prelude rejection callback missing".into())); }
        JSGlobalContextSetUnhandledRejectionCallback(ctx, on_rejection, null_mut());
    }
    Ok(())
}

fn teardown() {
    let cleanups = ENGINE.with(|e| e.borrow().as_ref().map(|e| e.cleanups.keys().copied().collect::<Vec<_>>()).unwrap_or_default());
    for id in cleanups { finish_cleanup(id, CleanupStatus::Cancelled, Some("realm closed before cleanup completed".into())); }
    let Some(mut engine) = ENGINE.with(|e| e.borrow_mut().take()) else { return };
    engine.http.abort_all();
    for job in engine.pending_jobs.values().chain(engine.timer_jobs.values()) { job.abort(); }
    engine.continuations.clear();
    engine.cancel_handlers.clear();
    for call in engine.calls.values() { call.scope.cancel.stop(); }
    unsafe {
        for (_, (resolve, _)) in engine.pending_ops {
            JSValueUnprotect(engine.ctx, resolve as JSValueRef);
        }
        if !engine.api.is_null() {
            JSValueUnprotect(engine.ctx, engine.api as JSValueRef);
        }
        JSGlobalContextRelease(engine.ctx);
        JSClassRelease(engine.binding_class);
        JSContextGroupRelease(engine.group);
    }
}

/// After each message: handle a terminated turn, wake idle waiters.
fn after_turn() {
    if let Some((kind, reason, scope)) = TERMINATED.with(|t| t.borrow_mut().take()) {
        let name = with(|e| { e.stderr.push_str(&format!("ply: {}: script terminated: {reason}\n", e.name)); e.name.clone() });
        // Clear JSC's pending termination before rejecting Promise chains.
        let ctx = with(|e| e.ctx);
        let _ = enter(None, || unsafe { eval(ctx, "void 0", "ply:recover") });
        TERMINATED.with(|t| t.borrow_mut().take());
        if let Some(scope) = scope { fail_call(scope.id, kind, format!("{name}: script terminated: {reason}")); }
    }
    let waiters = with(|e| if e.is_idle() { std::mem::take(&mut e.idle_waiters) } else { Vec::new() });
    for w in waiters {
        let _ = w.send(());
    }
}

// Native bound functions carry an unforgeable consumer scope. Their JS
// callback is an ordinary readonly property so JSC traces cycles correctly;
// only the Arc scope lives in private data (finalizers may run off-thread).
unsafe extern "C" fn drop_binding(object: JSObjectRef) {
    let data = unsafe { JSObjectGetPrivate(object) } as *mut Binding;
    if !data.is_null() { drop(unsafe { Box::from_raw(data) }); }
}
unsafe fn binding_class() -> JSClassRef {
    let definition = JSClassDefinition {
        version: 0, attributes: 0, class_name: c"PlyContinuation".as_ptr(), parent: null_mut(),
        static_values: std::ptr::null(), static_functions: std::ptr::null(), initialize: std::ptr::null(),
        finalize: Some(drop_binding), has_property: std::ptr::null(), get_property: std::ptr::null(),
        set_property: std::ptr::null(), delete_property: std::ptr::null(), get_property_names: std::ptr::null(),
        call: Some(enqueue_continuation), construct: std::ptr::null(), has_instance: std::ptr::null(), convert: std::ptr::null(),
    };
    unsafe { JSClassCreate(&definition) }
}
unsafe extern "C" fn register_promise_cancel(ctx: JSContextRef, _f: JSObjectRef, _this: JSObjectRef, argc: usize, argv: *const JSValueRef, _exc: *mut JSValueRef) -> JSValueRef {
    unsafe {
        if argc > 0 {
            let reject = *argv;
            if !JSValueIsObject(ctx, reject) || !JSObjectIsFunction(ctx, JSValueToObject(ctx, reject, null_mut())) { return JSValueMakeNumber(ctx, 0.0); }
            let scope = with(|e| e.current.clone());
            if let Some(scope) = scope {
                let active = with(|e| e.calls.contains_key(&scope.id));
                if active {
                    let root = RootedValue::new(ctx, reject);
                    let id = with(|e| { let id = e.next(); e.cancel_handlers.entry(scope.id).or_default().insert(id, root); id });
                    return JSValueMakeNumber(ctx, id as f64);
                }
            }
        }
        JSValueMakeNumber(ctx, 0.0)
    }
}
unsafe extern "C" fn unregister_promise_cancel(ctx: JSContextRef, _f: JSObjectRef, _this: JSObjectRef, argc: usize, argv: *const JSValueRef, _exc: *mut JSValueRef) -> JSValueRef {
    unsafe {
        if argc > 0 {
            let id = JSValueToNumber(ctx, *argv, null_mut()) as u64;
            with(|e| { for handlers in e.cancel_handlers.values_mut() { handlers.remove(&id); } });
        }
        JSValueMakeUndefined(ctx)
    }
}
unsafe extern "C" fn bind_continuation(ctx: JSContextRef, _f: JSObjectRef, _this: JSObjectRef, argc: usize, argv: *const JSValueRef, _exc: *mut JSValueRef) -> JSValueRef {
    unsafe {
        if argc == 0 || !JSValueIsObject(ctx, *argv) { return JSValueMakeUndefined(ctx); }
        let callback = JSValueToObject(ctx, *argv, null_mut());
        if !JSObjectIsFunction(ctx, callback) { return JSValueMakeUndefined(ctx); }
        let (class, scope) = with(|e| (e.binding_class, e.current.clone()));
        let data = Box::into_raw(Box::new(Binding { scope, fatal: argc > 1 && JSValueToBoolean(ctx, *argv.add(1)) }));
        let bound = RootedValue::new(ctx, JSObjectMake(ctx, class, data.cast()) as JSValueRef);
        JSObjectSetProperty(ctx, bound.get() as JSObjectRef, JsStr::new("callback").0, callback as JSValueRef, 2 | 4 | 8, null_mut());
        bound.get()
    }
}
unsafe extern "C" fn enqueue_continuation(ctx: JSContextRef, function: JSObjectRef, receiver: JSObjectRef, argc: usize, argv: *const JSValueRef, _exc: *mut JSValueRef) -> JSValueRef {
    unsafe {
        let private = JSObjectGetPrivate(function) as *const Binding;
        if private.is_null() { return JSValueMakeUndefined(ctx); }
        let binding = &*private;
        if binding.scope.as_ref().is_some_and(|s| s.cancel.is_stopped()) { return JSValueMakeUndefined(ctx); }
        let callback = RootedValue::new(ctx, get_property(ctx, function, "callback"));
        let receiver = RootedValue::new(ctx, receiver as JSValueRef);
        let args = (0..argc).map(|i| RootedValue::new(ctx, *argv.add(i))).collect();
        let work = Continuation { scope: binding.scope.clone(), fatal: binding.fatal, callback, receiver, args };
        with(|e| { let id = e.next(); e.continuations.insert(id, work); let _ = e.tx.send(Msg::Continuation(id)); });
        JSValueMakeUndefined(ctx)
    }
}
fn run_continuation(id: u64) {
    let Some((ctx, work)) = with(|e| e.continuations.remove(&id).map(|work| (e.ctx, work))) else { return };
    if work.scope.as_ref().is_some_and(|s| s.cancel.is_stopped()) { return; }
    unsafe {
        let args: Vec<_> = work.args.iter().map(RootedValue::get).collect();
        let mut exception = std::ptr::null();
        let value = enter(work.scope.clone(), || JSObjectCallAsFunction(ctx, work.callback.get() as JSObjectRef, work.receiver.get() as JSObjectRef, args.len(), args.as_ptr(), &mut exception));
        if TERMINATED.with(|t| t.borrow().is_some()) { return; }
        if !exception.is_null() {
            let message = describe_exception(ctx, exception);
            if work.fatal { if let Some(scope) = &work.scope { fail_call(scope.id, FailureKind::Failed, message.clone()); } }
            with(|e| e.stderr.push_str(&format!("Uncaught {message}\n")));
        }
        let _ = value;
    }
}
fn fail_call(id: u64, kind: FailureKind, message: String) {
    fail_call_error(id, JsError { kind, message, ..JsError::default() });
}
fn fail_call_error(id: u64, mut error: JsError) {
    let Some(active) = with(|e| e.calls.remove(&id)) else { return };
    active.scope.cancel.is_stopped();
    let timed_out = active.scope.cancel.timed_out.load(Ordering::Relaxed);
    if timed_out { error.message = "onDispose exceeded its two-second deadline".into(); }
    let message = error.message.clone();
    retire_scope(&active.scope, &message);
    let status = if timed_out { CleanupStatus::TimedOut } else if error.kind == FailureKind::Cancelled { CleanupStatus::Cancelled } else { CleanupStatus::Failed };
    finish_cleanup(id, status, Some(error.message.clone()));
    if !active.disposals.is_empty() { start_disposal(active.disposals, active.scope.fiber.clone()); }
    let _ = active.reply.send(Err(error));
}

// Stop every resource owned by this invocation, including work a cleanup
// callback started without awaiting. Ordinary successful calls retain their
// Node-style timers; disposal invocations always retire their whole scope.
fn retire_scope(scope: &Invocation, message: &str) {
    scope.cancel.stop();
    let id = scope.id;
    let ctx = with(|e| e.ctx);
    let owned = |scope: &Scope| scope.as_ref().is_some_and(|scope| scope.id == id);
    let (resolves, timers, rejects) = with(|e| {
        if e.terminal_owner == Some(id) { e.terminal.release(); e.terminal_owner = None; }
        let op_ids: Vec<_> = e.pending_ops.iter().filter(|(_, (_, scope))| owned(scope)).map(|(id, _)| *id).collect();
        let mut resolves = Vec::new();
        for op in op_ids {
            if let Some(job) = e.pending_jobs.remove(&op) { job.abort(); }
            if let Some((resolve, _)) = e.pending_ops.remove(&op) { resolves.push(resolve); }
        }
        let timer_ids: Vec<_> = e.timers.iter().filter(|(_, (_, scope))| owned(scope)).map(|(id, _)| *id).collect();
        for timer in &timer_ids { e.timers.remove(timer); if let Some(job) = e.timer_jobs.remove(timer) { job.abort(); } }
        e.continuations.retain(|_, work| !owned(&work.scope));
        let http_ids: Vec<_> = e.http_owners.iter().filter(|(_, scope)| owned(scope)).map(|(id, _)| *id).collect();
        for http in http_ids { e.http_owners.remove(&http); e.http.abort(http); }
        (resolves, timer_ids, e.cancel_handlers.remove(&id).unwrap_or_default())
    });
    unsafe {
        for resolve in resolves { JSValueUnprotect(ctx, resolve as JSValueRef); }
        let error = RootedValue::new(ctx, string_value(ctx, message));
        for reject in rejects.into_values() { let _ = enter(None, || call(ctx, reject.get() as JSObjectRef, &[error.get()])); }
        if !timers.is_empty() {
            let api = with(|e| e.api);
            let discard = get_property(ctx, api, "discardTimers");
            let ids = RootedValue::new(ctx, json_value(ctx, &json!(timers).to_string()));
            let _ = enter(None, || call(ctx, discard as JSObjectRef, &[ids.get()]));
        }
    }
}

// Cleanup has its own bounded invocation, so an interrupted generator's
// finally block is not required to release plugin-owned session state.
fn cleanup_event(id: u64, owner: Option<String>, status: CleanupStatus, error: Option<String>) {
    let (realm, bridge) = with(|e| {
        if matches!(status, CleanupStatus::Failed | CleanupStatus::TimedOut | CleanupStatus::Cancelled) {
            e.stderr.push_str(&format!("ply: onDispose {id}: {status:?}: {}\n", error.as_deref().unwrap_or("")));
        }
        (e.name.clone(), e.bridge.clone())
    });
    let event = FiberEvent::Cleanup { at_ms: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
        id, realm, owner, status, error };
    if let Some(bridge) = bridge {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| bridge.on_event(event))).is_err() {
            with(|e| e.stderr.push_str("ply: cleanup event observer panicked\n"));
        }
    }
}
fn finish_cleanup(id: u64, status: CleanupStatus, error: Option<String>) {
    let Some(cleanup) = with(|e| e.cleanups.remove(&id)) else { return };
    cleanup.deadline.abort();
    // A timeout can happen before dispose has removed its JS callbacks.
    // Defer bookkeeping until JSC's termination bit has been cleared.
    with(|e| { let _ = e.tx.send(Msg::DiscardDisposals(cleanup.watches)); });
    cleanup_event(id, cleanup.owner, status, error);
}
fn start_disposal(ids: Vec<u64>, owner: Option<String>) {
    // Across all realms these private call ids occupy their own JS-safe range.
    static NEXT_CLEANUP: AtomicU64 = AtomicU64::new(1_u64 << 52);
    let id = NEXT_CLEANUP.fetch_add(1, Ordering::Relaxed);
    let (tx, handle) = with(|e| (e.tx.clone(), e.handle.clone()));
    let deadline_at = Instant::now() + Duration::from_secs(2);
    let cancel = Arc::new(CallCancellation::with_deadline(deadline_at));
    let deadline_cancel = cancel.clone();
    // Arm before calling JS: a callback can block before its first await.
    let deadline = handle.spawn(async move {
        tokio::time::sleep_until(deadline_at.into()).await;
        // The flag/watch interrupts CPU and synchronous waits without
        // requiring the busy realm to consume this message first.
        deadline_cancel.expire();
        let _ = tx.send(Msg::CancelCall(id));
    });
    with(|e| { e.cleanups.insert(id, CleanupRun { owner: owner.clone(), watches: ids.clone(), deadline: deadline.abort_handle() }); });
    cleanup_event(id, owner, CleanupStatus::Started, None);
    let (reply, _result) = oneshot::channel();
    // Enter on the next engine turn, after the failed invocation has left
    // the JS stack. Nested JS entry would drain lazy thenable jobs under
    // the failed caller's scope instead of this cleanup's scope.
    with(|e| { let _ = e.tx.send(Msg::Api { id, method: "dispose", args: vec![json!(ids).to_string()], fiber: None, cancel, reply }); });
}

fn start_call(id: u64, method: &'static str, args: Vec<String>, fiber: Option<String>, cancel: Arc<CallCancellation>, reply: Reply<Result<Value, JsError>>) {
    let (ctx, api, scope) = with(|e| {
        let origin = if method == "method" {
            args.get(3).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null)
        } else {
            json!({ "plugin": e.name, "loading": method == "load", "host": false, "client": format!("{:?}", e.id),
                "fiber": fiber, "ancestors": [] })
        };
        let scope = Arc::new(Invocation { id, fiber, cancel, origin });
        e.calls.insert(id, ActiveCall { reply, scope: scope.clone(), disposals: Vec::new(), method });
        (e.ctx, e.api, scope)
    });
    if scope.cancel.is_stopped() { fail_call(id, FailureKind::Cancelled, "call cancelled before execution".into()); return; }
    unsafe {
        let f = RootedValue::new(ctx, JSValueToObject(ctx, get_property(ctx, api, method), null_mut()) as JSValueRef);
        // A Vec's storage is on the Rust heap, outside JSC's root scan. Root
        // each argument immediately: constructing the next one can run GC.
        let mut roots = Vec::with_capacity(args.len() + 1);
        roots.push(RootedValue::new(ctx, JSValueMakeNumber(ctx, id as f64)));
        for arg in &args {
            roots.push(RootedValue::new(ctx, string_value(ctx, arg)));
        }
        let argv: Vec<_> = roots.iter().map(RootedValue::get).collect();
        if let Err(message) = enter(Some(scope), || call(ctx, f.get() as JSObjectRef, &argv)) {
            if TERMINATED.with(|t| t.borrow().is_none()) { fail_call(id, FailureKind::Failed, message); }
        }
    }
}

fn complete(id: u64, result: OpResult) {
    let Some((ctx, resolve, fiber)) = with(|e| {
        e.pending_jobs.remove(&id);
        e.pending_ops.remove(&id).map(|(r, f)| (e.ctx, r, f))
    }) else { return };
    unsafe {
        let value = RootedValue::new(ctx, result_value(ctx, result));
        let _ = enter(fiber, || call(ctx, resolve, &[value.get()]));
        JSValueUnprotect(ctx, resolve as JSValueRef);
    }
}

fn fire_timer(id: u64) {
    let target = with(|e| {
        e.timer_jobs.remove(&id);
        let (repeat, fiber) = e.timers.get(&id).cloned()?;
        match repeat {
            Some(ms) => arm_timer(e, id, ms),
            None => {
                e.timers.remove(&id);
            }
        }
        Some((e.ctx, e.api, fiber))
    });
    let Some((ctx, api, fiber)) = target else { return };
    unsafe {
        let f = JSValueToObject(ctx, get_property(ctx, api, "fire"), null_mut());
        let _ = enter(fiber, || call(ctx, f, &[JSValueMakeNumber(ctx, id as f64)]));
    }
}

fn arm_timer(e: &mut Engine, id: u64, ms: f64) {
    let tx = e.tx.clone();
    let task = e.handle.spawn(async move {
        tokio::time::sleep(Duration::from_secs_f64(ms / 1000.0)).await;
        let _ = tx.send(Msg::Timer(id));
    });
    e.timer_jobs.insert(id, task.abort_handle());
}

// ------------------------------------------------------------ host calls

unsafe fn read_call(ctx: JSContextRef, argc: usize, argv: *const JSValueRef) -> (String, Value, Option<Vec<u8>>) {
    unsafe {
        let args = std::slice::from_raw_parts(argv, argc);
        let op = args.first().map(|v| to_rust_string(ctx, *v)).unwrap_or_default();
        let json = args.get(1).map(|v| to_rust_string(ctx, *v)).unwrap_or_default();
        let bytes = args.get(2).and_then(|v| value_bytes(ctx, *v));
        (op, serde_json::from_str(&json).unwrap_or(Value::Null), bytes)
    }
}

unsafe fn result_value(ctx: JSContextRef, result: OpResult) -> JSValueRef {
    unsafe {
        match result {
            OpResult::Json(v) => json_value(ctx, &v.to_string()),
            OpResult::Bytes(b) => bytes_value(ctx, &b),
            OpResult::Error(e) => json_value(ctx, &json!({ "__error": e }).to_string()),
        }
    }
}

unsafe extern "C" fn host_sync(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    argv: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let (op, args, bytes) = read_call(ctx, argc, argv);
        // Permit finalizers and error reporting to settle an interrupted
        // call; rejecting console/settle here could recursively reject the
        // unhandled-rejection reporter while a realm is shutting down.
        let cleanup = matches!(op.as_str(), "settle" | "console.write" | "timer.clear" | "http.abort" | "terminal.close" | "kernel.directory" | "clock.now");
        let result = if !cleanup && with(|e| *e.cancelled.borrow() || e.current.as_ref().is_some_and(|scope| scope.cancel.is_stopped())) {
            OpResult::error("ERR_PLY_CANCELLED", "fiber or realm cancelled")
        } else { sync_op(&op, args, bytes) };
        result_value(ctx, result)
    }
}

unsafe extern "C" fn host_async(
    ctx: JSContextRef,
    _f: JSObjectRef,
    _this: JSObjectRef,
    argc: usize,
    argv: *const JSValueRef,
    _exception: *mut JSValueRef,
) -> JSValueRef {
    unsafe {
        let (op, args, bytes) = read_call(ctx, argc, argv);
        let mut resolve: JSObjectRef = null_mut();
        let mut reject: JSObjectRef = null_mut();
        let promise = JSObjectMakeDeferredPromise(ctx, &mut resolve, &mut reject, null_mut());
        JSValueProtect(ctx, resolve as JSValueRef);
        // The op belongs to the fiber of the entry it was started in.
        let (id, tx, handle, fs, fiber, bridge, realm, realm_id, loading) = with(|e| {
            let id = e.next();
            e.pending_ops.insert(id, (resolve, e.current.clone()));
            let loading = e.current.as_ref().and_then(|scope| e.calls.get(&scope.id)).is_some_and(|call| call.method == "load");
            (id, e.tx.clone(), e.handle.clone(), e.fs.clone(), e.current.clone(), e.bridge.clone(), e.name.clone(), e.id, loading)
        });
        let completion = HostCompletion { tx, id, completed: false };
        let finish = move |result: OpResult| completion.finish(result);
        let job = if with(|e| *e.cancelled.borrow() || e.current.as_ref().is_some_and(|scope| scope.cancel.is_stopped())) {
            finish(closed_realm());
            None
        } else if op.starts_with("fs.") {
            Some(handle.spawn(async move { finish(fs_op(fs, &op, args, bytes).await) }))
        } else if op == "sh.exec" {
            Some(handle.spawn(async move { finish(crate::sh::execute(fs, args).await) }))
        } else if op == "proc.exec" {
            match exec_request(&fs, &args, bytes) {
                Ok(spec) => {
                    Some(handle.spawn(async move { finish(run_exec(fs, spec).await) }))
                }
                Err(invalid) => { finish(invalid); None }
            }
        } else if op == "terminal.read" {
            let terminal = with(|e| e.terminal.clone());
            Some(handle.spawn(async move { finish(terminal.read(args).await) }))
        } else if op.starts_with("checkpoint.") {
            let checkpoints = with(|e| e.checkpoints.clone());
            Some(handle.spawn_blocking(move || finish(checkpoints.call(&op, args))))
        } else if op.starts_with("http.") {
            let http = with(|e| e.http.clone());
            Some(handle.spawn(async move { finish(http.call(&op, args).await) }))
        } else if op.starts_with("kernel.") {
            match bridge {
                Some(bridge) => {
                    Some(handle.spawn(async move {
                        let result = match bridge.call(Caller { realm, realm_id, loading, fiber: fiber.as_ref().and_then(|scope| scope.fiber.clone()) }, &op, args).await {
                            Ok(value) => OpResult::Json(value),
                            Err(message) => OpResult::error("ERR_PLY_FIBER", message),
                        };
                        finish(result)
                    }))
                }
                None => { finish(OpResult::error("ERR_PLY_NO_KERNEL", "other plugins are only reachable under the ply kernel")); None }
            }
        } else {
            finish(OpResult::error("ERR_PLY", format!("{op} has no async form")));
            None
        };
        if let Some(job) = job { with(|e| { e.pending_jobs.insert(id, job.abort_handle()); }); }
        promise as JSValueRef
    }
}

fn invalid(what: &str) -> OpResult {
    OpResult::error("ERR_PLY_INVALID_CALL", format!("invalid {what} in host call"))
}

fn closed_realm() -> OpResult { OpResult::error("ERR_PLY_REALM_CLOSED", "the realm is closing") }

fn blocking<T>(future: impl std::future::Future<Output = T>) -> Result<T, OpResult> {
    let (handle, mut cancelled, scope) = with(|e| (e.handle.clone(), e.cancelled.clone(), e.current.clone()));
    handle.block_on(async move {
        let stop_scope = async {
            match scope {
                Some(scope) => { let _ = scope.cancel.signal.subscribe().wait_for(|stopped| *stopped).await; }
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            biased;
            _ = cancelled.wait_for(|closed| *closed) => Err(closed_realm()),
            _ = stop_scope => Err(OpResult::error("ERR_PLY_CANCELLED", "fiber cancelled by host")),
            result = future => Ok(result),
        }
    })
}

fn sync_op(op: &str, args: Value, bytes: Option<Vec<u8>>) -> OpResult {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match op {
        "invocation.info" => with(|e| OpResult::Json(e.current.as_ref().map(|s| s.origin.clone()).unwrap_or(Value::Null))),
        "markdown.parse" => crate::markdown::parse(args),
        "skills.parse" => crate::skills::parse(args),
        "markdown.source" => OpResult::Json(json!(include_str!("markdown.js"))),
        _ if op.starts_with("terminal.") => with(|e| {
            if op == "terminal.open" && e.current.is_none() { return invalid("terminal owner"); }
            let result = e.terminal.call(op, args);
            if matches!(result, OpResult::Json(_)) {
                if op == "terminal.open" { e.terminal_owner = e.current.as_ref().map(|s| s.id); }
                if op == "terminal.close" { e.terminal_owner = None; }
            }
            result
        }),
        "models.source" => {
            static COMPILED: OnceLock<Result<String, String>> = OnceLock::new();
            match COMPILED.get_or_init(|| lower_async(PI_AI, Path::new("ply:pi-ai"))) {
                Ok(code) => OpResult::Json(json!(code)), Err(error) => OpResult::error("ERR_SYNTAX", error),
            }
        },
        "fiber.watch" => with(|e| {
            let Some(scope) = e.current.clone() else { return invalid("cleanup owner"); };
            if e.cleanups.contains_key(&scope.id) { return invalid("cleanup cannot register nested cleanup"); }
            let id = e.next();
            let Some(call) = e.calls.get_mut(&scope.id) else { return invalid("cleanup owner"); };
            call.disposals.push(id);
            OpResult::Json(json!(id))
        }),
        "fiber.unwatch" => with(|e| {
            let Some(scope) = e.current.clone() else { return invalid("cleanup owner"); };
            let id = args.get("id").and_then(Value::as_u64).unwrap_or(0);
            if let Some(call) = e.calls.get_mut(&scope.id) { call.disposals.retain(|watch| *watch != id); }
            OpResult::Json(Value::Null)
        }),
        "fiber.unhandled" => {
            if let Some(scope) = with(|e| e.current.clone()) { fail_call(scope.id, FailureKind::Failed, s("error")); }
            OpResult::Json(Value::Null)
        }
        "models.credentials" => crate::http::credentials(&args),
        "http.auth_configured" => crate::http::auth_configured(&args),
        "http.open" => with(|e| {
            let result = e.http.open(args, bytes);
            if let OpResult::Json(value) = &result { if let Some(id) = value.as_u64() { e.http_owners.insert(id, e.current.clone()); } }
            result
        }),
        "http.abort" => {
            if let Some(id) = args.get("id").and_then(Value::as_u64) {
                with(|e| { e.http_owners.remove(&id); e.http.abort(id); });
            }
            OpResult::Json(Value::Null)
        }
        _ if op.starts_with("fs.") => {
            let fs = with(|e| e.fs.clone());
            blocking(fs_op(fs, op, args, bytes)).unwrap_or_else(|error| error)
        }
        "proc.exec" => {
            let fs = with(|e| e.fs.clone());
            match exec_request(&fs, &args, bytes) {
                Ok(spec) => blocking(run_exec(fs, spec)).unwrap_or_else(|error| error),
                Err(invalid) => invalid,
            }
        }
        "console.write" => {
            let text = s("text");
            let fd = args.get("fd").and_then(Value::as_u64).unwrap_or(1);
            with(|e| if fd == 2 { e.stderr.push_str(&text) } else { e.stdout.push_str(&text) });
            OpResult::Json(Value::Null)
        }
        "timer.set" => {
            let ms = args.get("ms").and_then(Value::as_f64).filter(|m| m.is_finite()).unwrap_or(0.0).max(0.0);
            let repeat = args.get("repeat").and_then(Value::as_bool).unwrap_or(false);
            let id = with(|e| {
                let id = e.next();
                e.timers.insert(id, (repeat.then_some(ms.max(1.0)), e.current.clone()));
                arm_timer(e, id, ms);
                id
            });
            OpResult::Json(json!(id))
        }
        "timer.clear" => {
            if let Some(id) = args.get("id").and_then(Value::as_u64) {
                with(|e| {
                    e.timers.remove(&id);
                    if let Some(job) = e.timer_jobs.remove(&id) { job.abort(); }
                });
            }
            OpResult::Json(Value::Null)
        }
        "kernel.directory" => OpResult::Json(with(|e| e.directory.clone())),
        "clock.now" => OpResult::Json(json!(with(|e| e.started.elapsed().as_secs_f64() * 1000.0))),
        "process.cwd" => OpResult::Json(json!(with(|e| e.fs.cwd().to_string()))),
        "process.chdir" => {
            let dir = s("dir");
            let fs = with(|e| e.fs.clone());
            let target = fs.resolve(&dir);
            let stat = match blocking(fs.stat(&dir)) { Ok(stat) => stat, Err(error) => return error };
            match stat {
                Ok(entry) if entry.kind == EntryKind::Dir => {
                    with(|e| e.fs = fs.with_cwd(&target));
                    OpResult::Json(Value::Null)
                }
                Ok(_) => node_error(SysError::new(Errno::ENOTDIR, ""), "chdir", &dir, None),
                Err(err) => node_error(err, "chdir", &dir, None),
            }
        }
        "process.exit" => {
            let code = args.get("code").and_then(Value::as_i64).unwrap_or(0) as i32;
            with(|e| e.exit_code = Some(code));
            OpResult::Json(Value::Null)
        }
        "path.normalize" => OpResult::Json(json!(lpath::normalize(&s("path")))),
        "path.resolve" => OpResult::Json(json!(lpath::resolve(&s("cwd"), &s("path")))),
        "path.dirname" => OpResult::Json(json!(lpath::dirname(&s("path")))),
        "path.basename" => OpResult::Json(json!(lpath::basename(&s("path")))),
        "path.extname" => OpResult::Json(json!(lpath::extname(&s("path")))),
        "module.resolve" => resolve_module(&s("spec"), args.get("from")),
        "module.load" => load_module(&s("id")),
        "settle" => {
            let id = args.get("id").and_then(Value::as_u64).unwrap_or(0);
            let owner = with(|e| e.current.as_ref().map(|scope| scope.id));
            if owner != Some(id) { return invalid("settle owner"); }
            if with(|e| e.current.as_ref().is_some_and(|scope| scope.cancel.is_stopped())) {
                fail_call(id, FailureKind::Cancelled, "call cancelled before completion".into());
                return OpResult::Json(Value::Null);
            }
            let Some(reply) = with(|e| e.calls.remove(&id)) else {
                let expected = with(|e| e.current.as_ref().is_some_and(|scope| scope.id == id && scope.cancel.is_stopped()));
                return if expected { OpResult::Json(Value::Null) } else { invalid("settle id") };
            };
            let result = if args.get("ok").and_then(Value::as_bool) == Some(true) {
                Ok(args.get("value").cloned().unwrap_or(Value::Null))
            } else {
                Err(JsError {
                    kind: FailureKind::Failed,
                    message: s("error"),
                    exit_code: args.get("exit").and_then(Value::as_i64).map(|c| c as i32),
                    stack: args.get("stack").and_then(Value::as_str).filter(|st| !st.is_empty()).map(str::to_string),
                })
            };
            if result.is_err() {
                // Put it back so the common cleanup path retires this scope.
                let error = result.unwrap_err();
                with(|e| { e.calls.insert(id, reply); });
                fail_call_error(id, error);
            } else {
                with(|e| { if e.terminal_owner == Some(id) { e.terminal.release(); e.terminal_owner = None; } });
                if matches!(reply.method, "event" | "command") || with(|e| e.cleanups.contains_key(&id)) {
                    retire_scope(&reply.scope, "callback completed; pending callback work cancelled");
                }
                finish_cleanup(id, CleanupStatus::Done, None);
                with(|e| { e.cancel_handlers.remove(&id); });
                if !reply.disposals.is_empty() {
                    unsafe {
                        let (ctx, api) = with(|e| (e.ctx, e.api));
                        let callback = get_property(ctx, api, "discardDisposals");
                        let ids = RootedValue::new(ctx, json_value(ctx, &json!(reply.disposals).to_string()));
                        let _ = call(ctx, callback as JSObjectRef, &[ids.get()]);
                    }
                }
                let _ = reply.reply.send(result);
            }
            OpResult::Json(Value::Null)
        }
        other => OpResult::error("ERR_PLY", format!("unknown host op {other}")),
    }
}

// -------------------------------------------------------------- modules

fn resolve_module(spec: &str, from: Option<&Value>) -> OpResult {
    if matches!(spec, "ply:models" | "ply:checkpoints" | "ply:lifecycle" | "ply:sh" | "ply:events" | "ply:http" | "ply:terminal" | "ply:plugins" | "ply:commands" | "ply:markdown" | "ply:skills") || spec.strip_prefix("ply:async/helpers/").is_some_and(|name|
        ["asyncToGenerator", "wrapAsyncGenerator", "awaitAsyncGenerator", "asyncGeneratorDelegate", "asyncIterator"].contains(&name)) {
        return OpResult::Json(json!({ "builtin": spec }));
    }
    let bare = spec.strip_prefix("node:").unwrap_or(spec);
    if BUILTINS.contains(&bare) {
        return OpResult::Json(json!({ "builtin": bare }));
    }
    if spec.starts_with("node:") {
        return OpResult::error("ERR_UNKNOWN_BUILTIN_MODULE", format!("No such built-in module: {spec}"));
    }
    if !(spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/')) {
        return OpResult::error(
            "ERR_MODULE_NOT_FOUND",
            format!("Cannot find package '{spec}' (importing packages is not supported yet)"),
        );
    }
    let found = with(|e| {
        let base = match from {
            None | Some(Value::Null) => e.root.clone(),
            Some(Value::String(f)) if e.loaded.contains(f) => lpath::dirname(f),
            Some(_) => return Err(invalid("importer")),
        };
        Ok((e.code.clone(), e.root.clone(), base, e.handle.clone()))
    });
    let (code, root, base, handle) = match found {
        Ok(found) => found,
        Err(invalid) => return invalid,
    };
    // Decide containment on the path alone, before touching any storage, so
    // a probe outside the root learns nothing about what exists there.
    let target = lpath::resolve(&base, spec);
    if !under(&target, &root) {
        return OpResult::error("ERR_MODULE_OUTSIDE_ROOT", format!("'{spec}' resolves outside the plugin root {root}"));
    }
    let mut candidates = vec![target.clone()];
    candidates.extend(["ts", "tsx", "mts", "js", "mjs"].iter().map(|ext| format!("{target}.{ext}")));
    candidates.extend(["index.ts", "index.js"].iter().map(|index| lpath::join(&target, index)));
    for candidate in candidates {
        if handle.block_on(code.is_file(&candidate)) {
            with(|e| e.resolved.insert(candidate.clone()));
            return OpResult::Json(json!({ "id": candidate, "dir": lpath::dirname(&candidate) }));
        }
    }
    let shown_from = from.and_then(Value::as_str).unwrap_or("<entry>");
    OpResult::error("ERR_MODULE_NOT_FOUND", format!("Cannot find module '{spec}' imported from {shown_from}"))
}

fn load_module(id: &str) -> OpResult {
    let found = with(|e| e.resolved.contains(id).then(|| (e.code.clone(), e.handle.clone())));
    let Some((code, handle)) = found else { return invalid("module id") };
    let source = match handle.block_on(code.read(id)) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => return OpResult::error("ERR_MODULE_NOT_FOUND", format!("{id}: {e}")),
    };
    match transpile(&source, Path::new(id)) {
        Ok(js) => {
            with(|e| e.loaded.insert(id.to_string()));
            OpResult::Json(json!(js))
        }
        Err(e) => OpResult::error("ERR_SYNTAX", e),
    }
}

/// `path` equals `root` or lies below it, compared by whole components.
fn under(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches('/');
    root.is_empty() || path == root || path.starts_with(&format!("{root}/"))
}

// ------------------------------------------------------------------- fs

/// A node-shaped error object: `ENOENT: no such file or directory, open 'x'`.
fn node_error(err: SysError, syscall: &str, path: &str, dest: Option<&str>) -> OpResult {
    let code = err.errno.code();
    let mut message = format!("{code}: {}, {syscall} '{path}'", err.errno.uv_message());
    if let Some(d) = dest {
        message.push_str(&format!(" -> '{d}'"));
    }
    OpResult::Error(json!({
        "code": code,
        "errno": -err.errno.number(),
        "syscall": syscall,
        "path": path,
        "dest": dest,
        "message": message,
        "detail": err.detail,
    }))
}

async fn fs_op(fs: Fs, op: &str, args: Value, bytes: Option<Vec<u8>>) -> OpResult {
    let path = args.get("path").and_then(Value::as_str).unwrap_or("").to_string();
    let dest = args.get("dest").and_then(Value::as_str).map(str::to_string);
    let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    let done = |r: Result<(), SysError>, syscall: &str| match r {
        Ok(()) => OpResult::Json(Value::Null),
        Err(e) => node_error(e, syscall, &path, dest.as_deref()),
    };
    match op {
        "fs.readFile" => match fs.read(&path).await {
            Ok(data) => match args.get("encoding").and_then(Value::as_str) {
                Some(enc) if enc.eq_ignore_ascii_case("utf8") || enc.eq_ignore_ascii_case("utf-8") => {
                    OpResult::Json(json!(String::from_utf8_lossy(&data)))
                }
                _ => OpResult::Bytes(data),
            },
            Err(e) => node_error(e, "open", &path, None),
        },
        "fs.writeFile" => {
            let data = bytes.unwrap_or_default();
            let r = if flag("append") { fs.append(&path, &data).await } else { fs.write(&path, &data).await };
            done(r, "open")
        }
        "fs.stat" => match fs.stat(&path).await {
            Ok(e) => OpResult::Json(json!({
                "isDirectory": e.kind == EntryKind::Dir,
                "size": e.size,
                "mtimeMs": ply_libc::time::unix_seconds(e.modified) as f64 * 1000.0,
            })),
            Err(e) => node_error(e, "stat", &path, None),
        },
        "fs.readdir" => match fs.read_dir(&path).await {
            Ok(items) => OpResult::Json(Value::Array(
                items.into_iter().map(|i| json!({ "name": i.name, "isDirectory": i.entry.kind == EntryKind::Dir })).collect(),
            )),
            Err(e) => node_error(e, "scandir", &path, None),
        },
        "fs.mkdir" => done(fs.mkdir(&path, flag("recursive")).await, "mkdir"),
        "fs.rm" => {
            let entry = match fs.stat(&path).await {
                Ok(e) => e,
                Err(e) if e.errno == Errno::ENOENT && flag("force") => return OpResult::Json(Value::Null),
                Err(e) => return node_error(e, if flag("dirOnly") { "rmdir" } else { "lstat" }, &path, None),
            };
            let r = match (entry.kind, flag("recursive"), flag("dirOnly")) {
                (EntryKind::Dir, true, _) => fs.remove_all(&path).await,
                (EntryKind::Dir, false, true) => fs.remove_dir(&path).await,
                (EntryKind::Dir, false, false) => Err(SysError::new(Errno::EISDIR, "")),
                (EntryKind::File, _, true) => Err(SysError::new(Errno::ENOTDIR, "")),
                (EntryKind::File, _, false) => fs.remove_file(&path).await,
            };
            done(r, if flag("dirOnly") { "rmdir" } else { "rm" })
        }
        "fs.unlink" => done(fs.remove_file(&path).await, "unlink"),
        "fs.rename" => done(fs.rename(&path, dest.as_deref().unwrap_or("")).await, "rename"),
        "fs.copyFile" => done(fs.copy_file(&path, dest.as_deref().unwrap_or("")).await, "copyfile"),
        "fs.access" => match fs.stat(&path).await {
            Ok(_) => OpResult::Json(Value::Null),
            Err(e) => node_error(e, "access", &path, None),
        },
        "fs.realpath" => match fs.stat(&path).await {
            Ok(_) => OpResult::Json(json!(fs.resolve(&path))),
            Err(e) => node_error(e, "realpath", &path, None),
        },
        "fs.exists" => OpResult::Json(json!(fs.exists(&path).await)),
        other => OpResult::error("ERR_PLY", format!("unknown fs op {other}")),
    }
}

// ------------------------------------------------------------ processes

/// Validate a `proc.exec` call into the process to start.
fn exec_request(fs: &Fs, args: &Value, stdin: Option<Vec<u8>>) -> Result<ExecSpec, OpResult> {
    let argv: Vec<String> = match args.get("argv").and_then(Value::as_array) {
        Some(items) if !items.is_empty() && items.iter().all(Value::is_string) => {
            items.iter().filter_map(Value::as_str).map(str::to_string).collect()
        }
        _ => return Err(OpResult::error("ERR_INVALID_ARG_TYPE", "argv must be a non-empty array of strings")),
    };
    let mut env = BTreeMap::new();
    if let Some(map) = args.get("env").and_then(Value::as_object) {
        for (k, v) in map {
            let Some(v) = v.as_str() else {
                return Err(OpResult::error("ERR_INVALID_ARG_TYPE", format!("env.{k} must be a string")));
            };
            env.insert(k.clone(), v.to_string());
        }
    }
    let timeout = args
        .get("timeout")
        .and_then(Value::as_f64)
        .filter(|ms| ms.is_finite() && *ms > 0.0)
        .map(|ms| Duration::from_secs_f64(ms / 1000.0));
    let max_output = args.get("maxOutput").and_then(Value::as_u64).unwrap_or(0) as usize;
    let cwd = fs.resolve(args.get("cwd").and_then(Value::as_str).unwrap_or("."));
    Ok(ExecSpec { argv, cwd, env, clear_env: false, stdin, timeout, max_output })
}

async fn run_exec(fs: Fs, spec: ExecSpec) -> OpResult {
    let program = spec.argv[0].clone();
    match fs.io().exec(spec).await {
        Ok(result) => OpResult::Json(serde_json::to_value(result).unwrap_or(Value::Null)),
        Err(err) => node_error(SysError::from(err), "spawn", &program, None),
    }
}
