//! `wendy` — command line entry.
//!
//! ```text
//! wendy tools                              describe the loaded plugins
//! wendy call edit '{"path": "a.txt", ...}' run one plugin, print its output
//! wendy mcp                                serve the plugins as MCP tools (stdio)
//! wendy run ./script.ts -- arg1            run a TS/JS file, with `tools` available
//! ```
//!
//! Everything runs over explicitly mounted storage (default: the current
//! directory, read-write, at its own path, so the file plugins and the
//! system bash see the same paths). The default plugins (agent, bash,
//! create, edit, remove) are loaded unless `--no-default-plugins`;
//! `--plugin DIR` adds more. `--record FILE` appends every fiber event as a JSON line.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use wendy_kernel::{Io, JsonlRecorder, Kernel, KernelConfig, KernelError, MountMode, PluginSource, StorageSpec};

#[derive(Parser)]
#[command(name = "wendy", version, about = "wendy agent harness")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Describe the loaded plugins (JSON).
    Tools(Setup),
    /// Run one plugin and print its output (JSON).
    Call(CallArgs),
    /// Serve the plugins to an agent as MCP tools (JSON-RPC on stdin/stdout).
    Mcp(Setup),
    /// Run a TypeScript/JavaScript file, with `tools` available.
    Run(RunArgs),
}

#[derive(clap::Args)]
struct Setup {
    /// `AT=disk:PATH[:ro]`, `AT=sqlite:PATH[:ro]`, `AT=memory`, or
    /// `disk:PATH[:ro]` (mounted at its own absolute path). Repeatable.
    /// Default: the current directory, read-write, at its own path.
    #[arg(long = "mount", value_name = "SPEC")]
    mounts: Vec<String>,
    /// Starting directory inside the namespace (default: first mount).
    #[arg(long)]
    cwd: Option<String>,
    /// Load a plugin directory (its name is the directory's). Repeatable.
    #[arg(long = "plugin", value_name = "DIR")]
    plugins: Vec<PathBuf>,
    /// Do not load agent, bash, create, edit and remove.
    #[arg(long)]
    no_default_plugins: bool,
    /// Append every fiber event (JSON lines) to this file.
    #[arg(long, value_name = "FILE")]
    record: Option<PathBuf>,
}

#[derive(clap::Args)]
struct CallArgs {
    #[command(flatten)]
    setup: Setup,
    /// Plugin name.
    plugin: String,
    /// Input as JSON (default `{}`).
    input: Option<String>,
}

#[derive(clap::Args)]
struct RunArgs {
    #[command(flatten)]
    setup: Setup,
    /// Entry module (`.ts`, `.mts`, `.js`, `.mjs`); its directory is the
    /// root modules may be imported from.
    file: PathBuf,
    /// Arguments visible as `process.argv.slice(2)`.
    #[arg(last = true)]
    args: Vec<String>,
}

fn parse_mount(spec: &str, base: &Path) -> Result<(String, StorageSpec, MountMode)> {
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

async fn kernel(setup: &Setup) -> Result<Kernel> {
    let io = Io::new();
    let base = std::env::current_dir()?;
    let specs = if setup.mounts.is_empty() { vec!["disk:.".to_string()] } else { setup.mounts.clone() };
    let mut first_mount = None;
    for spec in &specs {
        let (at, storage, mode) = parse_mount(spec, &base)?;
        let opened = storage.open(&base).with_context(|| format!("opening {spec}"))?;
        io.mount(&at, opened, mode).with_context(|| format!("mounting {spec}"))?;
        first_mount.get_or_insert(at);
    }
    let cwd = setup.cwd.clone().or(first_mount).unwrap_or_else(|| "/".into());
    let kernel = Kernel::new(KernelConfig::new(io, cwd));
    if let Some(path) = &setup.record {
        kernel.add_hook(JsonlRecorder::open(path).with_context(|| format!("opening record file {}", path.display()))?);
    }
    let defaults = if setup.no_default_plugins { Vec::new() } else { PluginSource::defaults() };
    for plugin in defaults {
        kernel.load(plugin).await.context("loading the default plugins")?;
    }
    for dir in &setup.plugins {
        kernel.load(PluginSource::Dir(dir.clone())).await.with_context(|| format!("loading plugin {}", dir.display()))?;
    }
    Ok(kernel)
}

async fn describe(setup: Setup) -> Result<i32> {
    let kernel = kernel(&setup).await?;
    println!("{}", serde_json::to_string_pretty(&kernel.plugins())?);
    Ok(0)
}

async fn call(args: CallArgs) -> Result<i32> {
    let input: Value = match &args.input {
        Some(text) => serde_json::from_str(text).context("input must be JSON")?,
        None => json!({}),
    };
    let kernel = kernel(&args.setup).await?;
    let result = kernel.call(&args.plugin, input).await;
    for plugin in kernel.plugins() {
        if let Some(out) = kernel.take_output(&plugin.name).await {
            eprint!("{}{}", out.stdout, out.stderr);
        }
    }
    match result {
        Ok(Value::String(text)) => println!("{text}"),
        Ok(output) => println!("{}", serde_json::to_string_pretty(&output)?),
        Err(err) => {
            eprintln!("wendy: {}: {err}", args.plugin);
            return Ok(if matches!(err, KernelError::UnknownPlugin(_)) { 2 } else { 1 });
        }
    }
    Ok(0)
}

async fn run(args: RunArgs) -> Result<i32> {
    let kernel = kernel(&args.setup).await?;
    let outcome = kernel.run_script(&args.file, args.args).await?;
    std::io::stdout().write_all(outcome.output.stdout.as_bytes())?;
    std::io::stderr().write_all(outcome.output.stderr.as_bytes())?;
    Ok(match outcome.result {
        Ok(()) => outcome.output.exit_code.unwrap_or(0),
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

// ------------------------------------------------------------------- mcp

/// One JSON-RPC message per line on stdin/stdout. Requests are handled
/// concurrently; stdout carries nothing but protocol messages.
async fn serve_mcp(setup: Setup) -> Result<i32> {
    let kernel = kernel(&setup).await?;
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
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": kernel.catalog().into_iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.parameters,
            })).collect::<Vec<_>>(),
        })),
        "tools/call" => match params.get("name").and_then(Value::as_str) {
            None => Err((-32602, "tools/call needs a tool name".into())),
            Some(name) => match kernel.catalog().into_iter().find(|t| t.name == name) {
                None => Err((-32602, format!("unknown tool: {name}"))),
                Some(tool) => {
                    let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                    let input = if tool.wraps_input { arguments.get("input").cloned().unwrap_or(Value::Null) } else { arguments };
                    match kernel.call(name, input).await {
                        Ok(output) => Ok(tool_result(&output)),
                        Err(err) => Ok(json!({ "content": [{ "type": "text", "text": err.to_string() }], "isError": true })),
                    }
                }
            },
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let status = match cli.command {
        Cmd::Tools(setup) => describe(setup).await?,
        Cmd::Call(args) => call(args).await?,
        Cmd::Mcp(setup) => serve_mcp(setup).await?,
        Cmd::Run(args) => run(args).await?,
    };
    std::process::exit(status);
}
