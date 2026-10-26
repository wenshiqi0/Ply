//! Context policy, compaction boundaries, and rollback without model calls.
use serde_json::{Value, json};
use ply_kernel::{Io, Kernel, KernelConfig, PluginSource};

fn policy(enabled: bool) -> Value {
    json!({
        "max_context_tokens": 4096, "reserve_output_tokens": 512,
        "compaction": {
            "enabled": enabled, "trigger_ratio": 0.7, "target_ratio": 0.4,
            "keep_recent_turns": 1, "summary_max_tokens": 64
        }
    })
}

async fn context(settings: Value) -> Kernel {
    let mut config = KernelConfig::new(Io::new(), "/");
    config.env.insert("PLY_CONTEXT_CONFIG".into(), settings.to_string());
    let kernel = Kernel::new(config);
    kernel.load(PluginSource::builtin("context").unwrap()).await.unwrap();
    kernel.ready().await.unwrap();
    kernel
}

fn exchange(n: usize, size: usize) -> [Value; 2] {
    [
        json!({
            "role": "assistant",
            "content": [
                { "type": "thinking", "thinking": format!("reason-{n}") },
                { "type": "toolCall", "id": format!("call-{n}"), "name": "read", "arguments": { "path": format!("file-{n}") } }
            ],
            // Old usage must not become a valid anchor after compaction.
            "usage": { "input": 999999, "output": 20, "cacheRead": 0, "cacheWrite": 0 }
        }),
        json!({
            "role": "toolResult", "toolName": "read", "toolCallId": format!("call-{n}"),
            "content": [{ "type": "text", "text": format!("evidence-{n}:{}", "x".repeat(size)) }]
        }),
    ]
}

