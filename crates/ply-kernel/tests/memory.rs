//! Persistent-memory regressions. Added for later execution; no model endpoints.
mod common;

use ply_kernel::{DiskStorage, Io, Kernel, KernelConfig, MountMode, PluginSource};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

/// The root agent remembers, then delegates; the child tries the same.
const MODEL: &str = r#"async function modelRequest(request) {
  if (request.operation === "describe") return { contextWindow: 1_000_000, maxTokens: 16384 };
  const { messages, tools } = request.context, results = messages.filter(m => m.role === "toolResult");
  const emit = (blocks, stopReason = "stop") => ({ role: "assistant", stopReason, usage: { input: 20, output: 10, cacheRead: 0, cacheWrite: 0, totalTokens: 30 },
    content: stopReason === "stop" ? [...blocks, { type: "text", text: "\n[[PLY_REPORT_COMPLETE]]" }] : blocks });
  const call = (name, args) => emit([{ type: "toolCall", id: "memory-" + results.length, name, arguments: args }], "toolUse");
  const final = value => emit([{ type: "text", text: JSON.stringify({ offered: tools.some(t => t.name === "memory"), ...value }) }]);
  const outcome = r => ({ isError: r.isError, text: r.content[0].text });
  if (messages[0].content === "child") return results.length ? final(outcome(results[0]))
    : call("memory", { action: "remember", scope: "user", key: "delegated", content: "delegated note" });
  if (!results.length) return call("memory", { action: "remember", scope: "user", key: "owner", content: "explicit root note" });
  if (results.length === 1) return call("agent", { prompt: "child" });
  return final({ root: outcome(results[0]), child: JSON.parse(JSON.parse(results[1].content[0].text).text) });
}"#;

async fn configured(work: &Path, directory: &Path) -> Kernel {
    let cwd = work.canonicalize().unwrap().display().to_string();
    let io = Io::new();
    io.mount(
        &cwd,
        Arc::new(DiskStorage::open(work).unwrap()),
        MountMode::ReadWrite,
    )
    .unwrap();
    let mut config = KernelConfig::new(io, cwd);
    config
        .plugin_options
        .insert("memory".into(), json!({"directory":directory}));
    let kernel = Kernel::new(config);
    for plugin in PluginSource::defaults() {
        kernel.load(common::mock_model(plugin, MODEL)).await.unwrap();
    }
    kernel.ready().await.unwrap();
    kernel
}

