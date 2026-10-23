//! `wendy` — command line entry.
//!
//! ```text
//! wendy tools                                   list the tools of the loaded plugins
//! wendy call edit '{"path": "a.txt", ...}'      run one tool call, print its output
//! wendy mcp                                     serve the tools over MCP (stdio)
//! wendy run ./script.ts -- arg1                 run a TS/JS file in the node-aligned sandbox
//! ```
//!
//! Everything runs over explicitly mounted storage (default: the current
//! directory, read-write, at its own path, so the file tools and the system
//! bash see the same paths). `--ask` makes every write and every process
//! start wait for a y/a/n answer on the terminal; `--deny-write PATH`
//! refuses writes below PATH; `--allow-net` lets processes use the network;
//! `--trace FILE` appends every trace event to a JSONL file.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use wendy_io::{
    Access, AllowAll, Answer, Approver, ExecPolicy, Gate, Io, IoRequest, MountMode, Policy, Rule, RuleAction,
};
use wendy_jsc::{JsRuntime, Limits, ModuleSource, RealmConfig};
use wendy_kernel::{Kernel, KernelConfig, KernelError, PluginSource};
use wendy_storage::{DiskStorage, StorageSpec};
use wendy_trace::{JsonlHook, Tracer};

#[derive(Parser)]
#[command(name = "wendy", version, about = "wendy agent harness")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the tools of the loaded plugins (JSON).
    Tools(ToolArgs),
    /// Run one tool call and print its output (JSON).
    Call(CallArgs),
    /// Serve the tools to an agent over MCP (JSON-RPC on stdin/stdout).
    Mcp(ToolArgs),
    /// Run a TypeScript/JavaScript file in wendy's node-aligned sandbox.
    Run(RunArgs),
}

#[derive(clap::Args)]
struct Sandbox {
    /// `AT=disk:PATH[:ro]`, `AT=sqlite:PATH[:ro]`, `AT=memory`, or
    /// `disk:PATH[:ro]` (mounted at its own absolute path). Repeatable.
    /// Default: the current directory, read-write, at its own path.
    #[arg(long = "mount", value_name = "SPEC")]
    mounts: Vec<String>,
    /// Starting directory inside the namespace (default: first mount).
    #[arg(long)]
    cwd: Option<String>,
    /// Append trace events (JSON lines) to this file.
    #[arg(long)]
    trace: Option<PathBuf>,
    /// Ask on the terminal before every write and every process start.
    #[arg(long)]
    ask: bool,
    /// Refuse writes (and process starts) under this path. Repeatable.
    #[arg(long = "deny-write", value_name = "PATH")]
    deny_write: Vec<String>,
    /// Let processes started by tools use the network.
    #[arg(long)]
    allow_net: bool,
    /// Host directory processes may not read. Repeatable.
    #[arg(long = "deny-read", value_name = "PATH")]
    deny_read: Vec<PathBuf>,
}

#[derive(clap::Args)]
struct ToolArgs {
    #[command(flatten)]
    sandbox: Sandbox,
    /// Load a plugin (a directory with main.ts / index.ts, or an entry
    /// file). Repeatable.
    #[arg(long = "plugin", value_name = "PATH")]
    plugins: Vec<PathBuf>,
    /// Do not load wendy's built-in tools (bash, read, edit, create, delete).
    #[arg(long)]
    no_builtin: bool,
}

#[derive(clap::Args)]
struct CallArgs {
    #[command(flatten)]
    tools: ToolArgs,
    /// Tool name.
    tool: String,
    /// Tool input as JSON (default `{}`).
    input: Option<String>,
}

#[derive(clap::Args)]
struct RunArgs {
    #[command(flatten)]
    sandbox: Sandbox,
    /// Entry module (`.ts`, `.mts`, `.js`, `.mjs`); its directory is the
    /// root modules may be imported from.
    file: PathBuf,
    /// Arguments visible as `process.argv.slice(2)`.
    #[arg(last = true)]
    args: Vec<String>,
}

fn parse_mount(spec: &str, base: &std::path::Path) -> Result<(String, StorageSpec, MountMode)> {
    let (rest, mode) = match spec.strip_suffix(":ro") {
        Some(r) => (r, MountMode::ReadOnly),
        None => (spec.strip_suffix(":rw").unwrap_or(spec), MountMode::ReadWrite),
    };
    // `disk:PATH` alone: mounted at its own canonical path.
    if let Some(path) = rest.strip_prefix("disk:") {
        let host = std::fs::canonicalize(base.join(path)).with_context(|| format!("mount {spec}"))?;
        return Ok((host.display().to_string(), StorageSpec::Disk { root: host }, mode));
    }
    let (at, kind) = rest.split_once('=').context("mount must look like AT=KIND[:PATH][:ro] or disk:PATH[:ro]")?;
    let storage = match kind.split_once(':') {
        Some(("disk", path)) => StorageSpec::Disk { root: path.into() },
        Some(("sqlite", path)) => StorageSpec::Sqlite { path: path.into() },
        None if kind == "memory" => StorageSpec::Memory,
        _ => bail!("unknown storage in `{spec}` (use disk:PATH, sqlite:PATH or memory)"),
    };
    Ok((at.to_string(), storage, mode))
}

