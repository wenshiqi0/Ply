//! Plugin imports and native command registration/lifecycle; no model calls.
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use wendy_kernel::{
    DiskStorage, Exit, FiberEvent, FiberHook, Io, Kernel, KernelConfig, MountMode, PluginSource,
};

#[derive(Default)]
struct Records(Mutex<Vec<FiberEvent>>);
impl FiberHook for Records {
    fn on_event(&self, event: &FiberEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}
fn source(name: &str, code: &str) -> PluginSource {
    PluginSource::Embedded {
        name: name.into(),
        source: code.into(),
    }
}
fn kernel(work: &std::path::Path) -> (Kernel, Arc<Records>) {
    let root = std::fs::canonicalize(work).unwrap().display().to_string();
    let io = Io::new();
    io.mount(
        &root,
        Arc::new(DiskStorage::open(work).unwrap()),
        MountMode::ReadWrite,
    )
    .unwrap();
    let k = Kernel::new(KernelConfig::new(io, root));
    let records = Arc::new(Records::default());
    k.add_hook(records.clone());
    (k, records)
}

#[tokio::test(flavor = "multi_thread")]
async fn commands_use_the_registered_realm_and_preserve_call_ancestry() {
    let work = tempfile::tempdir().unwrap();
    let (k, records) = kernel(work.path());
    k.load(source("provider",r#"
import plugins from "wendy:plugins";
import * as commands from "wendy:commands";
export async function onLoad() {
  await commands.register({name:"greet",description:"Say hello",handler:async input => ({text:"hello "+input.name,owner:await fiber.info(),legacy:typeof globalThis.tools,implicit:typeof globalThis.plugins,names:Object.keys(plugins)})});
  await commands.register({name:"chain",handler:async input => ({owner:await fiber.info(),child:await plugins.worker(input)})});
}
"#)).await.unwrap();
    k.load(source(
        "worker",
        "export async function onHandle() { return await fiber.info(); }",
    ))
    .await
    .unwrap();
    k.load(source("caller",r#"
import * as commands from "wendy:commands";
export async function onHandle(input: {}) { return {owner:await fiber.info(),result:await commands.run("chain",input)}; }
"#)).await.unwrap();
    k.ready().await.unwrap();
    assert_eq!(
        k.commands()
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["chain", "greet"]
    );
    assert!(
        !k.catalog()
            .iter()
            .any(|tool| tool.name == "provider" || tool.name == "greet" || tool.name == "chain")
    );
    let result = k
        .run_command("greet", json!({"name":"Wendy"}))
        .await
        .unwrap();
    assert_eq!(result["text"], "hello Wendy");
    assert_eq!(result["legacy"], "undefined");
    assert_eq!(result["implicit"], "undefined");
    assert!(
        result["names"]
            .as_array()
            .unwrap()
            .contains(&json!("worker")),
        "import must reflect plugins loaded later"
    );
    assert_eq!(result["owner"]["parent"], Value::Null);
    let chained = k.call("caller", json!({})).await.unwrap();
    assert_eq!(chained["result"]["owner"]["parent"], chained["owner"]["id"]);
    assert_eq!(
        chained["result"]["child"]["parent"],
        chained["result"]["owner"]["id"]
    );
    assert!(records.0.lock().unwrap().iter().any(|event|matches!(event,FiberEvent::Spawn{fiber,..} if fiber.plugin=="provider"&&fiber.event.as_deref()==Some("command:greet"))));
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_failed_and_forged_registrations_cannot_replace_commands() {
    let work = tempfile::tempdir().unwrap();
    let (k, _) = kernel(work.path());
    k.load(source(
        "provider",
        r#"
import * as commands from "wendy:commands";
export async function onLoad() {await commands.register({name:"greet",handler:()=>"original"});}
export async function onHandle() {await commands.register({name:"late",handler:()=>"late"});}
"#,
    ))
    .await
    .unwrap();
    k.ready().await.unwrap();
    assert!(
        k.call("provider", json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("onLoad")
    );
    let script = work.path().join("provider.ts");
    std::fs::write(&script,"import * as commands from 'wendy:commands'; await commands.register({name:'forged',handler:()=>true});").unwrap();
    assert!(
        k.run_script(&script, vec![])
            .await
            .unwrap()
            .result
            .unwrap_err()
            .to_string()
            .contains("loaded plugin realm")
    );
    k.load(source(
        "broken",
        r#"
import * as commands from "wendy:commands";
export async function onLoad() {
 await commands.register({name:"transient",handler:()=>"stale"});
 await commands.register({name:"greet",handler:()=>"replacement"});
}
"#,
    ))
    .await
    .unwrap();
    assert!(k.ready().await.is_err());
    assert_eq!(k.commands().len(), 1);
    assert_eq!(k.run_command("greet", json!({})).await.unwrap(), "original");
    assert!(
        k.run_command("transient", json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("no registered command")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn command_success_and_parent_cancel_reclaim_pending_callback_work() {
    let work = tempfile::tempdir().unwrap();
    let (k, records) = kernel(work.path());
    k.load(source("provider",r#"
import fs from "node:fs";
import * as commands from "wendy:commands";
export async function onLoad() {
 await commands.register({name:"background",handler:()=>{setTimeout(()=>fs.writeFileSync("stray","bad"),60);return "done";}});
 await commands.register({name:"slow",handler:async()=>{fs.writeFileSync("entered","yes");await new Promise(resolve=>setTimeout(resolve,150));fs.writeFileSync("late","bad");}});
}
"#)).await.unwrap();
    k.load(source(
        "caller",
        r#"
import * as commands from "wendy:commands";
export async function onHandle() {return await commands.run("slow");}
"#,
    ))
    .await
    .unwrap();
    k.ready().await.unwrap();
    assert_eq!(
        k.run_command("background", json!({})).await.unwrap(),
        "done"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!work.path().join("stray").exists());
    let root = k.wake("caller", json!({})).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !work.path().join("entered").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(k.cancel_fiber(&root));
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(!work.path().join("late").exists());
    let events = records.0.lock().unwrap();
    let child = events
        .iter()
        .find_map(|event| match event {
            FiberEvent::Spawn { fiber, .. } if fiber.event.as_deref() == Some("command:slow") => {
                Some(&fiber.id)
            }
            _ => None,
        })
        .unwrap();
    assert!(events.iter().any(
        |event| matches!(event,FiberEvent::Exit{id,exit:Exit::Cancelled{..},..} if id==child)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn command_callbacks_do_not_spend_plugin_onhandle_quotas() {
    let work = tempfile::tempdir().unwrap();
    let (k, _) = kernel(work.path());
    k.load(source(
        "provider",
        r#"
import * as commands from "wendy:commands";
export function onHandle() {return "tool";}
export async function onLoad() {await commands.register({name:"ping",handler:()=>"pong"});}
"#,
    ))
    .await
    .unwrap();
    k.load(source(
        "caller",
        r#"
import plugins from "wendy:plugins";
import * as commands from "wendy:commands";
export async function onHandle() {
 await fiber.limitChildren("provider",1);
 await commands.run("ping");await commands.run("ping");
 const before=await fiber.childBudget("provider");
 await plugins.provider({});
 let rejected=false;try{await plugins.provider({});}catch{rejected=true;}
 return {before,after:await fiber.childBudget("provider"),rejected};
}
"#,
    ))
    .await
    .unwrap();
    k.ready().await.unwrap();
    let output = k.call("caller", json!({})).await.unwrap();
    assert_eq!(output["before"]["started"], 0);
    assert_eq!(output["after"]["started"], 1);
    assert_eq!(output["rejected"], true);
}
