//! The agent plugin's loop, with the model call replaced: a fake `node`
//! first on PATH answers the way pi-ai's `complete` would.

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wendy_kernel::{DiskStorage, FiberEvent, FiberHook, Io, JsonlRecorder, Kernel, KernelConfig, MountMode, PluginSource};

/// Turn 1: ask for two tools (one that does not exist). Turn 2: report
/// what came back, which tools were offered, and the profile it was given.
const FAKE_NODE: &str = r#"#!/bin/bash
exec python3 -c '
import json, sys
req = json.load(sys.stdin)
ctx = req["context"]
task = ctx["messages"][0]["content"]
results = [m for m in ctx["messages"] if m["role"] == "toolResult"]
def usage(miss, output, read, write=0, reasoning=0):
    return {"input": miss, "output": output, "cacheRead": read, "cacheWrite": write, "reasoning": reasoning, "totalTokens": miss + output + read + write}
def emit(content, tokens=None, stop="stop", error=None):
    reply = {"role": "assistant", "content": content, "stopReason": stop, "responseId": "fake-" + str(len(results))}
    if tokens is not None:
        reply["usage"] = tokens
    if error is not None:
        reply["errorMessage"] = error
    print(json.dumps(reply))
if task == "transport-fail":
    sys.stderr.write("transport unavailable")
    sys.exit(7)
if task == "fail":
    emit([], usage(5, 0, 0), "error", "rate limited")
    sys.exit(0)
if task == "missing-usage":
    emit([{"type": "text", "text": "no usage"}])
    sys.exit(0)
if task == "zero-usage":
    emit([{"type": "text", "text": "placeholder usage"}], usage(0, 0, 0))
    sys.exit(0)
if task == "delegate-missing":
    if not results:
        emit([{"type": "toolCall", "id": "missing", "name": "agent", "arguments": {"prompt": "missing-usage"}}], usage(10, 5, 90), "toolUse")
    else:
        emit([{"type": "text", "text": "done"}], usage(10, 5, 90))
    sys.exit(0)
if task in ["delegate", "child branch", "grandchild", "child fail"]:
    tokens = usage(10, 5, 90, reasoning=2)
    if task == "child fail":
        emit([], tokens, "error", "child failed after reporting usage")
    elif task == "delegate" and not results:
        emit([
            {"type": "toolCall", "id": "child1", "name": "agent", "arguments": {"prompt": "child branch"}},
            {"type": "toolCall", "id": "child2", "name": "agent", "arguments": {"prompt": "child fail"}},
        ], tokens, "toolUse")
    elif task == "child branch" and not results:
        emit([{"type": "toolCall", "id": "grandchild", "name": "agent", "arguments": {"prompt": "grandchild"}}], tokens, "toolUse")
    else:
        emit([{"type": "text", "text": "done " + task}], tokens)
    sys.exit(0)
if not results:
    content = [
        {"type": "text", "text": "working"},
        {"type": "toolCall", "id": "c1", "name": "create", "arguments": {"path": "made.txt", "content": "by agent"}},
        {"type": "toolCall", "id": "c2", "name": "nope", "arguments": {}},
    ]
    emit(content, usage(100, 20, 300, 100, 5), "toolUse")
else:
    seen = [[r["toolName"], r["isError"], r["content"][0]["text"][:30]] for r in results]
    text = "tools=" + ",".join(sorted(t["name"] for t in ctx["tools"])) + " key=" + req["profile"]["apiKeyEnv"] + " results=" + json.dumps(seen)
    emit([{"type": "text", "text": text}], usage(50, 30, 950, reasoning=7))
'
"#;

#[derive(Default)]
struct Spawns {
    seen: Mutex<Vec<(String, Option<String>, String)>>,
    records: Mutex<Vec<(String, Value)>>,
}

impl FiberHook for Spawns {
    fn on_event(&self, event: &FiberEvent) {
        match event {
            FiberEvent::Spawn { fiber, .. } => {
                self.seen.lock().unwrap().push((fiber.id.clone(), fiber.parent.clone(), fiber.plugin.clone()));
            }
            FiberEvent::Record { id, data, .. } => self.records.lock().unwrap().push((id.clone(), data.clone())),
            _ => {}
        }
    }
}