/// Asks on the controlling terminal; stdin may be busy carrying a protocol.
struct TerminalApprover;

#[async_trait::async_trait]
impl Approver for TerminalApprover {
    async fn approve(&self, req: &IoRequest) -> Answer {
        let what = match &req.argv {
            Some(argv) => format!("run `{}` in {}", argv.join(" "), req.path),
            None => format!("{} {}{}", req.op.as_str(), req.path, req.to.as_deref().map(|t| format!(" -> {t}")).unwrap_or_default()),
        };
        let op = req.actor.op.as_deref().map(|op| format!(" ({op})")).unwrap_or_default();
        let question = format!("wendy: allow {} `{}`{op} to {what}? [y]es / [a]lways / [n]o: ", req.actor.session, req.actor.label);
        let answer = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
            let mut tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty")?;
            tty.write_all(question.as_bytes())?;
            let mut line = String::new();
            std::io::BufReader::new(tty).read_line(&mut line)?;
            Ok(line.trim().to_lowercase())
        })
        .await;
        match answer {
            Ok(Ok(a)) if a == "y" || a == "yes" => Answer::Once,
            Ok(Ok(a)) if a == "a" || a == "always" => Answer::Always,
            Ok(Ok(_)) => Answer::Deny("declined on the terminal".into()),
            _ => Answer::Deny("no terminal to ask on".into()),
        }
    }
}

/// The IO layer everything runs on, plus the starting directory.
fn build_io(sandbox: &Sandbox) -> Result<(Io, String)> {
    let tracer = Tracer::new();
    // One trace file may collect many runs; op ids are per run.
    let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    tracer.set_run(Some(format!("{}-{}", started.as_millis(), std::process::id())));
    if let Some(path) = &sandbox.trace {
        tracer.add_hook(JsonlHook::open(path).with_context(|| format!("opening trace file {}", path.display()))?);
    }
    let gate: Arc<dyn Gate> = if sandbox.ask || !sandbox.deny_write.is_empty() {
        let mut policy = Policy::new().default_write(if sandbox.ask { RuleAction::Ask } else { RuleAction::Allow });
        for prefix in &sandbox.deny_write {
            policy = policy.rule(Rule::new(Some(Access::Write), prefix.clone(), RuleAction::Deny));
        }
        Arc::new(policy.approver(Kernel::approver(&tracer, Arc::new(TerminalApprover))))
    } else {
        Arc::new(AllowAll)
    };
    let io = Io::new(tracer, gate);
    io.set_exec_policy(ExecPolicy { allow_net: sandbox.allow_net, deny_read: sandbox.deny_read.clone(), ..ExecPolicy::default() });
    let base = std::env::current_dir()?;
    let specs = if sandbox.mounts.is_empty() { vec!["disk:.".to_string()] } else { sandbox.mounts.clone() };
    let mut first_mount = None;
    for spec in &specs {
        let (at, storage, mode) = parse_mount(spec, &base)?;
        let opened = storage.open(&base).with_context(|| format!("opening {spec}"))?;
        io.mount(&at, opened, mode).with_context(|| format!("mounting {spec}"))?;
        first_mount.get_or_insert(at);
    }
    let start = sandbox.cwd.clone().or(first_mount).unwrap_or_else(|| "/".into());
    Ok((io, start))
}

async fn kernel(args: &ToolArgs) -> Result<Kernel> {
    let (io, cwd) = build_io(&args.sandbox)?;
    let kernel = Kernel::new(KernelConfig::new(io, cwd));
    if !args.no_builtin {
        kernel.load(PluginSource::builtin()).await.context("loading the built-in tools")?;
    }
    for path in &args.plugins {
        kernel.load(PluginSource::Path(path.clone())).await.with_context(|| format!("loading plugin {}", path.display()))?;
    }
    Ok(kernel)
}

async fn list_tools(args: ToolArgs) -> Result<i32> {
    let kernel = kernel(&args).await?;
    println!("{}", serde_json::to_string_pretty(&kernel.tools())?);
    Ok(0)
}

async fn call_tool(args: CallArgs) -> Result<i32> {
    let input: Value = match &args.input {
        Some(text) => serde_json::from_str(text).context("tool input must be JSON")?,
        None => json!({}),
    };
    let kernel = kernel(&args.tools).await?;
    let result = kernel.call(&args.tool, input).await;
    for plugin in kernel.plugins() {
        if let Some(out) = kernel.take_output(&plugin.name).await {
            eprint!("{}{}", out.stdout, out.stderr);
        }
    }
    match result {
        Ok(Value::String(text)) => println!("{text}"),
        Ok(output) => println!("{}", serde_json::to_string_pretty(&output)?),
        Err(err) => {
            eprintln!("wendy: {}: {err}", args.tool);
            return Ok(if matches!(err, KernelError::UnknownTool(_)) { 2 } else { 1 });
        }
    }
    Ok(0)
}