fn request() -> Value {
    json!({"prompt":"What language should I use?","model":{"contextWindow":131072,"maxTokens":16384},"allowed_tools":["memory","read"],
        "facts":{"max_children":0,"max_delegation_depth":0,"report_sections":[],"completion_marker":"[[DONE]]"}})
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_survives_new_kernels_and_context_injection_preserves_the_prefix() {
    let work = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let directory = state.path().join("memory");
    let first = configured(work.path(), &directory).await;
    assert!(
        !directory.exists(),
        "loading the plugin must not initialize user data"
    );
    let empty = first
        .call_plugin_method("context", "openTask", vec![request()])
        .await
        .unwrap();
    let before = first
        .call_plugin_method("context", "prepareContext", vec![empty.clone()])
        .await
        .unwrap();
    first
        .call_plugin_method("context", "closeConversation", vec![empty])
        .await
        .unwrap();
    assert!(
        !directory.exists(),
        "recalling empty memory must not create files"
    );
    let saved=first.run_command("memory",json!({"action":"remember","scope":"user","key":"language","content":"Use Chinese for responses."})).await.unwrap();
    assert_eq!(saved["changed"], true);
    let one = first
        .call_plugin_method("context", "openTask", vec![request()])
        .await
        .unwrap();
    let prepared = first
        .call_plugin_method("context", "prepareContext", vec![one.clone()])
        .await
        .unwrap();
    assert!(
        prepared["context"]["messages"]
            .to_string()
            .contains("Use Chinese for responses.")
    );
    assert_eq!(
        before["context"]["systemPrompt"],
        prepared["context"]["systemPrompt"]
    );
    assert_eq!(
        before["budget"]["fixed_prefix_fingerprint"],
        prepared["budget"]["fixed_prefix_fingerprint"]
    );
    let checkpoint = first
        .call_plugin_method("context", "exportConversation", vec![one.clone()])
        .await
        .unwrap();
    first
        .call_plugin_method("context", "closeConversation", vec![one])
        .await
        .unwrap();
    drop(first);

    let second = configured(work.path(), &directory).await;
    let recalled = second
        .call(
            "memory",
            json!({"action":"recall","scope":"user","key":"language"}),
        )
        .await
        .unwrap();
    assert_eq!(
        recalled["entries"][0]["content"],
        "Use Chinese for responses."
    );
    let stale = second
        .call(
            "memory",
            json!({"action":"forget","id":saved["id"],"expected_version":99}),
        )
        .await
        .unwrap_err();
    assert!(stale.to_string().contains("expected_version"));
    let forgotten = second
        .call(
            "memory",
            json!({"action":"forget","id":saved["id"],"expected_version":1}),
        )
        .await
        .unwrap();
    assert_eq!(forgotten["changed"], true);
    let recalled = second
        .call("memory", json!({"action":"recall","scope":"user"}))
        .await
        .unwrap();
    assert_eq!(recalled["entries"], json!([]));
    let mut restore = request();
    restore.as_object_mut().unwrap().remove("prompt");
    let resumed = second
        .call_plugin_method("context", "restoreTask", vec![checkpoint, restore])
        .await
        .unwrap();
    let blocks = second
        .call_plugin_method("context", "blocks", vec![resumed.clone()])
        .await
        .unwrap();
    let memory = blocks
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["source"] == "memory" && b["key"] == "memory")
        .unwrap();
    assert_eq!(
        memory["content"],
        Value::Null,
        "forgotten memory must not become an effective reference after restore"
    );
    second
        .call_plugin_method("context", "closeConversation", vec![resumed])
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_interface_rejects_foreign_realms_and_delegated_mutations() {
    let work = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let kernel = configured(work.path(), &state.path().join("memory")).await;
    kernel
        .load(PluginSource::Embedded {
            name: "spy".into(),
            source: r#"
      import * as store from "ply:memory-store";
      export async function onHandle(input:{}) { return await store.recall({scope:"all"}); }
    "#
            .into(),
        })
        .await
        .unwrap();
    kernel.ready().await.unwrap();
    assert!(
        kernel
            .call("spy", json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("private memory")
    );
    // Write access is the agent's grant: a root agent keeps memory, a
    // delegated one is not offered it.
    let out = kernel.call("agent", json!({"prompt":"root"})).await.unwrap();
    let report: Value = serde_json::from_str(out["text"].as_str().unwrap()).unwrap();
    assert_eq!(report["offered"], true);
    assert_eq!(report["root"]["isError"], false);
    assert_eq!(serde_json::from_str::<Value>(report["root"]["text"].as_str().unwrap()).unwrap()["changed"], true);
    assert_eq!(report["child"]["offered"], false);
    assert_eq!(report["child"]["isError"], true);
    assert_eq!(report["child"]["text"], "no tool named memory");
    let recalled = kernel.call("memory", json!({"action":"recall","scope":"user"})).await.unwrap();
    let keys: Vec<&Value> = recalled["entries"].as_array().unwrap().iter().map(|e| &e["key"]).collect();
    assert_eq!(keys, [&json!("owner")]);
}

#[tokio::test(flavor = "multi_thread")]
async fn recall_pages_preserve_long_content_without_tool_result_truncation() {
    let work = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let kernel = configured(work.path(), &state.path().join("memory")).await;
    let content = "\"\\\n".repeat(4000);
    let saved = kernel
        .call(
            "memory",
            json!({"action":"remember","scope":"user","key":"long note","content":content}),
        )
        .await
        .unwrap();
    assert_eq!(saved["changed"], true);
    let mut offset = 0;
    let mut restored = String::new();
    loop {
        let out=kernel.call("memory",json!({"action":"recall","scope":"user","key":"long note","content_offset":offset,"expected_version":1})).await.unwrap();
        assert!(serde_json::to_vec(&out).unwrap().len() <= 16000);
        let entry = &out["entries"][0];
        restored.push_str(entry["content"].as_str().unwrap());
        let next = entry["next_content_offset"].as_u64().unwrap();
        if entry["has_more_content"] == false {
            break;
        }
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(restored, content);
}

#[tokio::test(flavor = "multi_thread")]
async fn key_lookup_and_id_only_forget_have_unambiguous_roles() {
    let work = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let kernel = configured(work.path(), &state.path().join("memory")).await;
    let user = kernel
        .call(
            "memory",
            json!({"action":"remember","scope":"user","key":"shared","content":"user note"}),
        )
        .await
        .unwrap();
    let project = kernel
        .call(
            "memory",
            json!({"action":"remember","scope":"project","key":"shared","content":"project note"}),
        )
        .await
        .unwrap();
    let recalled = kernel
        .call(
            "memory",
            json!({"action":"recall","scope":"user","key":" shared "}),
        )
        .await
        .unwrap();
    assert_eq!(recalled["entries"][0]["content"], "user note");
    assert!(
        kernel
            .call("memory", json!({"action":"recall","key":"shared"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("multiple scopes")
    );
    for input in [
        json!({"action":"recall","id":user["id"]}),
        json!({"action":"recall","query":"shared"}),
        json!({"action":"remember","key":"other","id":user["id"],"content":"bad"}),
        json!({"action":"forget","key":"shared","expected_version":1}),
        json!({"action":"forget","id":user["id"]}),
        json!({"action":"forget","id":user["id"],"key":"shared","expected_version":1}),
        json!({"action":"forget","scope":"user","id":user["id"],"expected_version":1}),
    ] {
        assert!(kernel.call("memory", input).await.is_err());
    }
    assert_eq!(
        kernel
            .call(
                "memory",
                json!({"action":"forget","id":project["id"],"expected_version":1})
            )
            .await
            .unwrap()["scope"],
        "project"
    );
    assert_eq!(
        kernel
            .call(
                "memory",
                json!({"action":"recall","scope":"user","key":"shared"})
            )
            .await
            .unwrap()["entries"][0]["id"],
        user["id"]
    );
    kernel
        .call(
            "memory",
            json!({"action":"forget","id":user["id"],"expected_version":1}),
        )
        .await
        .unwrap();
    let recreated = kernel
        .call(
            "memory",
            json!({"action":"remember","scope":"user","key":"shared","content":"new lifetime"}),
        )
        .await
        .unwrap();
    assert_ne!(recreated["id"], user["id"]);
    assert_eq!(
        kernel
            .call(
                "memory",
                json!({"action":"forget","id":user["id"],"expected_version":1})
            )
            .await
            .unwrap()["changed"],
        false
    );
    assert_eq!(
        kernel
            .call(
                "memory",
                json!({"action":"recall","scope":"user","key":"shared"})
            )
            .await
            .unwrap()["entries"][0]["content"],
        "new lifetime"
    );
}