impl Spawns {
    fn records_for(&self, id: &str, kind: &str) -> Vec<Value> {
        self.records.lock().unwrap().iter().filter(|(owner, data)| owner == id && data["type"] == kind).map(|(_, data)| data.clone()).collect()
    }
}

async fn start_kernel(work: &std::path::Path, defaults: impl Fn(&str) -> bool) -> (Kernel, Arc<Spawns>) {
    let root = std::fs::canonicalize(work).unwrap().display().to_string();
    let io = Io::new();
    io.mount(&root, Arc::new(DiskStorage::open(work).unwrap()), MountMode::ReadWrite).unwrap();
    let kernel = Kernel::new(KernelConfig::new(io, root));
    let spawns = Arc::new(Spawns::default());
    kernel.add_hook(spawns.clone());
    for plugin in PluginSource::defaults() {
        let PluginSource::Embedded { name, .. } = &plugin else { unreachable!() };
        if defaults(name) {
            kernel.load(plugin).await.unwrap();
        }
    }
    kernel.ready().await.unwrap();
    (kernel, spawns)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_loops_model_turns_and_runs_the_offered_tools_as_child_fibers() {
    let bin = tempfile::tempdir().unwrap();
    let node = bin.path().join("node");
    std::fs::write(&node, FAKE_NODE).unwrap();
    std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: the only test in this binary; nothing else reads PATH meanwhile.
    unsafe { std::env::set_var("PATH", format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default())) };

    let work = tempfile::tempdir().unwrap();
    let (kernel, spawns) = start_kernel(work.path(), |_| true).await;
    let record = work.path().join("fibers.jsonl");
    kernel.add_hook(JsonlRecorder::open(&record).unwrap());
    let out = kernel.call("agent", json!({ "prompt": "make a file" })).await.unwrap();
    assert_eq!(
        out["text"],
        r#"tools=agent,bash,create,edit,read,remove key=DEEPSEEK_API_KEY results=[["create", false, "{\"bytes\":8,\"overwritten\":false"], ["nope", true, "no tool named nope"]]"#
    );
    assert_eq!(
        (out["turns"].clone(), out["tool_calls"].clone(), out["model"].clone()),
        (json!(2), json!(2), json!("deepseek/deepseek-v4-flash"))
    );
    assert_eq!(std::fs::read_to_string(work.path().join("made.txt")).unwrap(), "by agent");
    let expected_usage = json!({
        "calls": 2, "reported_calls": 2, "missing_calls": 0, "complete": true,
        "input_tokens": 1500, "uncached_input_tokens": 150, "output_tokens": 50,
        "cache_read_tokens": 1250, "cache_write_tokens": 100, "reasoning_tokens": 12,
        "total_tokens": 1550, "cache_hit_rate": 1250.0 / 1500.0,
    });
    assert_eq!(out["usage"], expected_usage);
    assert_eq!(out["total_usage"], expected_usage);

    // The tool calls are children of the agent's fiber.
    let spawned = spawns.seen.lock().unwrap().clone();
    let agent = spawned.iter().find(|s| s.2 == "agent").unwrap().0.clone();
    let create = spawned.iter().find(|s| s.2 == "create").unwrap();
    assert_eq!(create.1.as_deref(), Some(agent.as_str()));
    let calls = spawns.records_for(&agent, "model_call");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["turn"], 1);
    assert_eq!(calls[0]["response_id"], "fake-0");
    assert_eq!(calls[0]["usage"]["input_tokens"], 500);
    assert_eq!(calls[0]["usage"]["cache_hit_rate"], 0.6);
    assert_eq!(calls[1]["usage"]["input_tokens"], 1000);
    assert_eq!(calls[1]["usage"]["cache_hit_rate"], 0.95);
    assert_eq!(spawns.records_for(&agent, "agent_usage")[0]["usage"], expected_usage);
    let persisted: Vec<Value> = std::fs::read_to_string(&record).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(persisted.iter().filter(|event| event["event"] == "record" && event["id"] == agent).count(), 3);
    assert_eq!(persisted.last().unwrap()["event"], "exit");

    // The model's errors end the task; the conversation is closed either way.
    let err = kernel.call("agent", json!({ "prompt": "fail" })).await.unwrap_err();
    assert_eq!(err.to_string(), "Error: deepseek/deepseek-v4-flash: rate limited");
    let failed = spawns.seen.lock().unwrap().last().unwrap().0.clone();
    let failed_usage = spawns.records_for(&failed, "agent_usage");
    assert_eq!(failed_usage[0]["status"], "failed");
    assert_eq!(failed_usage[0]["usage"]["input_tokens"], 5);
    assert_eq!(failed_usage[0]["usage"]["cache_hit_rate"], 0.0);
    let fresh = kernel.call_ctx("openConversation", vec![json!("")]).await.unwrap();
    assert_eq!(fresh, json!("conversation-3"));
    let err = kernel.call_ctx("appendMessages", vec![json!("conversation-1"), json!([])]).await.unwrap_err();
    assert!(err.to_string().contains("no open conversation"), "{err}");

    // Parallel children, a grandchild, and a reported failure contribute
    // once each to the root, without being added again from tool results.
    let delegated = kernel.call("agent", json!({ "prompt": "delegate" })).await.unwrap();
    assert_eq!(delegated["usage"]["calls"], 2);
    assert_eq!(delegated["total_usage"]["calls"], 6);
    assert_eq!(delegated["total_usage"]["input_tokens"], 600);
    assert_eq!(delegated["total_usage"]["output_tokens"], 30);
    assert_eq!(delegated["total_usage"]["total_tokens"], 630);
    assert_eq!(delegated["total_usage"]["cache_hit_rate"], 0.9);
    assert_eq!(delegated["total_usage"]["complete"], true);
    let partial = kernel.call("agent", json!({ "prompt": "delegate-missing" })).await.unwrap();
    assert_eq!(partial["usage"]["complete"], true);
    assert_eq!(partial["usage"]["cache_hit_rate"], 0.9);
    assert_eq!(partial["total_usage"]["calls"], 3);
    assert_eq!(partial["total_usage"]["reported_calls"], 2);
    assert_eq!(partial["total_usage"]["missing_calls"], 1);
    assert_eq!(partial["total_usage"]["complete"], false);
    assert_eq!(partial["total_usage"]["cache_hit_rate"], Value::Null);

    // Missing data is not zero usage or a measured zero cache hit rate.
    for prompt in ["missing-usage", "zero-usage"] {
        let missing = kernel.call("agent", json!({ "prompt": prompt })).await.unwrap();
        assert_eq!(missing["usage"]["calls"], 1);
        assert_eq!(missing["usage"]["reported_calls"], 0);
        assert_eq!(missing["usage"]["missing_calls"], 1);
        assert_eq!(missing["usage"]["complete"], false);
        assert_eq!(missing["usage"]["cache_hit_rate"], Value::Null);
        assert_eq!(missing["usage"]["reasoning_tokens"], Value::Null);
    }
    let err = kernel.call("agent", json!({ "prompt": "transport-fail" })).await.unwrap_err();
    assert!(err.to_string().contains("transport unavailable"), "{err}");
    let failed = spawns.seen.lock().unwrap().last().unwrap().0.clone();
    let failed_calls = spawns.records_for(&failed, "model_call");
    assert_eq!(failed_calls.len(), 1);
    assert_eq!(failed_calls[0]["usage"], Value::Null);
    assert_eq!(spawns.records_for(&failed, "agent_usage")[0]["total_usage"]["complete"], false);

    let err = kernel.call("agent", json!({ "prompt": "make a file", "max_turns": 1 })).await.unwrap_err();
    assert!(err.to_string().contains("no answer after 1 model turns"), "{err}");
    let capped = spawns.seen.lock().unwrap().iter().rev().find(|s| s.2 == "agent").unwrap().0.clone();
    let capped_usage = spawns.records_for(&capped, "agent_usage");
    assert_eq!(capped_usage[0]["status"], "failed");
    assert_eq!(capped_usage[0]["usage"]["calls"], 1);
    assert_eq!(capped_usage[0]["usage"]["input_tokens"], 500);

    // Without a model integration there is nothing to call.
    let bare = tempfile::tempdir().unwrap();
    let (kernel, _) = start_kernel(bare.path(), |name| name != "deepseek").await;
    let err = kernel.call("agent", json!({ "prompt": "anything" })).await.unwrap_err();
    assert_eq!(err.to_string(), "Error: no model in the context: load a model integration such as deepseek");
}