// ------------------------------------------------------------------- mcp

/// MCP over stdio: one JSON-RPC message per line. Requests are handled
/// concurrently; stdout carries nothing but protocol messages.
async fn serve_mcp(args: ToolArgs) -> Result<i32> {
    let kernel = kernel(&args).await?;
    let out = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut tasks = tokio::task::JoinSet::new();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let (kernel, out) = (kernel.clone(), out.clone());
        tasks.spawn(async move {
            let response = match serde_json::from_str::<Value>(&line) {
                Ok(message) => handle_mcp(&kernel, message).await,
                Err(e) => Some(rpc_error(Value::Null, -32700, &format!("parse error: {e}"))),
            };
            if let Some(response) = response {
                let mut out = out.lock().await;
                let _ = out.write_all(format!("{response}\n").as_bytes()).await;
                let _ = out.flush().await;
            }
        });
    }
    while tasks.join_next().await.is_some() {}
    Ok(0)
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

async fn handle_mcp(kernel: &Kernel, message: Value) -> Option<Value> {
    // Messages without a method are responses to us; we send no requests.
    let method = message.get("method")?.as_str()?.to_string();
    let id = message.get("id").cloned();
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    let result: Result<Value, (i64, String)> = match method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18"),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "wendy", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "wendy runs each tool call as a traced operation over mounted storage. \
                File tools only see the mounts; bash may write only inside read-write mounts.",
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": kernel.tools().into_iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
            })).collect::<Vec<_>>(),
        })),
        "tools/call" => match params.get("name").and_then(Value::as_str) {
            None => Err((-32602, "tools/call needs a tool name".into())),
            Some(name) => {
                let input = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                match kernel.call(name, input).await {
                    Ok(output) => Ok(tool_result(&output)),
                    Err(KernelError::UnknownTool(name)) => Err((-32602, format!("unknown tool: {name}"))),
                    Err(err) => Ok(json!({ "content": [{ "type": "text", "text": err.to_string() }], "isError": true })),
                }
            }
        },
        _ if method.starts_with("notifications/") => return None,
        other => Err((-32601, format!("method not found: {other}"))),
    };
    let id = id?;
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => rpc_error(id, code, &message),
    })
}

fn tool_result(output: &Value) -> Value {
    let text = match output {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    let mut result = json!({ "content": [{ "type": "text", "text": text }], "isError": false });
    if output.is_object() {
        result["structuredContent"] = output.clone();
    }
    result
}

// ------------------------------------------------------------------- run

async fn run_js(args: RunArgs) -> Result<i32> {
    let (io, start) = build_io(&args.sandbox)?;
    let file = std::fs::canonicalize(&args.file).with_context(|| format!("opening {}", args.file.display()))?;
    let dir = file.parent().context("entry file has no directory")?;
    let root = dir.display().to_string();
    // The script's own code comes from a read-only namespace of its own.
    let code = Io::new(io.tracer().clone(), Arc::new(AllowAll));
    code.mount(&root, Arc::new(DiskStorage::open(dir)?), MountMode::ReadOnly)?;
    let name = file.file_stem().and_then(|s| s.to_str()).unwrap_or("main").to_string();
    let entry = file.display().to_string();
    let runtime = JsRuntime::start();
    let realm = runtime
        .create_realm(RealmConfig {
            name,
            io,
            cwd: start.clone(),
            env: BTreeMap::from([("HOME".to_string(), start), ("USER".to_string(), "wendy".to_string())]),
            argv: std::iter::once(entry.clone()).chain(args.args).collect(),
            modules: ModuleSource { io: code, root },
            limits: Limits::default(),
            bridge: None,
        })
        .await?;
    let result = runtime.run_main(realm, &entry).await;
    // Like node: stay until nothing is scheduled (unless it already failed).
    if result.is_ok() {
        runtime.idle(realm).await;
    }
    let out = runtime.take_output(realm).await;
    std::io::stdout().write_all(out.stdout.as_bytes())?;
    std::io::stderr().write_all(out.stderr.as_bytes())?;
    Ok(match result {
        Ok(_) => out.exit_code.unwrap_or(0),
        Err(e) => match e.exit_code {
            Some(code) => code,
            None => {
                eprintln!("{}", e.message);
                if let Some(stack) = e.stack {
                    eprintln!("{stack}");
                }
                1
            }
        },
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let status = match cli.command {
        Cmd::Tools(args) => list_tools(args).await?,
        Cmd::Call(args) => call_tool(args).await?,
        Cmd::Mcp(args) => serve_mcp(args).await?,
        Cmd::Run(args) => run_js(args).await?,
    };
    std::process::exit(status);
}
