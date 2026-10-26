//! Context block ordering, prefix stability, ownership and plugin namespace regressions.
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
        .call_ctx("openConversation", vec![json!("base")])
        .await
        .unwrap();
    k.call_ctx(
        "appendMessages",
        vec![id.clone(), json!([{"role":"user","content":"task"}])],
    )
    .await
    .unwrap();
    id
}
async fn set(k: &Kernel, id: &Value, key: &str, content: &str) -> Value {
    k.call_ctx(
        "set",
        vec![json!({"conversation":id,"type":"state","key":key,"content":content})],
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn named_exports_are_live_owner_scoped_and_semantically_ordered() {
    let k=setup(&[
        ("zeta",r#"import {context} from "ply:plugins"; export async function onLoad(){await context.loaded;await context.set({type:context.Type.Extensions,key:"catalog",content:"ZETA_EXT"});return {probe:()=>true};}"#),
        ("alpha",r#"import plugins,{context,zeta} from "ply:plugins"; export async function onLoad(){await context.loaded;await context.set({type:context.Type.Project,key:"project",content:"ALPHA_PROJECT"});await context.set({type:context.Type.Extensions,key:"catalog",content:"ALPHA_EXT"});return {sameHandle:()=>context===plugins.context,hasZeta:()=>zeta.probe()};}"#),
    ]).await;
    assert_eq!(k.call_ctx("hasZeta", vec![]).await.unwrap(), true);
    assert_eq!(k.call_ctx("sameHandle", vec![]).await.unwrap(), true);
    let id = open(&k).await;
    let first = k
        .call_ctx("prepareContext", vec![id.clone()])
        .await
        .unwrap();
    let system = first["context"]["systemPrompt"].as_str().unwrap();
    assert!(system.find("ALPHA_PROJECT").unwrap() < system.find("ALPHA_EXT").unwrap());
    assert!(system.find("ALPHA_EXT").unwrap() < system.find("ZETA_EXT").unwrap());
    let blocks = k.call_ctx("blocks", vec![id.clone()]).await.unwrap();
    assert!(
        blocks
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["source"] == "zeta")
    );
    for input in [
        json!({"type":"state","key":"x","content":"x","order":1}),
        json!({"type":"rules","key":"x","content":"x","conversation":id}),
    ] {
        assert!(k.call_ctx("set", vec![input]).await.is_err());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn updates_are_appended_noops_preserve_prefix_and_snapshots_freeze_tools() {
    let k = setup(&[]).await;
    let id = open(&k).await;
    let first = k
        .call_ctx("prepareContext", vec![id.clone()])
        .await
        .unwrap();
    assert_eq!(set(&k, &id, "workspace", "v1").await["changed"], true);
    let second = k
        .call_ctx("prepareContext", vec![id.clone()])
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
        k.call_ctx("prepareContext", vec![id.clone()])
            .await
            .unwrap()["context"],
        second["context"]
    );
    set(&k, &id, "workspace", "v2").await;
    let third = k
        .call_ctx("prepareContext", vec![id.clone()])
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
    k.call_ctx("registerTool", vec![json!("late")])
        .await
        .unwrap();
    let same = k
        .call_ctx("prepareContext", vec![id.clone()])
        .await
        .unwrap();
    assert_eq!(same["tools_signature"], first["tools_signature"]);
    let next = open(&k).await;
    assert_eq!(
        k.call_ctx("prepareContext", vec![next]).await.unwrap()["context"]["tools"][0]["name"],
        "late"
    );
    let saved = k
        .call_ctx("exportConversation", vec![id.clone()])
        .await
        .unwrap();
    assert_eq!(saved["format"], 2);
    let restored = k
        .call_ctx(
            "restoreConversation",
            vec![
                saved.clone(),
                json!({"contextWindow":131072,"maxTokens":16384}),
                json!([]),
            ],
        )
        .await
        .unwrap();
    let view = k.call_ctx("prepareContext", vec![restored]).await.unwrap();
    assert_eq!(view["context"], third["context"]);
    let mut old = saved;
    old.as_object_mut().unwrap().remove("format");
    assert!(
        k.call_ctx(
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
    k.call_ctx("appendMessages",vec![id.clone(),json!([{"role":"assistant","content":[{"type":"toolCall","id":"read-1","name":"read","arguments":{}}]}])]).await.unwrap();
    set(&k, &id, "workspace", "changed while tool pending").await;
    assert!(
        k.call_ctx("prepareContext", vec![id.clone()])
            .await
            .is_err()
    );
    k.call_ctx("appendMessages",vec![id.clone(),json!([{"role":"toolResult","toolCallId":"read-1","toolName":"read","content":[{"type":"text","text":"done"}]}])]).await.unwrap();
    let view = k.call_ctx("prepareContext", vec![id]).await.unwrap();
    let messages = view["context"]["messages"].as_array().unwrap();
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[2]["role"], "toolResult");
    assert!(messages[3]["ply_context"].is_object());
}

#[tokio::test(flavor = "multi_thread")]
async fn mutable_blocks_are_bound_to_the_native_calling_fiber() {
    let k=setup(&[("driver",r#"import {context} from "ply:plugins";
      export async function onHandle(input:{name:string,foreign:string}){
        const id=await context.openConversation("driver");
        try {
          await context.appendMessages(id,[{role:"user",content:input.name}]);
          await context.set({type:context.Type.State,key:"identity",content:input.name});
          let rejected=false;
          try {await context.set({conversation:input.foreign,type:context.Type.State,key:"identity",content:"crossed"});}catch{rejected=true;}
          await new Promise(resolve=>setTimeout(resolve,10));
          return {rejected,view:(await context.prepareContext(id)).context};
        }finally{await context.closeConversation(id);}
      }"#)]).await;
    let foreign = open(&k).await;
    let (a, b) = tokio::join!(
        k.call("driver", json!({"name":"alpha","foreign":foreign})),
        k.call("driver", json!({"name":"beta","foreign":foreign}))
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a["rejected"], true);
    assert_eq!(b["rejected"], true);
    assert!(!a["view"].to_string().contains("beta"));
    assert!(!b["view"].to_string().contains("alpha"));
}
