//! Native host capability discovery and cancellation without supplier calls.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn command(work: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ply"));
    command.current_dir(work).env("HOME", work.join("home")).arg("mcp");
    command
}

#[test]
fn native_catalog_and_host_methods_are_explicit_and_not_model_tools() {
    let work = tempfile::tempdir().unwrap();
    for enabled in [false, true] {
        let mut cmd = command(work.path());
        if enabled { cmd.arg("--host-chat"); }
        let mut child = cmd.args(["--model", "mimo-v2.6-pro"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        for (id,method) in [(1,"initialize"),(2,"ply/agent/catalog"),(3,"tools/list")] {
            writeln!(input,"{}",json!({"jsonrpc":"2.0","id":id,"method":method,"params":{}})).unwrap();
        }
        drop(input);
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let rows: Vec<Value> = String::from_utf8(output.stdout).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        let reply = |id| &rows.iter().find(|value| value["id"]==id).unwrap()["result"];
        assert_eq!(reply(1)["capabilities"]["experimental"]["plyAgent"], if enabled {1}else{0});
        assert!(!reply(3)["tools"].to_string().contains("hostChat"));
        if enabled {
            let models=reply(2).as_array().unwrap();
            assert_eq!(models.len(),2);
            for model in models {
                assert_eq!(model["input"],json!(["text","image"]));
                assert_eq!(model["efforts"],json!(["off","high"]));
                assert_eq!(model["defaultEffort"],"high");
            }
        } else { assert_eq!(rows.iter().find(|v|v["id"]==2).unwrap()["error"]["code"],-32601); }
    }
}

#[test]
fn mcp_cancellation_stops_the_awaited_fiber_and_keeps_transport_alive() {
    let work=tempfile::tempdir().unwrap();
    let plugin=work.path().join("slow");std::fs::create_dir(&plugin).unwrap();
    std::fs::write(plugin.join("main.ts"),r#"import {writeFile} from "node:fs/promises";
export async function onHandle(){await writeFile("entered","yes");await new Promise(r=>setTimeout(r,6000));await writeFile("must-not-run","bad");return "done";}"#).unwrap();
    let mut child=command(work.path()).arg("--plugin").arg(plugin).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut input=child.stdin.take().unwrap();
    let out=child.stdout.take().unwrap();
    let (tx,rx)=std::sync::mpsc::channel();
    let reader=std::thread::spawn(move||for line in BufReader::new(out).lines(){if tx.send(serde_json::from_str::<Value>(&line.unwrap()).unwrap()).is_err(){break;}});
    writeln!(input,"{}",json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"slow","arguments":{}}})).unwrap();input.flush().unwrap();
    let deadline=Instant::now()+Duration::from_secs(5);
    while !work.path().join("entered").exists(){assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(10));}
    writeln!(input,"{}",json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}})).unwrap();input.flush().unwrap();
    let cancelled=rx.recv_timeout(Duration::from_secs(4)).unwrap();
    assert_eq!(cancelled["id"],1);assert_eq!(cancelled["error"]["code"],-32800);
    writeln!(input,"{}",json!({"jsonrpc":"2.0","id":2,"method":"ping"})).unwrap();input.flush().unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(4)).unwrap()["id"],2);
    drop(input);assert!(child.wait().unwrap().success());reader.join().unwrap();
    assert!(!work.path().join("must-not-run").exists());
}

#[test]
fn host_images_share_the_user_message_and_survive_context_restore() {
    let work=tempfile::tempdir().unwrap();
    let script=work.path().join("images.ts");
    std::fs::write(&script,r#"import {context} from "ply:plugins";
const model={contextWindow:131072,maxTokens:4096};
const facts={max_children:0,max_delegation_depth:0,report_sections:[],completion_marker:"[[DONE]]"};
const request={model,facts,allowed_tools:[]};
const image={type:"image" as const,mimeType:"image/png",data:"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a9iUAAAAASUVORK5CYII="};
const id=await context.openTask({...request,prompt:"inspect the supplied image",images:[image]});
const snapshot=await context.exportConversation(id);
await context.closeConversation(id);
const restored=await context.restoreTask(snapshot,request);
const prepared=await context.prepareContext(restored);
const messages=prepared.context!.messages.filter(m=>m.role==="user"&&Array.isArray(m.content)) as {content:{type:string;text?:string}[]}[];
console.log(JSON.stringify({count:messages.length,text:messages[0].content[0].text,image:messages[0].content[1].type}));
await context.closeConversation(restored);"#).unwrap();
    let output=Command::new(env!("CARGO_BIN_EXE_ply")).current_dir(work.path()).env("HOME",work.path().join("home"))
        .arg("run").arg(script).output().unwrap();
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let value:Value=serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value,json!({"count":1,"text":"inspect the supplied image","image":"image"}));
}
