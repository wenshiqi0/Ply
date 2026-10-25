//! Binary entrypoints, including long-lived MCP framing and error responses.
use std::io::Write;
use std::process::{Command, Output, Stdio};
use serde_json::{Value, json};

fn command(work: &std::path::Path, mode: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wendy"));
    command.current_dir(work).arg(mode);
    command
}
fn call(work: &std::path::Path, tool: &str, input: Value) -> Output {
    command(work, "call").arg(tool).arg(input.to_string()).output().unwrap()
}
fn value(output: &Output) -> Value {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn cli_tools_files_scripts_and_read_only_mounts_work_end_to_end() {
    let work = tempfile::tempdir().unwrap();
    let catalog = value(&command(work.path(), "tools").output().unwrap());
    for name in ["agent", "bash", "context", "create", "deepseek", "edit", "read", "remove"] {
        assert!(catalog.as_array().unwrap().iter().any(|p| p["name"] == name));
    }
    value(&call(work.path(), "create", json!({ "path": "note.txt", "content": "one\ntwo\nthree\n" })));
    let page = value(&call(work.path(), "read", json!({ "path": "note.txt", "offset": 2, "limit": 1 })));
    assert_eq!(page["content"], "two\n");
    assert_eq!(page["total_lines"], 3);
    assert_eq!(page["has_more"], true);
    value(&call(work.path(), "edit", json!({ "path": "note.txt", "old_string": "two", "new_string": "TWO" })));
    assert!(std::fs::read_to_string(work.path().join("note.txt")).unwrap().contains("TWO"));
    let shell = value(&call(work.path(), "bash", json!({ "command": "printf wendy-smoke" })));
    assert_eq!(shell["stdout"], "wendy-smoke");
    let denied = command(work.path(), "call").args(["--mount", "disk:.:ro", "create"])
        .arg(json!({ "path": "forbidden", "content": "x" }).to_string()).output().unwrap();
    assert!(!denied.status.success());
    assert!(!work.path().join("forbidden").exists());
    let plugin = work.path().join("echo"); std::fs::create_dir(&plugin).unwrap();
    std::fs::write(plugin.join("main.ts"), "export function onHandle(input: string): string { return input.toUpperCase(); }").unwrap();
    let script = work.path().join("script.ts");
    std::fs::write(&script, "import plugins from \"wendy:plugins\";\nconst v=await plugins.echo('script'); setTimeout(()=>console.log('late timer: '+v),20);").unwrap();
    let output = command(work.path(), "run").arg("--plugin").arg(&plugin).arg(&script).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("late timer: SCRIPT"));
    value(&call(work.path(), "remove", json!({ "path": "note.txt" })));
    assert!(!work.path().join("note.txt").exists());
}

#[test]
fn mcp_replies_to_bad_requests_and_keeps_handling_valid_requests() {
    let work = tempfile::tempdir().unwrap();
    let plugin = work.path().join("echo"); std::fs::create_dir(&plugin).unwrap();
    std::fs::write(plugin.join("main.ts"), "export function onHandle(input: string): string { return input.toUpperCase(); }").unwrap();
    let requests = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "echo", "arguments": { "input": "mcp" } } }),
        json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "missing" } }),
        json!({ "jsonrpc": "2.0", "id": 5, "method": "ping" }),
        json!({ "jsonrpc": "2.0", "id": 6, "method": 42 }),
        json!({ "jsonrpc": "1.0", "id": 7, "method": "ping" }),
        json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/list", "params": 42 }),
        json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": { "name": "echo", "arguments": 42 } }),
        json!({ "jsonrpc": "2.0", "id": 10 }),
        json!({ "jsonrpc": "2.0", "id": 11, "result": {} }),
        json!({ "jsonrpc": "2.0", "id": [12], "method": "ping" }),
    ];
    let mut child = command(work.path(), "mcp").arg("--plugin").arg(plugin)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for request in requests { writeln!(stdin, "{request}").unwrap(); }
    writeln!(stdin, "{{").unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let responses: Vec<Value> = String::from_utf8(output.stdout).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    let reply = |id: u64| responses.iter().find(|response| response["id"] == id).unwrap_or_else(|| panic!("missing response for {id}; received IDs: {:?}", responses.iter().map(|r| &r["id"]).collect::<Vec<_>>()));
    assert_eq!(reply(1)["result"]["serverInfo"]["name"], "wendy");
    assert!(reply(2)["result"]["tools"].as_array().unwrap().iter().any(|tool| tool["name"] == "echo"));
    assert_eq!(reply(3)["result"]["content"][0]["text"], "MCP");
    assert_eq!(reply(4)["error"]["code"], -32602);
    assert_eq!(reply(5)["result"], json!({}));
    for id in [6, 7, 10] { assert_eq!(reply(id)["error"]["code"], -32600); }
    for id in [8, 9] { assert_eq!(reply(id)["error"]["code"], -32602); }
    assert!(!responses.iter().any(|response| response["id"] == 11));
    assert!(responses.iter().any(|response| response["id"].is_null() && response["error"]["code"] == -32600));
    assert!(responses.iter().any(|response| response["id"].is_null() && response["error"]["code"] == -32700));
    assert_eq!(responses.len(), 12);
}


