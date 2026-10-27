//! Context block ordering, prefix stability, conversation capabilities and plugin namespace regressions.
use serde_json::{Value, json};
use ply_kernel::{Io, Kernel, KernelConfig, PluginSource};

async fn setup(providers: &[(&str, &str)]) -> Kernel {
    let kernel = Kernel::new(KernelConfig::new(Io::new(), "/"));
    // Providers can import context before its realm exists.
    for (name, source) in providers {
        kernel
            .load(PluginSource::Embedded {
                name: (*name).into(),
                source: (*source).into(),
            })
            .await
            .unwrap();
    }
    kernel
        .load(PluginSource::builtin("context").unwrap())
        .await
        .unwrap();
    kernel.ready().await.unwrap();
    kernel
}
async fn open(k: &Kernel) -> Value {
    let id = k
        .call_plugin_method("context", "openConversation", vec![json!("base")])
        .await
        .unwrap();
    k.call_plugin_method(
        "context",
        "appendMessages",
        vec![id.clone(), json!([{"role":"user","content":"task"}])],
    )
    .await
    .unwrap();
    id
}
async fn set(k: &Kernel, id: &Value, key: &str, content: &str) -> Value {
    k.call_plugin_method(
        "context",
        "set",
        vec![json!({"source":"host","conversation":id,"type":"state","key":key,"content":content})],
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn named_exports_are_live_source_scoped_and_semantically_ordered() {
    // Named imports order onLoad: context.Type is readable without waiting.
    let k=setup(&[
        ("zeta",r#"import {context} from "ply:plugins"; export async function onLoad(){await context.set({source:"zeta",type:context.Type.Extensions,key:"catalog",content:"ZETA_EXT"});return {probe:()=>true};}"#),
        ("alpha",r#"import plugins,{context,zeta} from "ply:plugins"; export async function onLoad(){await context.set({source:"alpha",type:context.Type.Project,key:"project",content:"ALPHA_PROJECT"});await context.set({source:"alpha",type:context.Type.Extensions,key:"catalog",content:"ALPHA_EXT"});return {sameHandle:()=>context===plugins.context,hasZeta:()=>zeta.probe()};}"#),
    ]).await;
    assert_eq!(k.call_plugin_method("alpha", "hasZeta", vec![]).await.unwrap(), true);
    assert_eq!(k.call_plugin_method("alpha", "sameHandle", vec![]).await.unwrap(), true);
    let id = open(&k).await;
    let first = k
        .call_plugin_method("context", "prepareContext", vec![id.clone()])
        .await
        .unwrap();
    let system = first["context"]["systemPrompt"].as_str().unwrap();
    assert!(system.find("ALPHA_PROJECT").unwrap() < system.find("ALPHA_EXT").unwrap());
    assert!(system.find("ALPHA_EXT").unwrap() < system.find("ZETA_EXT").unwrap());
    let blocks = k.call_plugin_method("context", "blocks", vec![id.clone()]).await.unwrap();
    assert!(
        blocks
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["source"] == "zeta")
    );
    for (input, error) in [
        (json!({"source":"host","type":"state","key":"x","content":"x","order":1}), "unknown context input field: order"),
        (json!({"source":"host","type":"rules","key":"x","content":"x","conversation":id}), "fixed context is frozen"),
        (json!({"type":"state","key":"x","content":"x","conversation":id}), "invalid context input"),
        (json!({"source":"context","type":"state","key":"x","content":"x","conversation":id}), "invalid context input"),
    ] {
        let err = k.call_plugin_method("context", "set", vec![input]).await.unwrap_err().to_string();
        assert!(err.contains(error), "{err}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn updates_are_appended_noops_preserve_prefix_and_snapshots_freeze_tools() {
    let k = setup(&[]).await;
    let id = open(&k).await;
    let first = k
        .call_plugin_method("context", "prepareContext", vec![id.clone()])
        .await
        .unwrap();
    assert_eq!(set(&k, &id, "workspace", "v1").await["changed"], true);
    let second = k
        .call_plugin_method("context", "prepareContext", vec![id.clone()])
        .await
        .unwrap();
    assert_eq!(
        first["context"]["systemPrompt"],
        second["context"]["systemPrompt"]
    );
    assert_eq!(
        first["budget"]["fixed_prefix_fingerprint"],
        second["budget"]["fixed_prefix_fingerprint"]
    );
    let a = first["context"]["messages"].as_array().unwrap();
    let b = second["context"]["messages"].as_array().unwrap();
    assert_eq!(a.as_slice(), &b[..a.len()]);
    assert_eq!(set(&k, &id, "workspace", "v1").await["changed"], false);
    assert_eq!(
        k.call_plugin_method("context", "prepareContext", vec![id.clone()])
            .await
            .unwrap()["context"],
        second["context"]
    );
    set(&k, &id, "workspace", "v2").await;
    let third = k
        .call_plugin_method("context", "prepareContext", vec![id.clone()])
        .await
        .unwrap();
    let c = third["context"]["messages"].as_array().unwrap();
    assert_eq!(b.as_slice(), &c[..b.len()]);
    k.load(PluginSource::Embedded {
        name: "late".into(),
        source: "export function onHandle(input:{value:string}){return input.value;}".into(),
    })
    .await
    .unwrap();
    k.call_plugin_method("context", "registerTool", vec![json!("late")])
        .await
        .unwrap();
    let same = k
        .call_plugin_method("context", "prepareContext", vec![id.clone()])
        .await
        .unwrap();
    assert_eq!(same["tools_signature"], first["tools_signature"]);
    let next = open(&k).await;
    assert_eq!(
        k.call_plugin_method("context", "prepareContext", vec![next]).await.unwrap()["context"]["tools"][0]["name"],
        "late"
    );
    let saved = k
        .call_plugin_method("context", "exportConversation", vec![id.clone()])
        .await
        .unwrap();
    assert_eq!(saved["format"], 2);
    let restored = k
        .call_plugin_method(
            "context",
            "restoreConversation",
            vec![
                saved.clone(),
                json!({"contextWindow":131072,"maxTokens":16384}),
                json!([]),
            ],
        )
        .await
        .unwrap();
    let view = k.call_plugin_method("context", "prepareContext", vec![restored]).await.unwrap();
    assert_eq!(view["context"], third["context"]);
    let mut old = saved;
    old.as_object_mut().unwrap().remove("format");
    assert!(
        k.call_plugin_method(
            "context",
            "restoreConversation",
            vec![
                old,
                json!({"contextWindow":131072,"maxTokens":16384}),
                json!([])
            ]
        )
        .await
        .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn state_updates_do_not_split_a_tool_exchange() {
    let k = setup(&[]).await;
    let id = open(&k).await;
    k.call_plugin_method("context", "appendMessages",vec![id.clone(),json!([{"role":"assistant","content":[{"type":"toolCall","id":"read-1","name":"read","arguments":{}}]}])]).await.unwrap();
    set(&k, &id, "workspace", "changed while tool pending").await;
    assert!(
        k.call_plugin_method("context", "prepareContext", vec![id.clone()])
            .await
            .is_err()
    );
    k.call_plugin_method("context", "appendMessages",vec![id.clone(),json!([{"role":"toolResult","toolCallId":"read-1","toolName":"read","content":[{"type":"text","text":"done"}]}])]).await.unwrap();
    let view = k.call_plugin_method("context", "prepareContext", vec![id]).await.unwrap();
    let messages = view["context"]["messages"].as_array().unwrap();
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[2]["role"], "toolResult");
    assert!(messages[3]["ply_context"].is_object());
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_conversation_ids_work_from_any_caller() {
    let k=setup(&[("driver",r#"import {context} from "ply:plugins";
      export async function onHandle(input:{name:string,foreign:string}){
        const id=await context.openConversation("driver");
        await context.appendMessages(id,[{role:"user",content:input.name}]);
        await context.set({source:"driver",conversation:id,type:context.Type.State,key:"identity",content:input.name});
        await context.set({source:"driver",conversation:input.foreign,type:context.Type.State,key:"from_"+input.name,content:input.name});
        await new Promise(resolve=>setTimeout(resolve,10));
        return {id,view:(await context.prepareContext(id)).context};
      }"#)]).await;
    let foreign = open(&k).await;
    let (a, b) = tokio::join!(
        k.call("driver", json!({"name":"alpha","foreign":foreign})),
        k.call("driver", json!({"name":"beta","foreign":foreign}))
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert!(!a["view"].to_string().contains("beta"));
    assert!(!b["view"].to_string().contains("alpha"));
    // A plugin may use an id the host opened, and the host one a plugin opened.
    for name in ["alpha", "beta"] {
        assert_eq!(block(&k, Some(&foreign), "driver", &format!("from_{name}")).await["content"], name);
    }
    assert_eq!(set(&k, &a["id"], "identity", "host").await["changed"], true);
    assert_eq!(block(&k, Some(&a["id"]), "driver", "identity").await["content"], "alpha");
    for id in [&a["id"], &b["id"]] {
        k.call_plugin_method("context", "closeConversation", vec![id.clone()]).await.unwrap();
        assert!(k.call_plugin_method("context", "blocks", vec![id.clone()]).await.unwrap_err().to_string().contains("no open conversation"));
    }
    assert!(k.call_plugin_method("context", "closeConversation", vec![]).await.unwrap_err().to_string().contains("requires a conversation id"));
}

const NOTES: (&str, &str) = ("notes", r#"import {context} from "ply:plugins"; let value="N1";
  export async function onLoad(){await context.set({source:"notes",type:context.Type.Reference,key:"notes",content:null,provider:"current"});return {current:()=>value,put:(next:string)=>{value=next;}};}"#);
async fn block(k: &Kernel, id: Option<&Value>, source: &str, key: &str) -> Value {
    let blocks = k.call_plugin_method("context", "blocks", id.into_iter().cloned().collect()).await.unwrap();
    blocks.as_array().unwrap().iter().find(|b| b["source"] == source && b["key"] == key).cloned().unwrap_or(Value::Null)
}
async fn batch(k: &Kernel, id: &Value, call: &str) {
    k.call_plugin_method("context", "appendMessages", vec![id.clone(), json!([{"role":"assistant","content":[{"type":"toolCall","id":call,"name":"read","arguments":{}}]}])]).await.unwrap();
    k.call_plugin_method("context", "appendToolResults", vec![id.clone(), json!([{"id":call,"name":"read","output":"done","isError":false}])]).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn set_without_a_conversation_changes_only_the_template() {
    let k = setup(&[]).await;
    let running = open(&k).await;
    let before = k.call_plugin_method("context", "prepareContext", vec![running.clone()]).await.unwrap();
    for (kind, content) in [("project", "LATE_PROJECT"), ("state", "LATE_STATE")] {
        let out = k.call_plugin_method("context", "set", vec![json!({"source":"host","type":kind,"key":kind,"content":content})]).await.unwrap();
        assert_eq!(out, json!({"changed":true,"version":1,"scope":"template"}));
        assert_eq!(block(&k, None, "host", kind).await["content"], content);
        assert_eq!(block(&k, Some(&running), "host", kind).await, Value::Null);
    }
    let same = k.call_plugin_method("context", "prepareContext", vec![running.clone()]).await.unwrap();
    assert_eq!(same["context"], before["context"]);
    let next = open(&k).await;
    let view = k.call_plugin_method("context", "prepareContext", vec![next]).await.unwrap();
    assert!(view["context"]["systemPrompt"].as_str().unwrap().contains("LATE_PROJECT"));
    assert!(view["context"]["messages"].to_string().contains("LATE_STATE"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reference_providers_refresh_after_a_tool_batch_and_skip_identical_content() {
    let k = setup(&[NOTES]).await;
    let id = open(&k).await;
    assert_eq!(block(&k, Some(&id), "notes", "notes").await, json!({"source":"notes","key":"notes","type":"reference","content":"N1","version":1}));
    k.call_plugin_method("notes", "put", vec![json!("N2")]).await.unwrap();
    k.call_plugin_method("context", "appendMessages", vec![id.clone(), json!([{"role":"user","content":"more"}])]).await.unwrap();
    assert_eq!(block(&k, Some(&id), "notes", "notes").await["version"], 1);
    batch(&k, &id, "read-1").await;
    let refreshed = block(&k, Some(&id), "notes", "notes").await;
    assert_eq!((&refreshed["content"], &refreshed["version"]), (&json!("N2"), &json!(2)));
    let view = k.call_plugin_method("context", "prepareContext", vec![id.clone()]).await.unwrap();
    let messages = view["context"]["messages"].as_array().unwrap();
    assert!(messages.last().unwrap()["ply_context"].is_object() && messages.last().unwrap().to_string().contains("N2"));
    batch(&k, &id, "read-2").await;
    assert_eq!(block(&k, Some(&id), "notes", "notes").await["version"], 2);
    assert_eq!(k.call_plugin_method("context", "inspect", vec![id.clone()]).await.unwrap()["pending_updates"], json!([]));
    let again = k.call_plugin_method("context", "prepareContext", vec![id]).await.unwrap();
    assert_eq!(again["context"]["messages"].as_array().unwrap().len(), messages.len() + 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_inherited_child_copies_the_parent_blocks_and_cannot_change_them() {
    let k = setup(&[NOTES]).await;
    k.call_plugin_method("context", "set", vec![json!({"source":"host","type":"state","key":"plan","content":"TEMPLATE"})]).await.unwrap();
    let parent = open(&k).await;
    set(&k, &parent, "plan", "PARENT").await;
    k.call_plugin_method("notes", "put", vec![json!("N2")]).await.unwrap();
    let parent_blocks = k.call_plugin_method("context", "blocks", vec![parent.clone()]).await.unwrap();
    let task = json!({"prompt":"child task","model":{"contextWindow":131072,"maxTokens":16384},"allowed_tools":[],"inherit":parent,
        "facts":{"max_children":0,"max_delegation_depth":0,"report_sections":[],"completion_marker":"[[DONE]]"}});
    let child = k.call_plugin_method("context", "openTask", vec![task.clone()]).await.unwrap();
    let own = |blocks: Value| -> Vec<Value> { blocks.as_array().unwrap().iter().filter(|b| b["source"] != "context").cloned().collect() };
    assert_eq!(own(k.call_plugin_method("context", "blocks", vec![child.clone()]).await.unwrap()), own(parent_blocks.clone()));
    assert_eq!(block(&k, Some(&child), "host", "plan").await["content"], "PARENT");
    assert_eq!(block(&k, Some(&child), "notes", "notes").await["content"], "N1", "a child does not refresh providers");
    set(&k, &child, "plan", "CHILD").await;
    batch(&k, &child, "read-1").await;
    assert_eq!(block(&k, Some(&child), "notes", "notes").await["content"], "N1");
    assert_eq!(k.call_plugin_method("context", "blocks", vec![parent.clone()]).await.unwrap(), parent_blocks);
    // Restoring as a child keeps the snapshot; a root restore refreshes.
    let mut args = vec![k.call_plugin_method("context", "exportConversation", vec![child.clone()]).await.unwrap(), task["model"].clone(), json!([])];
    let root = k.call_plugin_method("context", "restoreConversation", args.clone()).await.unwrap();
    assert_eq!(block(&k, Some(&root), "notes", "notes").await["content"], "N2");
    args.extend([Value::Null, parent.clone()]);
    let restored = k.call_plugin_method("context", "restoreConversation", args).await.unwrap();
    assert_eq!(block(&k, Some(&restored), "notes", "notes").await["content"], "N1");
    assert_eq!(block(&k, Some(&restored), "host", "plan").await["content"], "CHILD");
    k.call_plugin_method("context", "closeConversation", vec![parent.clone()]).await.unwrap();
    assert!(k.call_plugin_method("context", "openTask", vec![task]).await.unwrap_err().to_string().contains("no open conversation"));
    let fresh = open(&k).await;
    assert_eq!(block(&k, Some(&fresh), "host", "plan").await["content"], "TEMPLATE");
    assert_eq!(block(&k, Some(&fresh), "notes", "notes").await["content"], "N2");
}
