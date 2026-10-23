//! Realms: one plugin, one JSC context group, one OS thread.
//!
//! Important decisions:
//! - JSC is single-threaded per context group, so every realm gets its own
//!   group on its own thread. A plugin that blocks its thread (a sync fs
//!   call waiting for an approval, a long computation) holds up only itself.
//!   Callers talk to a realm through messages; every reply is a oneshot.
//! - Async host ops run on tokio; completion is a message back to the
//!   realm's thread, which resolves the deferred promise. JSC drains
//!   microtasks after each call into JS, so `await` continuations run there.
//!   Sync host ops (`readFileSync`) block the realm's thread on the same
//!   futures, like node's sync APIs block its loop.
//! - Nothing that arrives from JS is trusted. The prelude shares the realm
//!   with plugin code, which can replace any global the prelude uses, so
//!   every host op re-validates here: a module loads only if wendy itself
//!   resolved it inside the plugin root; trace labels come from a fixed list
//!   per op; settles, timers and IPO phases must belong to this realm. A
//!   tampered call can at most ask for something the plugin could have asked
//!   for honestly, and the tampering is traced.
//! - Each entry into JS gets a CPU budget and the process a memory ceiling
//!   (a watchdog checks both every 100 ms of CPU). A turn over either is
//!   terminated, the realm's pending calls fail, the realm stays usable.
//! - Plugin source is read through wendy-io (its own namespace, read-only),
//!   so loading code is a traced read like any other and plugins can live on
//!   any storage. IO the realm performs is attributed to the IPO operation
//!   it runs for when that is unambiguous (exactly one in flight).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::c_void;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::oneshot;
use wendy_io::{Actor, EntryKind, ExecSpec, Io, Runtime as IoRuntime};
use wendy_libc::{Errno, Fs, SysError, path as lpath};
use wendy_trace::{Scope, Tracer, Verdict};

use crate::ffi::*;
use crate::transpile::transpile;

const PRELUDE: &str = include_str!("prelude.js");
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
/// `import ... from "wendy"`: the plugin SDK.
const SDK: &str = "wendy";
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

/// Who called a bridged op. Filled in by wendy, never by plugin code.
#[derive(Debug, Clone)]
pub struct Caller {
    pub realm: String,
    /// The IPO operation the call belongs to, when known.
    pub op: Option<String>,
}

/// The host side of `kernel.*` ops (other tools, waking, listing).
#[async_trait::async_trait]
pub trait HostBridge: Send + Sync {
    async fn call(&self, caller: Caller, op: &str, args: Value) -> Result<Value, String>;
}

pub struct RealmConfig {
    pub name: String,
    /// The shared IO layer; the realm acts on it as a `js` actor.
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

#[derive(Debug, Clone, PartialEq, Default)]
pub struct JsError {
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
    fn error(code: &str, message: impl Into<String>) -> OpResult {
        OpResult::Error(json!({ "code": code, "message": message.into() }))
    }
}

type Reply<T> = oneshot::Sender<T>;

enum Msg {
    /// `api[method](callId, ...args)`; the prelude reports back via `settle`.
    Api { method: &'static str, args: Vec<String>, op: Option<String>, reply: Reply<Result<Value, JsError>> },
    TakeOutput(Reply<Output>),
    Idle(Reply<()>),
    Rename(String),
    RawHostCall(String, Value, Reply<Value>),
    Completion(u64, OpResult),
    Timer(u64),
    Shutdown,
}

struct RealmThread {
    tx: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

/// Owns the realm threads. Dropping it stops them.
pub struct JsRuntime {
    handle: tokio::runtime::Handle,
    realms: Mutex<Vec<RealmThread>>,
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
        JsRuntime { handle, realms: Mutex::new(Vec::new()) }
    }

    pub async fn create_realm(&self, config: RealmConfig) -> Result<RealmId, JsError> {
        let (tx, rx) = channel();
        let (ready, ready_rx) = oneshot::channel();
        let loop_tx = tx.clone();
        let handle = self.handle.clone();
        let thread = std::thread::Builder::new()
            .name(format!("wendy-js-{}", config.name))
            .spawn(move || engine_main(config, rx, loop_tx, handle, ready))
            .map_err(|e| JsError { message: format!("starting realm thread: {e}"), ..JsError::default() })?;
        let id = {
            let mut realms = self.realms.lock().expect("realm table poisoned");
            realms.push(RealmThread { tx, thread: Some(thread) });
            RealmId(realms.len() - 1)
        };
        ready_rx.await.unwrap_or_else(|_| Err(gone()))?;
        Ok(id)
    }