#[test]
fn jev_is_opt_in_missing_key_requires_approval_and_invalid_configuration_is_rejected() {
    let work = tempfile::tempdir().unwrap();
    let plain = command(work.path(), "tools").env_remove("TYPESAFE_API_KEY").output().unwrap();
    assert!(plain.status.success());
    let plugins: Value = serde_json::from_slice(&plain.stdout).unwrap();
    assert!(!plugins.as_array().unwrap().iter().any(|plugin| plugin["name"] == "jev"));
    let isolated = |mode: &str| {
        let mut c = command(work.path(), mode);
        for key in ["JEV_BASE_URL", "JEV_MODEL", "JEV_TIMEOUT_MS", "JEV_MAX_STATE_BYTES", "JEV_ALLOW_MAX_RISK", "JEV_DENY_MIN_RISK"] { c.env_remove(key); }
        c
    };
    let missing = isolated("call").arg("--jev").arg("bash")
        .arg(json!({ "command": "echo unsafe > should-not-exist" }).to_string())
        .env_remove("TYPESAFE_API_KEY").output().unwrap();
    assert!(missing.status.success(), "{}", String::from_utf8_lossy(&missing.stderr));
    let pending: Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(pending["status"], "approval_required");
    assert_eq!(pending["approval"]["status"], "pending");
    assert_eq!(pending["approval"]["request"]["assessments"][0]["reason_codes"][0], "missing_credentials");
    assert!(!work.path().join("should-not-exist").exists());
    let invalid = isolated("tools").arg("--jev")
        .env("TYPESAFE_API_KEY", "fixture-not-a-real-key")
        .env("JEV_ALLOW_MAX_RISK", "0.9").env("JEV_DENY_MIN_RISK", "0.1").output().unwrap();
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("JEV_ALLOW_MAX_RISK"));
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains("fixture-not-a-real-key"));
}

#[test]
fn cli_dispatches_registered_top_level_commands_and_lists_providers() {
    let work = tempfile::tempdir().unwrap();
    let provider = work.path().join("greeter"); std::fs::create_dir(&provider).unwrap();
    std::fs::write(provider.join("main.ts"), r#"
import * as commands from "wendy:commands";
import plugins from "wendy:plugins";
export async function onLoad() {
  await commands.register({name:"hello",description:"Say hello",handler:async input => ({greeting:"hello "+input.name,owner:await fiber.info(),legacy:typeof globalThis.tools,names:Object.keys(plugins)})});
}
"#).unwrap();
    let list = command(work.path(), "commands").args(["--no-default-plugins", "--plugin"]).arg(&provider).output().unwrap();
    assert!(list.status.success(),"{}",String::from_utf8_lossy(&list.stderr));
    let metadata: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(metadata,json!([{"name":"hello","description":"Say hello","plugin":"greeter"}]));
    let output = command(work.path(), "hello").args(["--no-default-plugins", "--plugin"]).arg(&provider).arg("{\"name\":\"Wendy\"}").output().unwrap();
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let output: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output["greeting"],"hello Wendy");assert_eq!(output["legacy"],"undefined");
    assert_eq!(output["names"],json!(["greeter"]));assert_eq!(output["owner"]["parent"],Value::Null);
    let bundled=command(work.path(),"commands").output().unwrap();
    assert!(bundled.status.success(),"{}",String::from_utf8_lossy(&bundled.stderr));
    let bundled: Value=serde_json::from_slice(&bundled.stdout).unwrap();
    assert!(bundled.as_array().unwrap().iter().any(|c|c["name"]=="tui"&&c["plugin"]=="tui"));
    let missing=command(work.path(),"missing-command").arg("--no-default-plugins").output().unwrap();
    assert!(!missing.status.success());assert!(String::from_utf8_lossy(&missing.stderr).contains("no registered command"));
    let help=command(work.path(),"hello").arg("--help").output().unwrap();
    assert!(help.status.success());assert!(String::from_utf8_lossy(&help.stdout).contains("Usage:"));
}
