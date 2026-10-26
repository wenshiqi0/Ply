//! Agent-loop tests replace the embedded model boundary with a JSC fixture.
//! No process, network or API key is used.

use std::sync::{Arc, Mutex};
use std::os::unix::fs::PermissionsExt;

use serde_json::{Value, json};
use ply_kernel::{DiskStorage, FiberEvent, FiberHook, Io, JsonlRecorder, Kernel, KernelConfig, MountMode, PluginSource};

const FAKE_MODEL: &str = include_str!("fixtures/model.js");

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

async fn start_kernel(work: &std::path::Path, defaults: impl Fn(&str) -> bool, context_config: Option<Value>) -> (Kernel, Arc<Spawns>) {
    let root = std::fs::canonicalize(work).unwrap().display().to_string();
    let io = Io::new();
    io.mount(&root, Arc::new(DiskStorage::open(work).unwrap()), MountMode::ReadWrite).unwrap();
    let mut config = KernelConfig::new(io, root);
    if let Some(policy) = context_config {
        config.env.insert("PLY_CONTEXT_CONFIG".into(), policy.to_string());
    }
    let kernel = Kernel::new(config);
    let spawns = Arc::new(Spawns::default());
    kernel.add_hook(spawns.clone());
    for plugin in PluginSource::defaults() {
        let PluginSource::Embedded { name, .. } = &plugin else { unreachable!() };
        if defaults(name) {
            let plugin = match plugin {
                PluginSource::Embedded { name, source } if name == "agent" => {
                    let import = "import { request as modelRequest } from \"ply:models\";";
                    assert!(source.contains(import));
                    PluginSource::Embedded { name, source: source.replacen(import, FAKE_MODEL, 1) }
                }
                other => other,
            };
            kernel.load(plugin).await.unwrap();
        }
    }
    kernel.ready().await.unwrap();
    (kernel, spawns)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_loops_model_turns_and_runs_the_offered_tools_as_child_fibers() {
    let state = tempfile::tempdir().unwrap();
    // This binary has one test; keep host-private recovery writes isolated.
    std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    unsafe { std::env::set_var("PLY_CHECKPOINT_DIR", state.path()); }
    let work = tempfile::tempdir().unwrap();
    let (kernel, spawns) = start_kernel(work.path(), |_| true, None).await;
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
    assert_eq!(calls[0]["call_index"], 1);
    assert_eq!(calls[0]["response_id"], "fake-0");
    assert_eq!(calls[0]["usage"]["input_tokens"], 500);
    assert_eq!(calls[0]["usage"]["cache_hit_rate"], 0.6);
    assert_eq!(calls[1]["usage"]["input_tokens"], 1000);
    assert_eq!(calls[1]["call_index"], 2);
    let starts = spawns.records_for(&agent, "model_call_started");
    assert_eq!(starts.len(), calls.len());
    assert_eq!(starts[0]["call_index"], 1); assert_eq!(starts[1]["call_index"], 2);
    assert_eq!(calls[1]["usage"]["cache_hit_rate"], 0.95);
    assert_eq!(spawns.records_for(&agent, "agent_usage")[0]["usage"], expected_usage);
    let persisted: Vec<Value> = std::fs::read_to_string(&record).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(persisted.iter().filter(|event| event["event"] == "record" && event["id"] == agent).count(), 7);
    assert_eq!(persisted.last().unwrap()["event"], "exit");

    // The model's errors end the task; the conversation is closed either way.
    let err = kernel.call("agent", json!({ "prompt": "fail" })).await.unwrap_err();
    assert!(err.to_string().contains("deepseek/deepseek-v4-flash: rate limited"), "{err}");
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
    let delegated = kernel.call("agent", json!({ "prompt": "delegate", "max_delegation_depth": 2 })).await.unwrap();
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

    // The real agent plugin, in one kernel/realm: two long-running children
    // overlap, one starts a grandchild, and large contexts reach the model boundary.
    // Both children omit max_turns and finish after 25 replies, past the old
    // default limit of 20; explicit limits remain covered below.
    std::fs::write(work.path().join("large.txt"), "架构上下文 / quoted \"text\" / \\ path\n".repeat(400)).unwrap();
    let before = spawns.seen.lock().unwrap().len();
    let stressed = tokio::time::timeout(std::time::Duration::from_secs(60), kernel.call("agent", json!({ "prompt": "stress root", "max_turns": 4, "max_delegation_depth": 2 })))
        .await
        .expect("nested agents stalled")
        .unwrap();
    assert_eq!(stressed["text"], "both children completed");
    assert_eq!(stressed["usage"]["calls"], 2);
    assert_eq!(stressed["total_usage"]["calls"], 53);
    assert_eq!(stressed["total_usage"]["total_tokens"], 5565);
    assert_eq!(stressed["total_usage"]["cache_hit_rate"], 0.9);
    assert_eq!(stressed["total_usage"]["complete"], true);
    let tree = spawns.seen.lock().unwrap()[before..].to_vec();
    let agents: Vec<_> = tree.iter().filter(|s| s.2 == "agent").collect();
    assert_eq!(agents.len(), 4);
    let root = agents.iter().find(|s| s.1.is_none()).unwrap();
    let children: Vec<_> = agents.iter().filter(|s| s.1.as_ref() == Some(&root.0)).collect();
    assert_eq!(children.len(), 2);
    {
        let records = spawns.records.lock().unwrap();
        let first_done = records.iter().position(|(id, data)| children.iter().any(|child| id == &child.0) && data["type"] == "agent_usage").unwrap();
        for child in &children {
            let first_call = records.iter().position(|(id, data)| id == &child.0 && data["type"] == "model_call").unwrap();
            assert!(first_call < first_done, "both children must make progress before either completes");
        }
    }
    assert_eq!(agents.iter().filter(|s| children.iter().any(|child| s.1.as_ref() == Some(&child.0))).count(), 1);
    assert_eq!(tree.iter().filter(|s| s.2 == "read").count(), 48);
    for agent in agents {
        let records = spawns.records_for(&agent.0, "agent_usage");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["status"], "completed");
    }

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

    // Actual summary calls are metered alongside task calls, once per
    // ancestor. Each child independently compacts completed tool batches.
    let compact_work = tempfile::tempdir().unwrap();
    std::fs::write(compact_work.path().join("compact.txt"), "payload:xxxxxxx\n".repeat(200)).unwrap();
    let policy = json!({
        "max_context_tokens": 8192, "reserve_output_tokens": 1024,
        "compaction": { "trigger_ratio": 0.55, "target_ratio": 0.4, "keep_recent_turns": 1, "summary_max_tokens": 128 }
    });
    let (compacting, compact_events) = start_kernel(compact_work.path(), |name| ["context", "deepseek", "read", "agent"].contains(&name), Some(policy)).await;
    let compacted = compacting.call("agent", json!({ "prompt": "compact root" })).await.unwrap();
    assert_eq!(compacted["text"], "compacting children completed");
    let events = compact_events.records.lock().unwrap().clone();
    let model_calls: Vec<_> = events.iter().filter(|(_, d)| d["type"] == "model_call").collect();
    let summaries: Vec<_> = model_calls.iter().filter(|(_, d)| d["purpose"] == "compaction").collect();
    assert!(summaries.len() >= 2, "each child should summarize at least once");
    assert_eq!(model_calls.iter().filter(|(_, d)| d["purpose"] == "task").count(), 20);
    assert_eq!(compacted["total_usage"]["calls"], model_calls.len());
    assert_eq!(compacted["total_usage"]["total_tokens"], model_calls.iter().map(|(_, d)| d["usage"]["total_tokens"].as_u64().unwrap()).sum::<u64>());
    for (_, call) in &model_calls {
        assert!(call["context"]["estimated_input_tokens"].as_u64().unwrap() <= call["context"]["input_budget_tokens"].as_u64().unwrap());
    }
    let children: Vec<_> = compact_events.seen.lock().unwrap().iter().filter(|s| s.2 == "agent" && s.1.is_some()).map(|s| s.0.clone()).collect();
    assert_eq!(children.len(), 2);
    for child in children {
        let records = compact_events.records_for(&child, "context_compaction");
        assert!(!records.is_empty());
        assert!(records.iter().all(|d| d["status"] == "completed" && d["after_tokens"].as_u64().unwrap() < d["before_tokens"].as_u64().unwrap()));
        for (index, call) in compact_events.records_for(&child, "model_call").iter().enumerate() {
            assert_eq!(call["call_index"], index + 1);
        }
    }
    let err = compacting.call("agent", json!({ "prompt": "compact fail" })).await.unwrap_err();
    assert!(err.to_string().contains("summary unavailable"), "{err}");
    let failed_id = compact_events.seen.lock().unwrap().iter().rev().find(|s| s.2 == "agent").unwrap().0.clone();
    assert_eq!(compact_events.records_for(&failed_id, "context_compaction")[0]["status"], "failed");
    assert_eq!(compact_events.records_for(&failed_id, "agent_usage")[0]["status"], "failed");
    assert!(compact_events.records_for(&failed_id, "model_call").iter().any(|d| d["purpose"] == "compaction" && d["stop_reason"] == "error" && !d["usage"].is_null()));
    for (task, expected) in [
        ("compact truncated", "summary stopped with length"),
        ("compact tool", "compaction summary requested tools"),
        ("compact empty", "compaction returned an empty summary"),
    ] {
        let err = compacting.call("agent", json!({ "prompt": task })).await.unwrap_err();
        assert!(err.to_string().contains(expected), "{task}: {err}");
        let failed_id = compact_events.seen.lock().unwrap().iter().rev().find(|s| s.2 == "agent").unwrap().0.clone();
        assert_eq!(compact_events.records_for(&failed_id, "context_compaction")[0]["status"], "failed");
        let task_calls = compact_events.records_for(&failed_id, "model_call").iter().filter(|d| d["purpose"] == "task").count();
        let reads = compact_events.seen.lock().unwrap().iter().filter(|s| s.2 == "read" && s.1.as_ref() == Some(&failed_id)).count();
        assert_eq!(reads, task_calls, "a summary tool call must never execute");
    }
    let err = compacting.call("agent", json!({ "prompt": "x".repeat(30000) })).await.unwrap_err();
    assert!(err.to_string().contains("context input estimate"), "{err}");
    let oversized_id = compact_events.seen.lock().unwrap().iter().rev().find(|s| s.2 == "agent").unwrap().0.clone();
    assert!(compact_events.records_for(&oversized_id, "model_call").is_empty(), "oversized first input must be rejected before an API call");
    assert_eq!(compact_events.records_for(&oversized_id, "agent_usage")[0]["status"], "failed");
    let incomplete_usage = compacting.call("agent", json!({ "prompt": "compact missing" })).await.unwrap();
    assert_eq!(incomplete_usage["text"], "done compact missing");
    assert_eq!(incomplete_usage["usage"]["complete"], false);
    assert!(incomplete_usage["usage"]["missing_calls"].as_u64().unwrap() > 0);
    assert_eq!(incomplete_usage["usage"]["cache_hit_rate"], Value::Null);

    let soft_policy = json!({ "max_context_tokens": 8192, "reserve_output_tokens": 1024,
        "compaction": { "trigger_ratio": 0.2, "target_ratio": 0.1, "keep_recent_turns": 1, "summary_max_tokens": 64, "retry_delay_ms": 0 } });
    let (soft, soft_events) = start_kernel(compact_work.path(), |name| ["context", "deepseek", "read", "agent"].contains(&name), Some(soft_policy)).await;
    let continued = soft.call("agent", json!({ "prompt": "compact soft", "max_delegation_depth": 0 })).await.unwrap();
    assert_eq!(continued["text"], "done compact soft");
    assert!(soft_events.records.lock().unwrap().iter().any(|(_, data)| data["type"] == "context_compaction_deferred"));

    // A transient summary failure retries with the unchanged conversation.
    let retried = compacting.call("agent", json!({ "prompt": "compact transient" })).await.unwrap();
    assert_eq!(retried["text"], "done compact transient");
    let retry_id = compact_events.seen.lock().unwrap().iter().rev().find(|s| s.2 == "agent").unwrap().0.clone();
    assert!(compact_events.records_for(&retry_id, "context_compaction").iter().any(|r| r["will_retry"] == true));

    // Resume from a completed exchange after the summary service recovers.
    let err = compacting.call("agent", json!({ "prompt": "compact recover" })).await.unwrap_err();
    assert!(err.to_string().contains("resume_id"), "{err}");
    let failed_id = compact_events.seen.lock().unwrap().iter().rev().find(|s| s.2 == "agent").unwrap().0.clone();
    let checkpoint = compact_events.records_for(&failed_id, "agent_checkpoint")[0]["resume_id"].clone();
    std::fs::write(compact_work.path().join("allow-summary.flag"), "ready").unwrap();
    let resumed = compacting.call("agent", json!({ "resume_id": checkpoint })).await.unwrap();
    assert_eq!(resumed["text"], "done compact recover");
    assert_eq!(resumed["resumed_from"], checkpoint);
    assert!(resumed["previous_usage"]["calls"].as_u64().unwrap() > 0);
    assert!(!state.path().join(format!("{}.json", checkpoint.as_str().unwrap())).exists());

    // A leaf can invent an agent toolCall, but no new fiber may be created.
    let start = compact_events.seen.lock().unwrap().len();
    let bounded = compacting.call("agent", json!({ "prompt": "policy root", "allowed_tools": ["agent", "read"] })).await.unwrap();
    let leaf: Value = serde_json::from_str(bounded["text"].as_str().unwrap()).unwrap();
    assert_eq!(leaf["tools"], json!(["read"]));
    assert_eq!(leaf["denied"], true);
    assert_eq!(compact_events.seen.lock().unwrap()[start..].iter().filter(|s| s.2 == "agent").count(), 2);
    let rejected = compacting.call("agent", json!({ "prompt": "policy escalation", "allowed_tools": ["agent", "read"] })).await.unwrap();
    let denial: Value = serde_json::from_str(rejected["text"].as_str().unwrap()).unwrap();
    assert_eq!(denial["denied"], true);

    let truncated = kernel.call("agent", json!({ "prompt": "task truncated" })).await.unwrap_err();
    assert!(truncated.to_string().contains("model response truncated"), "{truncated}");
    assert!(truncated.to_string().contains("resume with agent"));
    let truncated_id = spawns.seen.lock().unwrap().iter().rev().find(|(_, _, name)| name == "agent").unwrap().0.clone();
    let calls = spawns.records_for(&truncated_id, "model_call");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["stop_reason"], "length");
    assert_eq!(calls[0]["usage"]["total_tokens"], 110);
    assert_eq!(spawns.records_for(&truncated_id, "agent_usage")[0]["status"], "failed");

    // Report repair stays inside the original child; no B2 is spawned.
    let before_report = spawns.seen.lock().unwrap().len();
    let reviewed = kernel.call("agent", json!({ "prompt": "report root", "max_children": 2,
        "allowed_tools": ["agent", "read"], "report_sections": ["架构判断", "发现", "验证边界"] })).await.unwrap();
    assert_eq!(reviewed["delegation"]["started"], 1);
    assert_eq!(reviewed["total_usage"]["calls"], 4);
    assert!(!reviewed["text"].as_str().unwrap().contains("PLY_REPORT_COMPLETE"));
    let report_agents: Vec<_> = spawns.seen.lock().unwrap()[before_report..].iter().filter(|(_, _, name)| name == "agent").cloned().collect();
    assert_eq!(report_agents.len(), 2);
    let rejected = spawns.records_for(&report_agents[1].0, "agent_report_rejected");
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0]["reasons"].as_array().unwrap().contains(&json!("residual_protocol_tokens")));
    assert_eq!(spawns.records_for(&report_agents[1].0, "agent_report_validated").len(), 1);

    // A truncated final report is repaired after resume; validation state survives.
    let error = kernel.call("agent", json!({ "prompt": "report length", "allowed_tools": ["read"],
        "report_sections": ["Findings"], "max_turns": 1 })).await.unwrap_err();
    assert!(error.to_string().contains("no answer after 1 model turns"));
    let rejected_id = spawns.seen.lock().unwrap().iter().rev().find(|(_, _, name)| name == "agent").unwrap().0.clone();
    let reasons = spawns.records_for(&rejected_id, "agent_report_rejected")[0]["reasons"].clone();
    for reason in ["truncated_report", "unclosed_code_fence", "missing_completion_marker"] {
        assert!(reasons.as_array().unwrap().contains(&json!(reason)));
    }
    let checkpoint = spawns.records_for(&rejected_id, "agent_checkpoint")[0]["resume_id"].clone();
    let repaired = kernel.call("agent", json!({ "resume_id": checkpoint })).await.unwrap();
    assert_eq!(repaired["text"], "## Findings\nThe complete report fits.");
    assert_eq!(repaired["report_validation"], json!({ "required_sections": ["Findings"], "rejected_reports": 1 }));

    // A fresh invocation restoring the checkpoint cannot reset spent slots.
    let quota_work = tempfile::tempdir().unwrap();
    let (quota, quota_events) = start_kernel(quota_work.path(), |_| true, None).await;
    let error = quota.call("agent", json!({ "prompt": "quota resume", "max_children": 2 })).await.unwrap_err();
    assert!(error.to_string().contains("pause after spending both slots"));
    let root_id = quota_events.seen.lock().unwrap().iter().find(|(_, parent, plugin)| parent.is_none() && plugin == "agent").unwrap().0.clone();
    let checkpoint = quota_events.records_for(&root_id, "agent_checkpoint")[0]["resume_id"].clone();
    let saved: Value = serde_json::from_slice(&std::fs::read(state.path().join(format!("{}.json", checkpoint.as_str().unwrap()))).unwrap()).unwrap();
    assert_eq!(saved["format"], 4);
    assert_eq!(saved["children_started"], 2);
    // Old snapshots are explicitly rejected before any model request; no migration/reset.
    let snapshot_path = state.path().join(format!("{}.json", checkpoint.as_str().unwrap()));
    let mut legacy = saved.clone(); legacy["format"] = json!(1);
    std::fs::write(&snapshot_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let error = quota.call("agent", json!({ "resume_id": checkpoint })).await.unwrap_err();
    assert!(error.to_string().contains("unsupported recovery format: expected 4"));
    let rejected_id = quota_events.seen.lock().unwrap().iter().rev().find(|(_, _, plugin)| plugin == "agent").unwrap().0.clone();
    assert!(quota_events.records_for(&rejected_id, "model_call_started").is_empty());
    assert!(snapshot_path.exists());
    std::fs::write(&snapshot_path, serde_json::to_vec(&saved).unwrap()).unwrap();
    std::fs::write(quota_work.path().join("quota-allow.flag"), "ready").unwrap();
    let before_resume = quota_events.seen.lock().unwrap().len();
    let restored = quota.call("agent", json!({ "resume_id": checkpoint })).await.unwrap();
    assert_eq!(restored["text"], "quota preserved");
    assert_eq!(restored["delegation"], json!({ "limit": 2, "started": 2, "remaining": 0 }));
    assert_eq!(quota_events.seen.lock().unwrap()[before_resume..].iter().filter(|(_, _, plugin)| plugin == "agent").count(), 1);

    // Model-issued bash tool calls use Ply sh and await the plugin gate.
    kernel.load(PluginSource::Embedded { name: "model_guard".into(), source: r#"import plugins from "ply:plugins";

import * as events from "ply:events";
export async function onLoad() {
  await plugins.bash.loaded;
  await events.on("bash.beforeExecute", event => ({ allow: !event.data.command.includes("__deny_demo__"), reason: "model hook" }));
}
"#.into() }).await.unwrap();
    kernel.ready().await.unwrap();
    let before = spawns.seen.lock().unwrap().len();
    let output = kernel.call("agent", json!({ "prompt": "bash hook", "allowed_tools": ["bash"], "max_delegation_depth": 0 })).await.unwrap();
    assert_eq!(output["text"], "ply sh hook works");
    assert_eq!(output["usage"]["calls"], 2);
    assert_eq!(output["tool_calls"], 2);
    assert!(!work.path().join("blocked-model-marker").exists());
    let graph = spawns.seen.lock().unwrap()[before..].to_vec();
    let agent_id = &graph.iter().find(|(_, _, name)| name == "agent").unwrap().0;
    let bash_ids: Vec<_> = graph.iter().filter(|(_, parent, name)| name == "bash" && parent.as_ref() == Some(agent_id)).map(|(id, _, _)| id.clone()).collect();
    assert_eq!(bash_ids.len(), 2);
    let callbacks: Vec<_> = graph.iter().filter(|(_, _, name)| name == "model_guard").collect();
    assert_eq!(callbacks.len(), 2);
    assert!(callbacks.iter().all(|(_, parent, _)| bash_ids.contains(parent.as_ref().unwrap())));

    // Without a model integration there is nothing to call.
    let bare = tempfile::tempdir().unwrap();
    let (kernel, _) = start_kernel(bare.path(), |name| name != "deepseek", None).await;
    let err = kernel.call("agent", json!({ "prompt": "anything" })).await.unwrap_err();
    assert_eq!(err.to_string(), "Error: no model in the context: load a model integration such as deepseek or mimo");

}