    fn send(&self, realm: RealmId, msg: Msg) -> bool {
        let realms = self.realms.lock().expect("realm table poisoned");
        realms.get(realm.0).is_some_and(|r| r.tx.send(msg).is_ok())
    }

    async fn api(&self, realm: RealmId, method: &'static str, args: Vec<String>, op: Option<String>) -> Result<Value, JsError> {
        let (reply, rx) = oneshot::channel();
        if !self.send(realm, Msg::Api { method, args, op, reply }) {
            return Err(gone());
        }
        rx.await.unwrap_or_else(|_| Err(gone()))
    }

    /// Load `module` (a path in the realm's module namespace) as the entry
    /// and wait for it to finish evaluating. Returns the names it exports.
    pub async fn run_main(&self, realm: RealmId, module: &str) -> Result<Value, JsError> {
        self.api(realm, "main", vec![module.into()], None).await
    }

    /// Call an export of a module (`"default.process"` walks properties),
    /// awaiting the result if it is a promise.
    pub async fn call(&self, realm: RealmId, module: &str, export: &str, args: Vec<Value>) -> Result<Value, JsError> {
        self.api(realm, "call", vec![module.into(), export.into(), Value::Array(args).to_string()], None).await
    }

    /// Evaluate a plain-JS script in the realm (awaiting a promise result).
    pub async fn eval(&self, realm: RealmId, code: &str) -> Result<Value, JsError> {
        self.api(realm, "eval", vec![code.into()], None).await
    }

    /// The plugin a module's default export defines (`definePlugin`), as
    /// JSON: name, description, version, tools with their input schemas.
    pub async fn describe_plugin(&self, realm: RealmId, module: &str) -> Result<Value, JsError> {
        self.api(realm, "describe", vec![module.into()], None).await
    }

    /// Run one tool of the plugin as IPO operation `op`: its input, process
    /// and output phases, in this realm.
    pub async fn invoke_tool(&self, realm: RealmId, module: &str, tool: &str, input: &Value, op: &str) -> Result<Value, JsError> {
        let args = vec![module.into(), tool.into(), input.to_string(), op.into()];
        self.api(realm, "invoke", args, Some(op.to_string())).await
    }

    /// Everything the realm wrote to stdout/stderr since the last call.
    pub async fn take_output(&self, realm: RealmId) -> Output {
        let (reply, rx) = oneshot::channel();
        if !self.send(realm, Msg::TakeOutput(reply)) {
            return Output::default();
        }
        rx.await.unwrap_or_default()
    }