fn history() -> Vec<Value> {
    let mut messages = vec![json!({ "role": "user", "content": "Keep the original task and constraints exactly." })];
    for n in 0..8 {
        messages.extend(exchange(n, 1500));
    }
    messages
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_anchors_and_model_limits_are_applied() {
    let kernel = context(policy(true)).await;
    let id = kernel.call_ctx("openConversation", vec![json!("system"), json!({ "contextWindow": 3072, "maxTokens": 256 })]).await.unwrap();
    let first = kernel.call_ctx("appendMessages", vec![id.clone(), json!([{ "role": "user", "content": "task" }])]).await.unwrap();
    assert_eq!(first["messages"][0]["content"], "task");
    let prepared = kernel.call_ctx("prepareContext", vec![id.clone()]).await.unwrap();
    assert_eq!(prepared["budget"]["configured_context_tokens"], 4096);
    assert_eq!(prepared["budget"]["max_context_tokens"], 3072);
    assert_eq!(prepared["budget"]["reserve_output_tokens"], 256);
    assert_eq!(prepared["budget"]["input_budget_tokens"], 2816);
    kernel
        .call_ctx(
            "appendMessages",
            vec![
                id.clone(),
                json!([{ "role": "assistant", "content": [{ "type": "text", "text": "answer" }] }]),
                json!({ "expected_revision": 1, "context_tokens": 900, "tools_signature": prepared["tools_signature"] }),
            ],
        )
        .await
        .unwrap();
    let anchored = kernel.call_ctx("prepareContext", vec![id.clone()]).await.unwrap();
    assert_eq!(anchored["budget"]["estimated_input_tokens"], 900);
    assert_eq!(anchored["budget"]["estimation"], "reported_usage_plus_estimate");
    kernel.call_ctx("appendMessages", vec![id.clone(), json!([{ "role": "user", "content": "new evidence" }])]).await.unwrap();
    let with_tail = kernel.call_ctx("prepareContext", vec![id.clone()]).await.unwrap();
    assert!(with_tail["budget"]["estimated_input_tokens"].as_u64().unwrap() > 900);
    assert!(
        kernel
            .call_ctx("appendMessages", vec![id, json!([{ "role": "assistant", "content": [] }]), json!({ "expected_revision": 0, "context_tokens": 42 })])
            .await
            .unwrap_err()
            .to_string()
            .contains("stale context revision")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn new_tools_do_not_change_a_running_conversation_or_its_usage_anchor() {
    let kernel = context(policy(true)).await;
    let id = kernel.call_ctx("openConversation", vec![json!("system")]).await.unwrap();
    kernel.call_ctx("appendMessages", vec![id.clone(), json!([{ "role": "user", "content": "task" }])]).await.unwrap();
    let prepared = kernel.call_ctx("prepareContext", vec![id.clone()]).await.unwrap();
    kernel
        .load(PluginSource::Embedded { name: "late_tool".into(), source: "export function onHandle(input: { text: string }) { return input.text; }".into() })
        .await
        .unwrap();
    kernel.call_ctx("registerTool", vec![json!("late_tool")]).await.unwrap();
    kernel
        .call_ctx(
            "appendMessages",
            vec![
                id.clone(),
                json!([{ "role": "assistant", "content": [{ "type": "text", "text": "reply" }] }]),
                json!({ "expected_revision": prepared["revision"], "context_tokens": 900, "tools_signature": prepared["tools_signature"] }),
            ],
        )
        .await
        .unwrap();
    let refreshed = kernel.call_ctx("prepareContext", vec![id]).await.unwrap();
    assert_eq!(refreshed["budget"]["estimation"], "reported_usage_plus_estimate");
    assert_eq!(refreshed["budget"]["estimated_input_tokens"], 900);
    assert_eq!(refreshed["tools_signature"], prepared["tools_signature"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn compaction_compares_progress_with_one_estimator_instead_of_a_usage_anchor() {
    let mut settings = policy(true);
    settings["max_context_tokens"] = json!(8192);
    settings["reserve_output_tokens"] = json!(1024);
    let kernel = context(settings).await;
    let id = kernel.call_ctx("openConversation", vec![json!("fixed rules ".repeat(250))]).await.unwrap();
    let mut messages = vec![json!({"role":"user","content":"original task"})];
    for n in 0..8 { messages.extend(exchange(n, 2000)); }
    kernel.call_ctx("appendMessages", vec![id.clone(), json!(messages)]).await.unwrap();
    kernel.call_ctx("appendMessages", vec![id.clone(), json!([{"role":"assistant","content":[{"type":"text","text":"observed"}]}]),
        json!({"expected_revision":1,"context_tokens":5500,"tools_signature":"[]"})]).await.unwrap();
    let plan = kernel.call_ctx("prepareContext", vec![id.clone()]).await.unwrap();
    assert!(plan.get("compaction").is_some(), "{plan}");
    let committed = kernel.call_ctx("commitCompaction", vec![id,plan["revision"].clone(),json!("retained evidence"),json!(20)]).await.unwrap();
    assert!(committed["before_tokens"].as_u64().unwrap() > 5500);
    assert!(committed["after_tokens"].as_u64().unwrap() < committed["before_tokens"].as_u64().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn over_budget_history_can_make_bounded_compaction_progress_before_task_resume() {
    let mut settings = policy(true);
    settings["max_context_tokens"] = json!(8192);
    settings["reserve_output_tokens"] = json!(1024);
    let kernel = context(settings).await;
    let id = kernel.call_ctx("openConversation", vec![json!("fixed rules ".repeat(250))]).await.unwrap();
    let mut messages = vec![json!({"role":"user","content":"original task"})];
    for n in 0..14 { messages.extend(exchange(n, 2000)); }
    kernel.call_ctx("appendMessages", vec![id.clone(), json!(messages)]).await.unwrap();
    kernel.call_ctx("appendMessages", vec![id.clone(), json!([{"role":"assistant","content":[{"type":"text","text":"observed"}]}]),
        json!({"expected_revision":1,"context_tokens":8000,"tools_signature":"[]"})]).await.unwrap();
    let mut commits = 0;
    loop {
        let prepared = kernel.call_ctx("prepareContext", vec![id.clone()]).await.unwrap();
        if prepared.get("context").is_some() {
            assert!(prepared["budget"]["estimated_input_tokens"].as_u64().unwrap() <= 7168);
            break;
        }
        assert!(prepared["compaction"]["estimated_input_tokens"].as_u64().unwrap() <= 7168);
        let result = kernel.call_ctx("commitCompaction", vec![id.clone(),prepared["revision"].clone(),json!("retained evidence"),json!(20)]).await.unwrap();
        assert!(result["after_tokens"].as_u64().unwrap() < result["before_tokens"].as_u64().unwrap());
        commits += 1;
        assert!(commits <= 8);
    }
    assert!(commits >= 2, "a bounded summary cannot remove this history in one step");
}

#[tokio::test(flavor = "multi_thread")]
async fn calibrated_context_defers_an_unhelpful_summary_while_inside_budget() {
    let mut settings = policy(true);
    settings["compaction"]["keep_recent_turns"] = json!(2);
    let kernel = context(settings).await;
    let id = kernel.call_ctx("openConversation", vec![json!("system")]).await.unwrap();
    let mut messages = vec![json!({ "role": "user", "content": "task" })];
    for n in 0..3 {
        messages.extend(exchange(n, 500));
    }
    messages.extend(exchange(3, 6000));
    kernel.call_ctx("appendMessages", vec![id.clone(), json!(messages)]).await.unwrap();
    kernel
        .call_ctx(
            "appendMessages",
            vec![
                id.clone(),
                json!([exchange(4, 10)[0]]),
                json!({
                    "expected_revision": 1, "context_tokens": 2600, "tools_signature": "[]"
                }),
            ],
        )
        .await
        .unwrap();
    kernel.call_ctx("appendMessages", vec![id.clone(), json!([exchange(4, 10)[1]])]).await.unwrap();
    let prepared = kernel.call_ctx("prepareContext", vec![id]).await.unwrap();
    assert!(prepared.get("context").is_some(), "a still-fitting calibrated context should continue");
    assert!(prepared.get("compaction").is_none());
    assert_eq!(prepared["budget"]["compaction_deferred"], "retained_history_estimate_not_smaller");
    assert!(prepared["budget"]["estimated_input_tokens"].as_u64().unwrap() <= prepared["budget"]["input_budget_tokens"].as_u64().unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fitting_context_can_continue_when_the_summary_prompt_cannot_fit() {
    let kernel = context(policy(true)).await;
    let id = kernel.call_ctx("openConversation", vec![json!("system")]).await.unwrap();
    let pair = exchange(0, 5000);
    kernel
        .call_ctx(
            "appendMessages",
            vec![
                id.clone(),
                json!([
                    { "role": "user", "content": "t".repeat(4000) }, pair[0], pair[1]
                ]),
            ],
        )
        .await
        .unwrap();
    kernel
        .call_ctx(
            "appendMessages",
            vec![
                id.clone(),
                json!([exchange(1, 10)[0]]),
                json!({
                    "expected_revision": 1, "context_tokens": 2600, "tools_signature": "[]"
                }),
            ],
        )
        .await
        .unwrap();
    kernel.call_ctx("appendMessages", vec![id.clone(), json!([exchange(1, 10)[1]])]).await.unwrap();
    let prepared = kernel.call_ctx("prepareContext", vec![id.clone()]).await.unwrap();
    assert!(prepared.get("context").is_some());
    assert_eq!(prepared["budget"]["compaction_deferred"], "summary_exchange_exceeds_budget");
    kernel.call_ctx("appendMessages", vec![id.clone(), json!([{ "role": "user", "content": "x".repeat(20000) }])]).await.unwrap();
    assert!(kernel.call_ctx("prepareContext", vec![id]).await.unwrap_err().to_string().contains("cannot fit in a compaction request"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_inflight_summary_keeps_its_prefix_when_tools_change() {
    let mut settings = policy(true);
    settings["max_context_tokens"] = json!(8192);
    settings["reserve_output_tokens"] = json!(1024);
    let kernel = context(settings).await;
    let id = kernel.call_ctx("openConversation", vec![json!("system")]).await.unwrap();
    let mut messages = vec![json!({ "role": "user", "content": "task" })];
    for n in 0..8 {
        messages.extend(exchange(n, 1000));
    }
    kernel.call_ctx("appendMessages", vec![id.clone(), json!(messages)]).await.unwrap();
    let first = kernel.call_ctx("prepareContext", vec![id.clone()]).await.unwrap();
    assert!(first.get("compaction").is_some());
    kernel
        .load(PluginSource::Embedded {
            name: "new_tool".into(),
            source: format!("/** {} */ export function onHandle(input: {{ text: string }}) {{ return input.text; }}", "extra tool documentation ".repeat(200)),
        })
        .await
        .unwrap();
    kernel.call_ctx("registerTool", vec![json!("new_tool")]).await.unwrap();
    let second = kernel.call_ctx("prepareContext", vec![id.clone()]).await.unwrap();
    assert_eq!(first["compaction"]["messages_to_summarize"], second["compaction"]["messages_to_summarize"]);
    assert_eq!(first["compaction"]["context"]["messages"][0]["content"], second["compaction"]["context"]["messages"][0]["content"]);
    let committed = kernel.call_ctx("commitCompaction", vec![id, first["revision"].clone(), json!("task memory"), json!(10)]).await.unwrap();
    assert_eq!(committed["messages_summarized"], first["compaction"]["messages_to_summarize"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_history_is_summarized_in_bounded_complete_chunks() {
    let kernel = context(policy(true)).await;
    let original = history();
    let id = kernel.call_ctx("openConversation", vec![json!("system")]).await.unwrap();
    kernel.call_ctx("appendMessages", vec![id.clone(), json!(original)]).await.unwrap();
    let mut previous = None;
    let mut applied = 0;
    let final_context = loop {
        let prepared = kernel.call_ctx("prepareContext", vec![id.clone()]).await.unwrap();
        if prepared.get("compaction").is_none() {
            break prepared;
        }
        assert!(prepared["compaction"]["estimated_input_tokens"].as_u64().unwrap() <= prepared["budget"]["input_budget_tokens"].as_u64().unwrap());
        assert_eq!(prepared["compaction"]["context"]["tools"], json!([]));
        let payload: Value = serde_json::from_str(prepared["compaction"]["context"]["messages"][0]["content"].as_str().unwrap()).unwrap();
        assert_eq!(payload["original_task"], original[0]["content"]);
        if let Some(previous) = &previous {
            assert_eq!(payload["previous_summary"], *previous);
        }
        let summary = format!("memory-{applied}: task, evidence and remaining work");
        let result = kernel.call_ctx("commitCompaction", vec![id.clone(), prepared["revision"].clone(), json!(summary), json!(20)]).await.unwrap();
        assert!(result["after_tokens"].as_u64().unwrap() < result["before_tokens"].as_u64().unwrap());
        previous = Some(json!(summary));
        applied += 1;
        assert!(applied < 10, "each summary must remove a complete exchange");
    };
    assert!(applied >= 2, "the oversized history should require multiple bounded summaries");
    assert!(final_context["budget"]["estimated_input_tokens"].as_u64().unwrap() <= final_context["budget"]["target_tokens"].as_u64().unwrap());
    assert_eq!(final_context["budget"]["estimation"], "utf8_bytes_estimate");
    let messages = final_context["context"]["messages"].as_array().unwrap();
    assert_eq!(messages[0], original[0]);
    assert!(messages[1]["content"].as_str().unwrap().contains("context_summary"));
    assert_eq!(messages.last().unwrap(), original.last().unwrap());
    let mut pending = std::collections::HashSet::new();
    for message in messages {
        if message["role"] == "assistant" {
            for block in message["content"].as_array().unwrap() {
                if block["type"] == "toolCall" {
                    pending.insert(block["id"].as_str().unwrap());
                }
            }
        } else if message["role"] == "toolResult" {
            assert!(pending.remove(message["toolCallId"].as_str().unwrap()), "orphan result");
        }
    }
    assert!(pending.is_empty());
    let recent = messages.iter().find(|m| m["role"] == "assistant").unwrap();
    assert_eq!(recent["content"][0]["type"], "thinking", "retained assistant reasoning must remain intact");
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_or_stale_summaries_do_not_replace_history() {
    let kernel = context(policy(true)).await;
    let original = history();
    let id = kernel.call_ctx("openConversation", vec![json!("system")]).await.unwrap();
    kernel.call_ctx("appendMessages", vec![id.clone(), json!(original)]).await.unwrap();
    let prepared = kernel.call_ctx("prepareContext", vec![id.clone()]).await.unwrap();
    let revision = prepared["revision"].clone();
    assert!(kernel.call_ctx("commitCompaction", vec![id.clone(), revision.clone(), json!("")]).await.unwrap_err().to_string().contains("empty summary"));
    assert!(
        kernel
            .call_ctx("commitCompaction", vec![id.clone(), revision.clone(), json!("x".repeat(30000))])
            .await
            .unwrap_err()
            .to_string()
            .contains("did not reduce")
    );
    let unchanged = kernel.call_ctx("appendMessages", vec![id.clone(), json!([])]).await.unwrap();
    assert_eq!(unchanged["messages"], json!(original));
    kernel.call_ctx("appendMessages", vec![id.clone(), json!([{ "role": "user", "content": "new constraint" }])]).await.unwrap();
    assert!(
        kernel
            .call_ctx("commitCompaction", vec![id.clone(), revision, json!("stale summary")])
            .await
            .unwrap_err()
            .to_string()
            .contains("stale context compaction")
    );
    let latest = kernel.call_ctx("appendMessages", vec![id, json!([])]).await.unwrap();
    assert_eq!(latest["messages"].as_array().unwrap().last().unwrap()["content"], "new constraint");
    assert!(!latest["messages"].as_array().unwrap().iter().any(|m| m["content"].as_str().is_some_and(|text| text.contains("context_summary"))));
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_compaction_and_irreducible_input_fail_before_model_execution() {
    let kernel = context(policy(false)).await;
    let id = kernel.call_ctx("openConversation", vec![json!("system")]).await.unwrap();
    kernel.call_ctx("appendMessages", vec![id.clone(), json!(history())]).await.unwrap();
    let err = kernel.call_ctx("prepareContext", vec![id]).await.unwrap_err();
    assert!(err.to_string().contains("automatic compaction is disabled"), "{err}");

    let kernel = context(policy(true)).await;
    let id = kernel.call_ctx("openConversation", vec![json!("system")]).await.unwrap();
    kernel.call_ctx("appendMessages", vec![id.clone(), json!([{ "role": "user", "content": "x".repeat(20000) }])]).await.unwrap();
    let err = kernel.call_ctx("prepareContext", vec![id]).await.unwrap_err();
    assert!(err.to_string().contains("original task or retained recent exchanges are too large"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn compaction_never_splits_pending_or_mismatched_tool_calls() {
    let kernel = context(policy(true)).await;
    let id = kernel.call_ctx("openConversation", vec![json!("system")]).await.unwrap();
    let mut pair = exchange(0, 100);
    pair[0]["content"].as_array_mut().unwrap().push(json!({
        "type": "toolCall", "id": "second-call", "name": "read", "arguments": { "path": "second-file" }
    }));
    kernel.call_ctx("appendMessages", vec![id.clone(), json!([{ "role": "user", "content": "task" }, pair[0]])]).await.unwrap();
    assert!(kernel.call_ctx("prepareContext", vec![id.clone()]).await.unwrap_err().to_string().contains("pending tool calls"));
    kernel.call_ctx("appendMessages", vec![id.clone(), json!([pair[1]])]).await.unwrap();
    assert!(kernel.call_ctx("prepareContext", vec![id.clone()]).await.unwrap_err().to_string().contains("pending tool calls"));
    kernel
        .call_ctx(
            "appendMessages",
            vec![
                id.clone(),
                json!([{
                    "role": "toolResult", "toolName": "read", "toolCallId": "second-call", "content": [{ "type": "text", "text": "second result" }]
                }]),
            ],
        )
        .await
        .unwrap();
    assert!(kernel.call_ctx("prepareContext", vec![id.clone()]).await.unwrap().get("context").is_some());
    kernel.call_ctx("appendMessages", vec![id.clone(), json!([exchange(1, 10)[1]])]).await.unwrap();
    assert!(kernel.call_ctx("prepareContext", vec![id]).await.unwrap_err().to_string().contains("orphan toolResult"));
}
