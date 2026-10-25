//! `wendy` — command line entry.
//!
//! ```text
//! wendy tools                              describe the loaded plugins
//! wendy call edit '{"path": "a.txt", ...}' run one plugin, print its output
//! wendy mcp                                serve the plugins as MCP tools (stdio)
//! wendy run ./script.ts -- arg1            run TS/JS; import wendy:plugins
//! wendy commands                          list registered plugin commands
//! wendy tui                               run the registered TUI command
//! ```
//!
//! Everything runs over explicitly mounted storage (default: the current
//! directory, read-write, at its own path, so the file plugins and Wendy sh
//! see the same paths). The default plugins (agent, bash,
//! context, create, deepseek, edit, read, remove) are loaded unless
//! `--no-default-plugins`; `--model mimo-v2.6-pro` replaces deepseek with
//! mimo. `--plugin DIR` adds more. `--record FILE` appends every fiber event as a JSON line.

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
    /// List commands registered by the loaded plugins (JSON).
    Commands(Setup),
    /// Run one plugin and print its output (JSON).
    Call(CallArgs),
    /// Serve the plugins to an agent as MCP tools (JSON-RPC on stdin/stdout).
    Mcp(Setup),
    /// Run a TypeScript/JavaScript file; import plugins from wendy:plugins.
    Run(RunArgs),
    /// Interact with the agent and respond to tool approval requests in a terminal.
    Tui(TuiArgs),
    /// Invoke a plugin-registered command with the same setup options as call.
    #[command(external_subcommand)]
    Registered(Vec<String>),
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
    /// Enable Jev risk checks and approval coordination (checks use host TYPESAFE_API_KEY).
    #[arg(long)]
    jev: bool,
    /// Model ID; selects deepseek-* or mimo-* integration (default deepseek-v4-flash).
    #[arg(long, value_name = "MODEL")]
    model: Option<String>,
    /// MiMo endpoint/provider. All providers read the host's MIMO_API_KEY.
    #[arg(long, value_name = "PROVIDER", requires = "model", value_parser = ["xiaomi", "xiaomi-token-plan-cn", "xiaomi-token-plan-sgp", "xiaomi-token-plan-ams"])]
    mimo_provider: Option<String>,
    /// Default cumulative direct-child agent limit (root input may override; default 2).
    #[arg(long, value_name = "COUNT", value_parser = clap::value_parser!(u64).range(0..=9_007_199_254_740_991))]
    agent_max_children: Option<u64>,
    /// Required Markdown report heading; repeat for multiple sections.
    #[arg(long, value_name = "HEADING")]
    agent_report_section: Vec<String>,
    /// JSON file with context window and automatic compaction settings.
    #[arg(long, value_name = "FILE")]
    context_config: Option<PathBuf>,
    /// Total context window in tokens, including the reserved output (default 131072).
    #[arg(long, value_name = "TOKENS", value_parser = clap::value_parser!(u64).range(1..))]
    max_context_tokens: Option<u64>,
    /// Output token reserve and maximum output per task call (default 16384).
    #[arg(long, value_name = "TOKENS", value_parser = clap::value_parser!(u64).range(1..))]
    context_reserve_tokens: Option<u64>,
    /// Automatic summary compaction (default auto).
    #[arg(long, value_name = "MODE", value_parser = ["auto", "off"])]
    context_compaction: Option<String>,
    /// Input budget ratio that triggers compaction (default 0.8).
    #[arg(long, value_name = "RATIO", value_parser = parse_context_ratio)]
    context_compact_trigger: Option<f64>,
    /// Desired input budget ratio after compaction (default 0.5).
    #[arg(long, value_name = "RATIO", value_parser = parse_context_ratio)]
    context_compact_target: Option<f64>,
    /// Do not load the default plugins (agent, bash, context, create,
    /// deepseek, edit, read, remove).
    #[arg(long)]
    no_default_plugins: bool,
    /// Append every fiber event (spawn / record / exit / cleanup / plugin, JSON lines) to this file.
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

