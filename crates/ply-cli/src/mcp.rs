use super::*;

// ------------------------------------------------------------------- mcp

/// One JSON-RPC message per line on stdin/stdout. Requests are handled
/// concurrently; stdout carries nothing but protocol messages.
pub(super) async fn serve_mcp(setup: Setup) -> Result<i32> {
    let kernel = kernel(&setup, UiMode::Disabled).await?;
    let out = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let enabled = setup.host_chat;
    let mut tasks = tokio::task::JoinSet::new();
    let mut pending = std::collections::HashMap::<String, tokio::task::AbortHandle>::new();
    loop {
        let line = tokio::select! {
            line = lines.next_line() => match line? { Some(line) => line, None => break },
            done = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Ok((id, response))) = done {
                    if let Some(id) = id { pending.remove(&id); }
                    if let Some(response) = response { send(&out, response).await; }
                }
                continue;
            }
        };
        if line.trim().is_empty() { continue; }
        let message = serde_json::from_str::<Value>(&line);
        if let Ok(value) = &message {
            if value["jsonrpc"] == "2.0" && value["method"] == "notifications/cancelled" && value.get("id").is_none() && value["params"].is_object() {
                if let Some(id) = value["params"].get("requestId") {
                    if let Some(task) = pending.remove(&id.to_string()) {
                        if !task.is_finished() {
                            task.abort();
                            send(&out, rpc_error(id.clone(), -32800, "request cancelled")).await;
                        }
                    }
                }
                continue;
            }
        }
        let id = message.as_ref().ok().and_then(|v| v.get("id")).map(Value::to_string);
        if id.as_ref().is_some_and(|id| pending.contains_key(id)) {
            send(&out, rpc_error(message.unwrap()["id"].clone(), -32600, "request id already running")).await;
            continue;
        }
        let (kernel, task_id) = (kernel.clone(), id.clone());
        let task = tasks.spawn(async move {
            let response = match message {
                Ok(message) => handle_mcp(&kernel, message, enabled).await,
                Err(_) => Some(rpc_error(Value::Null, -32700, "parse error")),
            };
            (task_id, response)
        });
        if let Some(id) = id { pending.insert(id, task); }
    }
    while let Some(done) = tasks.join_next().await {
        if let Ok((id, response)) = done {
            if let Some(id) = id { pending.remove(&id); }
            if let Some(response) = response { send(&out, response).await; }
        }
    }
    kernel.close().await;
    Ok(0)
}

async fn send(out: &tokio::sync::Mutex<tokio::io::Stdout>, response: Value) {
    let mut out = out.lock().await;
    let _ = out.write_all(format!("{response}\n").as_bytes()).await;
    let _ = out.flush().await;
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

async fn handle_mcp(kernel: &Kernel, message: Value, host_chat: bool) -> Option<Value> {
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
            "capabilities": { "tools": { "listChanged": false }, "experimental": { "plyAgent": if host_chat { 1 } else { 0 } } },
            "serverInfo": { "name": "ply", "version": env!("CARGO_PKG_VERSION") },
        })),
        "ply/agent/catalog" | "ply/agent/chat" | "ply/agent/state" | "ply/agent/approve" if host_chat => {
            let (export, args) = match method {
                "ply/agent/catalog" => ("modelCatalog", vec![]),
                "ply/agent/chat" => ("hostChat", vec![params]),
                "ply/agent/state" => ("hostState", vec![]),
                _ => ("hostApprove", vec![params]),
            };
            kernel.call_plugin_method("agent", export, args).await.map_err(|_| (-32000, "host agent request failed".into()))
        },
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

