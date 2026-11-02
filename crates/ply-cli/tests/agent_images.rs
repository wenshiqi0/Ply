//! Native image continuation remains independent of the host protocol.
use serde_json::{Value, json};
use std::process::Command;

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