#[derive(clap::Args)]
struct TuiArgs {
    #[command(flatten)]
    setup: Setup,
    /// Optional first task (otherwise enter a task interactively).
    prompt: Option<String>,
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

fn parse_context_ratio(text: &str) -> Result<f64, String> {
    let value: f64 = text.parse().map_err(|_| "expected a number between 0 and 1".to_string())?;
    if !value.is_finite() || value <= 0.0 || value >= 1.0 {
        return Err("expected a number between 0 and 1".into());
    }
    Ok(value)
}

fn context_policy(setup: &Setup) -> Result<Option<String>> {
    let requested = setup.context_config.is_some() || setup.max_context_tokens.is_some() || setup.context_reserve_tokens.is_some()
        || setup.context_compaction.is_some() || setup.context_compact_trigger.is_some() || setup.context_compact_target.is_some();
    if !requested {
        return Ok(None);
    }
    let mut value: Value = match &setup.context_config {
        Some(path) => serde_json::from_slice(&std::fs::read(path).with_context(|| format!("reading context config {}", path.display()))?)
            .with_context(|| format!("parsing context config {}", path.display()))?,
        None => json!({}),
    };
    let root = value.as_object_mut().context("context config must be a JSON object")?;
    if let Some(tokens) = setup.max_context_tokens {
        root.insert("max_context_tokens".into(), json!(tokens));
    }
    if let Some(tokens) = setup.context_reserve_tokens {
        root.insert("reserve_output_tokens".into(), json!(tokens));
    }
    if setup.context_compaction.is_some() || setup.context_compact_trigger.is_some() || setup.context_compact_target.is_some() {
        let compaction = root.entry("compaction").or_insert_with(|| json!({})).as_object_mut().context("compaction must be a JSON object")?;
        if let Some(mode) = &setup.context_compaction {
            compaction.insert("enabled".into(), json!(mode == "auto"));
        }
        if let Some(ratio) = setup.context_compact_trigger {
            compaction.insert("trigger_ratio".into(), json!(ratio));
        }
        if let Some(ratio) = setup.context_compact_target {
            compaction.insert("target_ratio".into(), json!(ratio));
        }
    }
    Ok(Some(serde_json::to_string(&value)?))
}

async fn kernel(setup: &Setup, tui: bool) -> Result<Kernel> {
    let integration = match setup.model.as_deref() {
        None => "deepseek",
        Some(model) if model.starts_with("deepseek-") => "deepseek",
        Some(model) if model.starts_with("mimo-") => "mimo",
        Some(model) => bail!("unsupported model integration for {model} (use a deepseek-* or mimo-* model ID)"),
    };
    if setup.mimo_provider.is_some() && integration != "mimo" {
        bail!("--mimo-provider requires a mimo-* model ID");
    }
    let mut integrations = if setup.no_default_plugins { Vec::new() } else { vec![integration] };
    for dir in &setup.plugins {
        if let Some(name @ ("deepseek" | "mimo")) = dir.file_name().and_then(|name| name.to_str()) {
            integrations.push(name);
        }
    }
    if integrations.len() > 1 {
        bail!("load one model integration: select it with --model, or use --no-default-plugins with one integration plugin");
    }
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
    let mut config = KernelConfig::new(io, cwd);
    // The host selects the approval policy. The TUI only presents requests;
    // agent tool visibility and delegation remain agent-owned defaults.
    if tui { config.env.insert("WENDY_APPROVAL_MODE".into(), "ask".into()); }
    if let Some(model) = &setup.model {
        config.env.insert("WENDY_MODEL".into(), model.clone());
    }
    if let Some(provider) = &setup.mimo_provider {
        config.env.insert("WENDY_MIMO_PROVIDER".into(), provider.clone());
    }
    if let Some(limit) = setup.agent_max_children {
        config.env.insert("WENDY_AGENT_MAX_CHILDREN".into(), limit.to_string());
    }
    if !setup.agent_report_section.is_empty() {
        config.env.insert("WENDY_AGENT_REPORT_SECTIONS".into(), serde_json::to_string(&setup.agent_report_section)?);
    }
    if let Some(policy) = context_policy(setup)? {
        config.env.insert("WENDY_CONTEXT_CONFIG".into(), policy);
    }
    let jev_directory = setup.plugins.iter().any(|path| path.file_name().and_then(|name| name.to_str()) == Some("jev"));
    if setup.jev && jev_directory { bail!("load jev once: use --jev or --plugin, not both"); }
    if setup.jev || jev_directory {
        for name in ["JEV_BASE_URL", "JEV_MODEL", "JEV_TIMEOUT_MS", "JEV_MAX_STATE_BYTES", "JEV_ALLOW_MAX_RISK", "JEV_DENY_MIN_RISK"] {
            if let Ok(value) = std::env::var(name) { config.env.insert(name.into(), value); }
        }
    }
    if let Ok(value) = std::env::var("WENDY_APPROVAL_TIMEOUT_MS") { config.env.insert("WENDY_APPROVAL_TIMEOUT_MS".into(), value); }
    let kernel = Kernel::new(config);
    if let Some(path) = &setup.record {
        kernel.add_hook(JsonlRecorder::open(path).with_context(|| format!("opening record file {}", path.display()))?);
    }
    let defaults = if setup.no_default_plugins { Vec::new() } else { PluginSource::defaults() };
    for plugin in defaults {
        let plugin = match &plugin {
            PluginSource::Embedded { name, .. } if name == "deepseek" && integration == "mimo" => {
                PluginSource::builtin("mimo").context("missing built-in MiMo integration")?
            }
            _ => plugin,
        };
        kernel.load(plugin).await.context("loading the default plugins")?;
    }
    for dir in &setup.plugins {
        kernel.load(PluginSource::Dir(dir.clone())).await.with_context(|| format!("loading plugin {}", dir.display()))?;
    }
    if (setup.jev || tui) && !setup.plugins.iter().any(|path| path.file_name().and_then(|name| name.to_str()) == Some("approval")) {
        kernel.load(PluginSource::builtin("approval").context("missing approval plugin")?).await.context("loading approval coordination")?;
    }
    if setup.jev { kernel.load(PluginSource::builtin("jev").context("missing Jev plugin")?).await.context("loading Jev")?; }
    if tui && !setup.plugins.iter().any(|path| path.file_name().and_then(|name| name.to_str()) == Some("tui")) {
        kernel.load(PluginSource::builtin("tui").context("missing TUI plugin")?).await.context("loading TUI")?;
    }
    // Every onLoad (tools registering into the context, the model choice)
    // has run before anything is called.
    kernel.ready().await.context("loading plugins")?;
    Ok(kernel)
}

async fn describe(setup: Setup) -> Result<i32> {
    let kernel = kernel(&setup, false).await?;
    println!("{}", serde_json::to_string_pretty(&kernel.plugins())?);
    Ok(0)
}

async fn describe_commands(setup: Setup) -> Result<i32> {
    // Discover the bundled TUI by default; --no-default-plugins lists only
    // explicitly selected providers without requiring agent/UI dependencies.
    let include_tui = !setup.no_default_plugins;
    let kernel = kernel(&setup, include_tui).await?;
    println!("{}", serde_json::to_string_pretty(&kernel.commands())?);
    Ok(0)
}

async fn registered(args: Vec<String>) -> Result<i32> {
    // Reuse the setup and JSON positional parser, keeping command execution
    // separate from plugin onHandle/tool dispatch.
    let parsed = match Cli::try_parse_from(["wendy".to_string(), "call".to_string()].into_iter().chain(args)) {
        Ok(parsed) => parsed,
        Err(error) if matches!(error.kind(), clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion) => {
            error.print()?;
            return Ok(0);
        }
        Err(error) => return Err(error.into()),
    };
    let Cmd::Call(args) = parsed.command else { unreachable!() };
    let input = args.input.as_deref().map(serde_json::from_str).transpose().context("command input must be JSON")?.unwrap_or_else(|| json!({}));
    let kernel = kernel(&args.setup, false).await?;
    let result = kernel.run_command(&args.plugin, input).await;
    for plugin in kernel.plugins() {
        if let Some(out) = kernel.take_output(&plugin.name).await { eprint!("{}{}", out.stdout, out.stderr); }
    }
    match result {
        Ok(output) => { println!("{}", serde_json::to_string_pretty(&output)?); Ok(0) }
        Err(error) => { eprintln!("wendy {}: {error}", args.plugin); Ok(1) }
    }
}

async fn call(args: CallArgs) -> Result<i32> {
    let input: Value = match &args.input {
        Some(text) => serde_json::from_str(text).context("input must be JSON")?,
        None => json!({}),
    };
    let kernel = kernel(&args.setup, false).await?;
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
    let kernel = kernel(&args.setup, false).await?;
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
    let kernel = kernel(&setup, false).await?;
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
    let id = message.get("id").cloned();
    let valid_id = |id: &Value| id.is_null() || id.is_string() || id.is_number();
    let response_id = id.as_ref().filter(|id| valid_id(id)).cloned().unwrap_or(Value::Null);
    if !message.is_object() || message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || id.as_ref().is_some_and(|id| !valid_id(id)) {
        return Some(rpc_error(response_id, -32600, "invalid JSON-RPC request"));
    }
    // Ignore response envelopes, but do not silently discard a malformed
    // request: a client may otherwise wait forever for its response.
    if message.get("method").is_none() && id.is_some() && (message.get("result").is_some() || message.get("error").is_some()) {
        return None;
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Some(rpc_error(response_id, -32600, "request method must be a string"));
    };
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return id.map(|id| rpc_error(id, -32602, "MCP params must be an object"));
    }
    let result: Result<Value, (i64, String)> = match method {
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
                    if !arguments.is_object() {
                        Err((-32602, "tools/call arguments must be an object".into()))
                    } else {
                        let input = if tool.wraps_input { arguments.get("input").cloned().unwrap_or(Value::Null) } else { arguments };
                        match kernel.call(name, input).await {
                            Ok(output) => Ok(tool_result(&output)),
                            Err(err) => Ok(json!({ "content": [{ "type": "text", "text": err.to_string() }], "isError": true })),
                        }
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
        Cmd::Commands(setup) => describe_commands(setup).await?,
        Cmd::Call(args) => call(args).await?,
        Cmd::Mcp(setup) => serve_mcp(setup).await?,
        Cmd::Run(args) => run(args).await?,
        Cmd::Tui(args) => {
            let kernel = kernel(&args.setup, true).await?;
            let input = args.prompt.map_or_else(|| json!({}), |prompt| json!({ "prompt": prompt }));
            match kernel.run_command("tui", input).await {
                Ok(_) => 0,
                Err(error) => { eprintln!("wendy tui: {error}"); 1 }
            }
        }
        Cmd::Registered(args) => registered(args).await?,
    };
    std::process::exit(status);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools_setup(args: &[&str]) -> Setup {
        let cli = Cli::try_parse_from(std::iter::once("wendy").chain(std::iter::once("tools")).chain(args.iter().copied())).unwrap();
        let Cmd::Tools(setup) = cli.command else { unreachable!() };
        setup
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn model_selection_loads_one_integration_and_keeps_keys_on_the_host() {
        let deepseek = kernel(&tools_setup(&[]), false).await.unwrap();
        assert_eq!(deepseek.call_ctx("currentModel", vec![]).await.unwrap()["provider"], "deepseek");

        for model in ["mimo-v2.6-pro", "mimo-v2.6-flash"] {
            let mimo = kernel(&tools_setup(&["--model", model]), false).await.unwrap();
            let profile = mimo.call_ctx("currentModel", vec![]).await.unwrap();
            assert_eq!(profile["provider"], "xiaomi-token-plan-cn");
            assert_eq!(profile["model"], model);
            assert_eq!(profile["apiKeyEnv"], "MIMO_API_KEY");
            assert_eq!(profile["apiKeyHeader"], "api-key");
            assert_eq!(profile["definition"]["id"], model);
            assert_eq!(profile["definition"]["baseUrl"], "https://token-plan-cn.xiaomimimo.com/v1");
            assert_eq!(profile["definition"]["compat"]["requiresReasoningContentOnAssistantMessages"], true);
            assert!(profile.get("apiKey").is_none());
            assert!(!mimo.plugins().iter().any(|plugin| plugin.name == "deepseek"));
            assert!(!mimo.catalog().iter().any(|tool| tool.name == "mimo"));
        }

        let public = kernel(&tools_setup(&["--model", "mimo-v2.6-pro-ultraspeed", "--mimo-provider", "xiaomi"]), false).await.unwrap();
        let profile = public.call_ctx("currentModel", vec![]).await.unwrap();
        assert_eq!(profile["definition"]["baseUrl"], "https://api.xiaomimimo.com/v1");
        assert_eq!(profile["definition"]["id"], "mimo-v2.6-pro-ultraspeed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_model_routes_and_retired_mimo_ids_fail_without_falling_back() {
        for args in [
            vec!["--model", "unknown-model"],
            vec!["--model", "deepseek-v4-flash", "--mimo-provider", "xiaomi"],
            vec!["--model", "mimo-v2.5-pro"],
            vec!["--model", "mimo-v2.6-pro-ultraspeed"],
            vec!["--plugin", "plugins/mimo"],
        ] {
            assert!(kernel(&tools_setup(&args), false).await.is_err(), "{args:?}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn context_file_and_flags_resolve_without_model_requests() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("context.json");
        std::fs::write(&path, json!({
            "max_context_tokens": 65536, "reserve_output_tokens": 8192,
            "compaction": { "trigger_ratio": 0.7, "target_ratio": 0.4, "keep_recent_turns": 2, "summary_max_tokens": 512 }
        }).to_string()).unwrap();
        let configured = kernel(&tools_setup(&[
            "--context-config", path.to_str().unwrap(), "--max-context-tokens", "32768", "--context-compact-trigger", "0.75",
        ]), false).await.unwrap();
        let policy = configured.call_ctx("contextConfig", vec![]).await.unwrap();
        assert_eq!(policy["max_context_tokens"], 32768);
        assert_eq!(policy["compaction"]["trigger_ratio"], 0.75);
        assert_eq!(policy["compaction"]["target_ratio"], 0.4);
        assert_eq!(policy["compaction"]["keep_recent_turns"], 2);
        let id = configured.call_ctx("openConversation", vec![json!("go"), json!({ "contextWindow": 24576, "maxTokens": 4096 })]).await.unwrap();
        let prepared = configured.call_ctx("prepareContext", vec![id]).await.unwrap();
        assert_eq!(prepared["budget"]["configured_context_tokens"], 32768);
        assert_eq!(prepared["budget"]["max_context_tokens"], 24576);
        assert_eq!(prepared["budget"]["reserve_output_tokens"], 4096);
        assert_eq!(prepared["budget"]["input_budget_tokens"], 20480);
        assert_eq!(prepared["budget"]["trigger_tokens"], 15360);

        let disabled = kernel(&tools_setup(&["--context-compaction", "off"]), false).await.unwrap();
        assert_eq!(disabled.call_ctx("contextConfig", vec![]).await.unwrap()["compaction"]["enabled"], false);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_context_policies_are_rejected_before_model_requests() {
        for args in [
            vec!["--max-context-tokens", "100"],
            vec!["--context-compact-trigger", "0.5", "--context-compact-target", "0.7"],
        ] {
            assert!(kernel(&tools_setup(&args), false).await.is_err(), "{args:?}");
        }
        for ratio in ["NaN", "inf", "0", "1", "-0.5"] {
            assert!(parse_context_ratio(ratio).is_err(), "{ratio}");
        }
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("context.json");
        for invalid in [
            json!({ "max_context_token": 12345 }),
            json!({ "max_context_tokens": null }),
            json!({ "compaction": { "enabled": "off" } }),
            json!({ "compaction": { "enabled": null } }),
            json!({ "compaction": { "unknown": true } }),
            json!({ "compaction": { "summary_max_tokens": 999999 } }),
        ] {
            std::fs::write(&path, invalid.to_string()).unwrap();
            assert!(kernel(&tools_setup(&["--context-config", path.to_str().unwrap()]), false).await.is_err(), "{invalid}");
        }
    }
}
