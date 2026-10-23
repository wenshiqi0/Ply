//! The agent plugin's conversation loop, with the model call replaced: a
//! fake `node` first on PATH answers the way pi-ai's `complete` would.

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use serde_json::json;
use wendy_kernel::{DiskStorage, FiberEvent, FiberHook, Io, Kernel, KernelConfig, MountMode, PluginSource};

/// Turn 1: ask for two tools (one that does not exist). Turn 2: report
/// what came back and which tools the model was offered.
const FAKE_NODE: &str = r#"#!/bin/bash
exec python3 -c '
import json, sys
req = json.load(sys.stdin)
if req["model"] == "fake/error":
    print(json.dumps({"role": "assistant", "content": [], "stopReason": "error", "errorMessage": "rate limited", "timestamp": 0}))
    sys.exit(0)
ctx = req["context"]
results = [m for m in ctx["messages"] if m["role"] == "toolResult"]
if not results:
    content = [
        {"type": "text", "text": "working"},
        {"type": "toolCall", "id": "c1", "name": "create", "arguments": {"path": "made.txt", "content": "by agent"}},
        {"type": "toolCall", "id": "c2", "name": "nope", "arguments": {}},
    ]
    print(json.dumps({"role": "assistant", "content": content, "stopReason": "toolUse", "timestamp": 0}))
else:
    seen = [[r["toolName"], r["isError"], r["content"][0]["text"][:30]] for r in results]
    text = "tools=" + ",".join(sorted(t["name"] for t in ctx["tools"])) + " results=" + json.dumps(seen)
    print(json.dumps({"role": "assistant", "content": [{"type": "text", "text": text}], "stopReason": "stop", "timestamp": 0}))
'
"#;

#[derive(Default)]
struct Spawns(Mutex<Vec<(String, Option<String>, String)>>);

impl FiberHook for Spawns {
    fn on_event(&self, event: &FiberEvent) {
        if let FiberEvent::Spawn { fiber, .. } = event {
            self.0.lock().unwrap().push((fiber.id.clone(), fiber.parent.clone(), fiber.plugin.clone()));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_loops_model_turns_and_runs_plugins_as_child_fibers() {
    let bin = tempfile::tempdir().unwrap();
    let node = bin.path().join("node");
    std::fs::write(&node, FAKE_NODE).unwrap();
    std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: the only test in this binary; nothing else reads PATH meanwhile.
    unsafe { std::env::set_var("PATH", format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default())) };

    let work = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(work.path()).unwrap().display().to_string();
    let io = Io::new();
    io.mount(&root, Arc::new(DiskStorage::open(work.path()).unwrap()), MountMode::ReadWrite).unwrap();
    let kernel = Kernel::new(KernelConfig::new(io, root));
    let spawns = Arc::new(Spawns::default());
    kernel.add_hook(spawns.clone());
    for plugin in PluginSource::defaults() {
        kernel.load(plugin).await.unwrap();
    }

    let out = kernel.call("agent", json!({ "prompt": "make a file", "model": "fake/model" })).await.unwrap();
    assert_eq!(
        out["text"],
        r#"tools=bash,create,edit,remove results=[["create", false, "{\"bytes\":8,\"overwritten\":false"], ["nope", true, "no tool named nope"]]"#
    );
    assert_eq!((out["turns"].clone(), out["tool_calls"].clone(), out["model"].clone()), (json!(2), json!(2), json!("fake/model")));
    assert_eq!(std::fs::read_to_string(work.path().join("made.txt")).unwrap(), "by agent");

    // The plugin calls are children of the agent's fiber.
    let spawns = spawns.0.lock().unwrap().clone();
    let agent = spawns.iter().find(|s| s.2 == "agent").unwrap().0.clone();
    let create = spawns.iter().find(|s| s.2 == "create").unwrap();
    assert_eq!(create.1.as_deref(), Some(agent.as_str()));

    let err = kernel.call("agent", json!({ "prompt": "anything", "model": "fake/error" })).await.unwrap_err();
    assert_eq!(err.to_string(), "Error: fake/error: rate limited");
}
