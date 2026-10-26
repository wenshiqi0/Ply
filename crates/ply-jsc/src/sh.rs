//! Ply sh for the model-visible bash plugin, with direct host executable lookup.
//! Parsing stays internal; no system shell fallback or startup-file loading.
use crate::runtime::OpResult;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use std::sync::Arc;
use ply_libc::{EntryKind, Fs};
use ply_sh::{Limits, Registry, Shell};

pub(crate) async fn execute(fs: Fs, args: Value) -> OpResult {
    let invalid = |message: &str| OpResult::error("ERR_PLY_SH", message);
    let Some(command) = args
        .get("command")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return invalid("command must be a non-empty string");
    };
    if command.len() > 1 << 20 {
        return invalid("shell script exceeds 1 MiB");
    }
    let timeout_ms = match args.get("timeout_ms") {
        None => 120_000,
        Some(value) => match value
            .as_u64()
            .filter(|v| *v > 0 && *v <= 9_007_199_254_740_991)
        {
            Some(value) => value,
            None => return invalid("timeout_ms must be a positive safe integer"),
        },
    };
    let budget = Duration::from_millis(timeout_ms);
    let started = Instant::now();
    let cwd = match args.get("cwd") {
        None => fs.cwd().to_string(),
        Some(value) => match value.as_str() {
            Some(path) => fs.resolve(path),
            None => return invalid("cwd must be a string"),
        },
    };
    match tokio::time::timeout(budget, fs.stat(&cwd)).await {
        Ok(Ok(entry)) if entry.kind == EntryKind::Dir => {}
        Ok(Ok(_)) => return invalid("cwd is not a directory"),
        Ok(Err(error)) => return OpResult::error("ERR_PLY_SH", error.to_string()),
        Err(_) => {
            return OpResult::Json(
                json!({ "exit_code": Value::Null, "signal": Value::Null, "timed_out": true, "stdout": "", "stderr": "" }),
            );
        }
    }
    let initial_home = args.get("env").and_then(|env| env.get("HOME")).and_then(Value::as_str).map(str::to_string);
    let commands = crate::host_commands::HostCommands::new(fs.io().clone(), initial_home);
    let mut shell = Shell::with_limits(
        fs.io(),
        Registry::standard().with_external(Arc::new(commands)),
        Limits {
            max_buffer_bytes: Some(1 << 20),
            ..Limits::default()
        },
    );
    shell.set_cwd(&cwd);
    shell.set_var("PATH", &crate::host_commands::bin_path(), true);
    if let Some(env) = args.get("env").and_then(Value::as_object) {
        for (name, value) in env {
            if let Some(value) = value.as_str() {
                shell.set_var(name, value, true);
            }
        }
    }
    shell.set_var("SHELL", "ply-sh", true);
    let fds = shell.capture(Vec::new());
    let stdout = fds.out.clone();
    let stderr = fds.err.clone();
    match tokio::time::timeout(
        budget.saturating_sub(started.elapsed()),
        shell.exec_with_fds(command, fds),
    )
    .await
    {
        Ok(output) => {
            let mut error = String::from_utf8_lossy(&output.stderr).into_owned();
            if output.truncated {
                error.push_str("\nply sh: shell buffer limit exceeded (1 MiB)\n");
            }
            OpResult::Json(
                json!({ "exit_code": if output.truncated { 1 } else { output.status }, "signal": Value::Null,
                "timed_out": false, "stdout": String::from_utf8_lossy(&output.stdout), "stderr": error }),
            )
        }
        Err(_) => OpResult::Json(
            json!({ "exit_code": Value::Null, "signal": Value::Null, "timed_out": true,
            "stdout": String::from_utf8_lossy(&stdout.take()), "stderr": String::from_utf8_lossy(&stderr.take()) }),
        ),
    }
}