    /// Change the name the realm acts and traces under (its IO actor's
    /// session). Applies to everything the realm does after this.
    pub fn rename(&self, realm: RealmId, name: &str) {
        self.send(realm, Msg::Rename(name.to_string()));
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
        let mut realms = self.realms.lock().expect("realm table poisoned");
        for r in realms.iter() {
            let _ = r.tx.send(Msg::Shutdown);
        }
        for r in realms.iter_mut() {
            if let Some(t) = r.thread.take() {
                let _ = t.join();
            }
        }
    }
}

// ---------------------------------------------------------------- engine

struct Pending {
    reply: Reply<Result<Value, JsError>>,
    op: Option<String>,
}

struct Engine {
    group: JSContextGroupRef,
    ctx: JSGlobalContextRef,
    api: JSObjectRef,
    name: String,
    fs: Fs,
    code: Fs,
    root: String,
    tracer: Tracer,
    bridge: Option<Arc<dyn HostBridge>>,
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    /// Module ids wendy resolved for this realm, and those it loaded.
    resolved: HashSet<String>,
    loaded: HashSet<String>,
    /// Armed timers: id → repeat interval (ms) for intervals.
    timers: HashMap<u64, Option<f64>>,
    pending_ops: HashMap<u64, JSObjectRef>,
    calls: HashMap<u64, Pending>,
    /// Calls failed by a termination; their late settles are expected.
    abandoned: HashSet<u64>,
    idle_waiters: Vec<Reply<()>>,
    next_id: u64,
    tx: Sender<Msg>,
    handle: tokio::runtime::Handle,
    started: Instant,
}

impl Engine {
    fn next(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// The IPO operation in flight, if exactly one is.
    fn current_op(&self) -> Option<String> {
        let mut ops = self.calls.values().filter_map(|p| p.op.as_ref());
        let first = ops.next()?;
        ops.all(|o| o == first).then(|| first.clone())
    }

    fn is_active_op(&self, op: &str) -> bool {
        self.calls.values().any(|p| p.op.as_deref() == Some(op))
    }

    fn is_idle(&self) -> bool {
        self.timers.is_empty() && self.pending_ops.is_empty() && self.calls.is_empty()
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
    static TERMINATED: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Short, non-reentrant access to the engine. Never call into JS inside.
fn with<R>(f: impl FnOnce(&mut Engine) -> R) -> R {
    ENGINE.with(|e| f(e.borrow_mut().as_mut().expect("JS engine is running")))
}

unsafe extern "C" fn should_terminate(_ctx: JSContextRef, _context: *mut c_void) -> bool {
    let ticks = TICKS.get() + 1;
    TICKS.set(ticks);
    let reason = if ticks >= BUDGET_TICKS.get() {
        Some(format!("used more than {:.1}s of CPU in one turn", f64::from(BUDGET_TICKS.get()) * WATCHDOG_TICK))
    } else {
        let ceiling = MEMORY_CEILING.get();
        (ceiling > 0 && footprint() > ceiling).then(|| format!("process memory passed {} MiB", ceiling >> 20))
    };
    match reason {
        Some(reason) => {
            TERMINATED.with(|t| *t.borrow_mut() = Some(reason));
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

/// Run `f`, which enters JS, with a fresh CPU budget.
fn enter<T>(f: impl FnOnce() -> T) -> T {
    TICKS.set(0);
    f()
}

fn engine_main(
    config: RealmConfig,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    handle: tokio::runtime::Handle,
    ready: Reply<Result<(), JsError>>,
) {
    let _ = ready.send(init(config, tx, handle));
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Shutdown => break,
            Msg::Api { method, args, op, reply } => start_call(method, args, op, reply),
            Msg::TakeOutput(reply) => {
                let out = with(|e| Output {
                    stdout: std::mem::take(&mut e.stdout),
                    stderr: std::mem::take(&mut e.stderr),
                    exit_code: e.exit_code,
                });
                let _ = reply.send(out);
            }
            Msg::Idle(reply) => with(|e| e.idle_waiters.push(reply)),
            Msg::Rename(name) => with(|e| {
                let renamed = |fs: &Fs| {
                    let actor = Actor { session: name.clone(), ..fs.io().actor().clone() };
                    Fs::new(fs.io().as_actor(actor), fs.cwd())
                };
                e.fs = renamed(&e.fs);
                e.code = renamed(&e.code);
                e.name = name;
            }),
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

fn init(config: RealmConfig, tx: Sender<Msg>, handle: tokio::runtime::Handle) -> Result<(), JsError> {
    let fail = |message: String| JsError { message, ..JsError::default() };
    let actor = Actor::new(IoRuntime::Js, config.name.clone(), "js");
    let io = config.io.as_actor(actor.clone());
    let code_io = config.modules.io.as_actor(Actor { label: "module.load".into(), ..actor });
    let tracer = io.tracer().clone();
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
                group,
                ctx,
                api: null_mut(),
                name: config.name.clone(),
                fs: Fs::new(io, config.cwd.clone()),
                code: Fs::new(code_io, root.clone()),
                root,
                tracer,
                bridge: config.bridge,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
                resolved: HashSet::new(),
                loaded: HashSet::new(),
                timers: HashMap::new(),
                pending_ops: HashMap::new(),
                calls: HashMap::new(),
                abandoned: HashSet::new(),
                idle_waiters: Vec::new(),
                next_id: 0,
                tx,
                handle,
                started: Instant::now(),
            })
        });
        let prelude = enter(|| eval(ctx, PRELUDE, "wendy:prelude")).map_err(fail)?;
        let host = JSObjectMakeFunctionWithCallback(ctx, JsStr::new("host").0, host_sync);
        let host_async_fn = JSObjectMakeFunctionWithCallback(ctx, JsStr::new("hostAsync").0, host_async);
        let settings = json!({ "env": config.env, "argv": config.argv, "version": env!("CARGO_PKG_VERSION") }).to_string();
        let prelude_fn = JSValueToObject(ctx, prelude, null_mut());
        let api = enter(|| call(ctx, prelude_fn, &[host as JSValueRef, host_async_fn as JSValueRef, string_value(ctx, &settings)]))
            .map_err(fail)?;
        JSValueProtect(ctx, api);
        let api = api as JSObjectRef;
        with(|e| e.api = api);
        let on_rejection = JSValueToObject(ctx, get_property(ctx, api, "onUnhandledRejection"), null_mut());
        JSGlobalContextSetUnhandledRejectionCallback(ctx, on_rejection, null_mut());
    }
    Ok(())
}

fn teardown() {
    let Some(engine) = ENGINE.with(|e| e.borrow_mut().take()) else { return };
    unsafe {
        for (_, resolve) in engine.pending_ops {
            JSValueUnprotect(engine.ctx, resolve as JSValueRef);
        }
        if !engine.api.is_null() {
            JSValueUnprotect(engine.ctx, engine.api as JSValueRef);
        }
        JSGlobalContextRelease(engine.ctx);
        JSContextGroupRelease(engine.group);
    }
}

/// After each message: report a terminated turn, wake idle waiters.
fn after_turn() {
    if let Some(reason) = TERMINATED.with(|t| t.borrow_mut().take()) {
        let (name, tracer, calls) = with(|e| {
            e.stderr.push_str(&format!("wendy: {}: script terminated: {reason}\n", e.name));
            let calls = std::mem::take(&mut e.calls);
            e.abandoned.extend(calls.keys());
            (e.name.clone(), e.tracer.clone(), calls)
        });
        tracer.emit(Scope::Exec, "js.terminated", Verdict::Error, json!({ "realm": name, "reason": reason }));
        for (_, pending) in calls {
            let message = format!("{name}: script terminated: {reason}");
            let _ = pending.reply.send(Err(JsError { message, ..JsError::default() }));
        }
        // A termination that hit while microtasks drained stays pending in
        // the VM, and the next entry's microtask queue is dropped while JSC
        // clears it. Spend that on an empty entry instead of a real call.
        let ctx = with(|e| e.ctx);
        let _ = enter(|| unsafe { eval(ctx, "void 0", "wendy:recover") });
        TERMINATED.with(|t| t.borrow_mut().take());
    }
    let waiters = with(|e| if e.is_idle() { std::mem::take(&mut e.idle_waiters) } else { Vec::new() });
    for w in waiters {
        let _ = w.send(());
    }
}

fn start_call(method: &'static str, args: Vec<String>, op: Option<String>, reply: Reply<Result<Value, JsError>>) {
    let (ctx, api, id) = with(|e| {
        let id = e.next();
        e.calls.insert(id, Pending { reply, op });
        (e.ctx, e.api, id)
    });
    unsafe {
        let f = JSValueToObject(ctx, get_property(ctx, api, method), null_mut());
        let mut argv = vec![JSValueMakeNumber(ctx, id as f64)];
        argv.extend(args.iter().map(|a| string_value(ctx, a)));
        if let Err(message) = enter(|| call(ctx, f, &argv))
            && let Some(pending) = with(|e| e.calls.remove(&id))
        {
            let _ = pending.reply.send(Err(JsError { message, ..JsError::default() }));
        }
    }
}

fn complete(id: u64, result: OpResult) {
    let Some((ctx, resolve)) = with(|e| e.pending_ops.remove(&id).map(|r| (e.ctx, r))) else { return };
    unsafe {
        let value = result_value(ctx, result);
        let _ = enter(|| call(ctx, resolve, &[value]));
        JSValueUnprotect(ctx, resolve as JSValueRef);
    }
}

fn fire_timer(id: u64) {
    let target = with(|e| {
        let repeat = *e.timers.get(&id)?;
        match repeat {
            Some(ms) => arm_timer(e, id, ms),
            None => {
                e.timers.remove(&id);
            }
        }
        Some((e.ctx, e.api))
    });
    let Some((ctx, api)) = target else { return };
    unsafe {
        let f = JSValueToObject(ctx, get_property(ctx, api, "fire"), null_mut());
        let _ = enter(|| call(ctx, f, &[JSValueMakeNumber(ctx, id as f64)]));
    }
}

fn arm_timer(e: &Engine, id: u64, ms: f64) {
    let tx = e.tx.clone();
    e.handle.spawn(async move {
        tokio::time::sleep(Duration::from_secs_f64(ms / 1000.0)).await;
        let _ = tx.send(Msg::Timer(id));
    });
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
        result_value(ctx, sync_op(&op, args, bytes))
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
        let (id, tx, handle) = with(|e| {
            let id = e.next();
            e.pending_ops.insert(id, resolve);
            (id, e.tx.clone(), e.handle.clone())
        });
        let finish = move |result: OpResult| {
            let _ = tx.send(Msg::Completion(id, result));
        };
        if op.starts_with("fs.") {
            match fs_for(&op, &args, false) {
                Ok(fs) => {
                    handle.spawn(async move { finish(fs_op(fs, &op, args, bytes).await) });
                }
                Err(denied) => finish(denied),
            }
        } else if op == "proc.exec" {
            match exec_request(&args, bytes, false) {
                Ok((fs, spec)) => {
                    handle.spawn(async move { finish(run_exec(fs, spec).await) });
                }
                Err(denied) => finish(denied),
            }
        } else if op.starts_with("kernel.") {
            let call = with(|e| {
                let caller_op = explicit_op(e, &args)?;
                let bridge = e.bridge.clone().ok_or_else(|| {
                    OpResult::error("ERR_WENDY_NO_KERNEL", format!("{op}: not running under the wendy kernel"))
                })?;
                Ok((bridge, Caller { realm: e.name.clone(), op: caller_op }))
            });
            match call {
                Ok((bridge, caller)) => {
                    let mut args = args;
                    if let Value::Object(map) = &mut args {
                        map.remove("op");
                    }
                    handle.spawn(async move {
                        let result = match bridge.call(caller, &op, args).await {
                            Ok(value) => OpResult::Json(value),
                            Err(message) => OpResult::error("ERR_WENDY_TOOL", message),
                        };
                        finish(result)
                    });
                }
                Err(denied) => finish(denied),
            }
        } else {
            finish(OpResult::error("ERR_WENDY", format!("{op} has no async form")));
        }
        promise as JSValueRef
    }
}

/// The op a call names explicitly (it must be one of this realm's own),
/// or the one in flight.
fn explicit_op(e: &Engine, args: &Value) -> Result<Option<String>, OpResult> {
    match args.get("op") {
        None | Some(Value::Null) => Ok(e.current_op()),
        Some(Value::String(op)) if e.is_active_op(op) => Ok(Some(op.clone())),
        Some(other) => Err(violation(e, "op", &other.to_string())),
    }
}

/// A host call no honest prelude makes: refused, and traced.
fn violation(e: &Engine, what: &str, value: &str) -> OpResult {
    e.tracer.emit(
        Scope::Exec,
        "js.violation",
        Verdict::Deny,
        json!({ "realm": e.name, "field": what, "value": value.chars().take(256).collect::<String>() }),
    );
    OpResult::error("ERR_WENDY_INVALID_CALL", format!("invalid {what} in host call"))
}

fn sync_op(op: &str, args: Value, bytes: Option<Vec<u8>>) -> OpResult {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match op {
        _ if op.starts_with("fs.") => match fs_for(op, &args, true) {
            Ok(fs) => {
                let handle = with(|e| e.handle.clone());
                handle.block_on(fs_op(fs, op, args, bytes))
            }
            Err(denied) => denied,
        },
        "proc.exec" => match exec_request(&args, bytes, true) {
            Ok((fs, spec)) => {
                let handle = with(|e| e.handle.clone());
                handle.block_on(run_exec(fs, spec))
            }
            Err(denied) => denied,
        },
        "console.write" => {
            let text = s("text");
            let fd = args.get("fd").and_then(Value::as_u64).unwrap_or(1);
            with(|e| {
                if fd == 2 { e.stderr.push_str(&text) } else { e.stdout.push_str(&text) }
                e.tracer.emit(
                    Scope::Log,
                    "js.console",
                    Verdict::Info,
                    json!({ "realm": e.name, "fd": if fd == 2 { 2 } else { 1 }, "text": text.chars().take(1024).collect::<String>() }),
                );
            });
            OpResult::Json(Value::Null)
        }
        "timer.set" => {
            let ms = args.get("ms").and_then(Value::as_f64).filter(|m| m.is_finite()).unwrap_or(0.0).max(0.0);
            let repeat = args.get("repeat").and_then(Value::as_bool).unwrap_or(false);
            let id = with(|e| {
                let id = e.next();
                e.timers.insert(id, repeat.then_some(ms.max(1.0)));
                arm_timer(e, id, ms);
                id
            });
            OpResult::Json(json!(id))
        }
        "timer.clear" => {
            if let Some(id) = args.get("id").and_then(Value::as_u64) {
                with(|e| e.timers.remove(&id));
            }
            OpResult::Json(Value::Null)
        }
        "clock.now" => OpResult::Json(json!(with(|e| e.started.elapsed().as_secs_f64() * 1000.0))),
        "process.cwd" => OpResult::Json(json!(with(|e| e.fs.cwd().to_string()))),
        "process.chdir" => {
            let dir = s("dir");
            let (fs, handle) = with(|e| (e.fs.clone(), e.handle.clone()));
            let target = fs.resolve(&dir);
            match handle.block_on(fs.labeled("process.chdir").stat(&dir)) {
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
        "ipo.phase" => with(|e| {
            let op = s("op");
            let phase = s("phase");
            if !e.is_active_op(&op) {
                return violation(e, "op", &op);
            }
            if !matches!(phase.as_str(), "input" | "process" | "output") {
                return violation(e, "phase", &phase);
            }
            e.tracer.emit(Scope::Ipo, "ipo.phase", Verdict::Info, json!({ "op": op, "phase": phase, "realm": e.name }));
            OpResult::Json(Value::Null)
        }),
        "js.unhandled" => with(|e| {
            let text = s("text");
            e.tracer.emit(
                Scope::Exec,
                "js.unhandled_rejection",
                Verdict::Error,
                json!({ "realm": e.name, "op": e.current_op(), "reason": text.chars().take(2048).collect::<String>() }),
            );
            OpResult::Json(Value::Null)
        }),
        "settle" => {
            let id = args.get("id").and_then(Value::as_u64).unwrap_or(0);
            let Some(pending) = with(|e| e.calls.remove(&id)) else {
                return with(|e| {
                    if e.abandoned.remove(&id) { OpResult::Json(Value::Null) } else { violation(e, "settle id", &id.to_string()) }
                });
            };
            let result = if args.get("ok").and_then(Value::as_bool) == Some(true) {
                Ok(args.get("value").cloned().unwrap_or(Value::Null))
            } else {
                Err(JsError {
                    message: s("error"),
                    exit_code: args.get("exit").and_then(Value::as_i64).map(|c| c as i32),
                    stack: args.get("stack").and_then(Value::as_str).filter(|st| !st.is_empty()).map(str::to_string),
                })
            };
            let _ = pending.reply.send(result);
            OpResult::Json(Value::Null)
        }
        other => OpResult::error("ERR_WENDY", format!("unknown host op {other}")),
    }
}

// -------------------------------------------------------------- modules

fn resolve_module(spec: &str, from: Option<&Value>) -> OpResult {
    let bare = spec.strip_prefix("node:").unwrap_or(spec);
    if BUILTINS.contains(&bare) {
        return OpResult::Json(json!({ "builtin": bare }));
    }
    if spec == SDK {
        return OpResult::Json(json!({ "builtin": SDK }));
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
    let (code, root, base) = match with(|e| {
        let base = match from {
            None | Some(Value::Null) => Ok(e.root.clone()),
            Some(Value::String(f)) if e.loaded.contains(f) => Ok(lpath::dirname(f)),
            Some(other) => Err(violation(e, "importer", &other.to_string())),
        }?;
        Ok((e.code.clone(), e.root.clone(), base))
    }) {
        Ok(found) => found,
        Err(denied) => return denied,
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
    let handle = with(|e| e.handle.clone());
    let probe = code.labeled("module.resolve");
    for candidate in candidates {
        if handle.block_on(probe.is_file(&candidate)) {
            with(|e| e.resolved.insert(candidate.clone()));
            return OpResult::Json(json!({ "id": candidate, "dir": lpath::dirname(&candidate) }));
        }
    }
    let shown_from = from.and_then(Value::as_str).unwrap_or("<entry>");
    OpResult::error("ERR_MODULE_NOT_FOUND", format!("Cannot find module '{spec}' imported from {shown_from}"))
}

fn load_module(id: &str) -> OpResult {
    let found = with(|e| if e.resolved.contains(id) { Ok((e.code.clone(), e.handle.clone())) } else { Err(violation(e, "module id", id)) });
    let (code, handle) = match found {
        Ok(found) => found,
        Err(denied) => return denied,
    };
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

/// Trace labels a prelude may claim for an op: the node API names that are
/// built on it. Anything else is a tampered call.
fn allowed_labels(op: &str, sync: bool) -> Vec<String> {
    let methods: &[&str] = match op {
        "fs.readFile" => &["readFile"],
        "fs.writeFile" => &["writeFile", "appendFile"],
        "fs.stat" => &["stat", "lstat"],
        "fs.readdir" => &["readdir"],
        "fs.mkdir" => &["mkdir", "mkdtemp"],
        "fs.rm" => &["rm", "rmdir"],
        "fs.unlink" => &["unlink"],
        "fs.rename" => &["rename"],
        "fs.copyFile" => &["copyFile"],
        "fs.access" => &["access", "exists"],
        "fs.realpath" => &["realpath"],
        "fs.exists" => &["exists"],
        "proc.exec" if sync => {
            return ["child_process.execFileSync", "child_process.execSync", "child_process.spawnSync"].map(String::from).to_vec();
        }
        "proc.exec" => return ["child_process.execFile", "child_process.exec", "wendy.exec"].map(String::from).to_vec(),
        _ => &[],
    };
    methods
        .iter()
        .flat_map(|m| if sync { vec![format!("fs.{m}Sync")] } else { vec![format!("fs.promises.{m}"), format!("fs.{m}")] })
        .collect()
}

/// The realm's fs, labeled and attributed for this call, or a refusal.
fn label_for(e: &Engine, op: &str, args: &Value, sync: bool) -> Result<Fs, OpResult> {
    let allowed = allowed_labels(op, sync);
    let label = match args.get("api") {
        None | Some(Value::Null) => allowed.first().cloned().ok_or_else(|| OpResult::error("ERR_WENDY", format!("unknown host op {op}")))?,
        Some(Value::String(api)) if allowed.contains(api) => api.clone(),
        Some(other) => return Err(violation(e, "api label", &other.to_string())),
    };
    let op_id = explicit_op(e, args)?;
    Ok(e.fs.labeled(&label).in_op(op_id))
}

fn fs_for(op: &str, args: &Value, sync: bool) -> Result<Fs, OpResult> {
    with(|e| label_for(e, op, args, sync))
}

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
                "mtimeMs": wendy_libc::time::unix_seconds(e.modified) as f64 * 1000.0,
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
        other => OpResult::error("ERR_WENDY", format!("unknown fs op {other}")),
    }
}

// ------------------------------------------------------------ processes

/// Validate a `proc.exec` call into the fs (for cwd and attribution) and
/// the process to start.
fn exec_request(args: &Value, stdin: Option<Vec<u8>>, sync: bool) -> Result<(Fs, ExecSpec), OpResult> {
    with(|e| {
        let fs = label_for(e, "proc.exec", args, sync)?;
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
        Ok((fs, ExecSpec { argv, cwd, env, stdin, timeout, max_output }))
    })
}

async fn run_exec(fs: Fs, spec: ExecSpec) -> OpResult {
    let program = spec.argv[0].clone();
    match fs.io().exec(spec).await {
        Ok(r) => OpResult::Json(json!({
            "code": r.code,
            "signal": r.signal,
            "stdout": base64(&r.stdout),
            "stderr": base64(&r.stderr),
            "truncated": r.truncated,
            "timedOut": r.timed_out,
            "durationMs": r.duration_ms,
            "changes": r.changes,
            "changesComplete": r.changes_complete,
        })),
        Err(err) => node_error(SysError::from(err), "spawn", &program, None),
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_the_node_names_built_on_each_op() {
        assert!(allowed_labels("fs.readFile", true).contains(&"fs.readFileSync".to_string()));
        assert!(allowed_labels("fs.readFile", false).contains(&"fs.promises.readFile".to_string()));
        assert!(!allowed_labels("fs.readFile", false).contains(&"fs.readFileSync".to_string()));
        assert!(allowed_labels("fs.writeFile", true).contains(&"fs.appendFileSync".to_string()));
        assert!(allowed_labels("proc.exec", false).contains(&"wendy.exec".to_string()));
        assert!(allowed_labels("module.load", true).is_empty());
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe]), "//4=");
    }
}
