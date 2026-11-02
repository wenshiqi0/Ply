//! ACP plugin against deterministic plugins; no supplier calls or credentials.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

struct Peer { child: Child, input: Option<ChildStdin>, rows: Receiver<Value>, seen: Vec<Value>, next: u64 }
impl Peer {
    fn start(work: &Path) -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut command = Command::new(env!("CARGO_BIN_EXE_ply"));
        command.current_dir(work).env("HOME", work)
            .args(["acp", "--no-default-plugins", "--plugin"]).arg(root.join("plugins/acp"))
            .arg("--plugin").arg(work.join("agent")).arg("--plugin").arg(work.join("approval"))
            .arg("--plugin").arg(work.join("worker"));
        Self::spawn(command)
    }
    fn spawn(mut command: Command) -> Self {
        let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
        let input = child.stdin.take(); let output = child.stdout.take().unwrap(); let (tx, rows) = channel();
        std::thread::spawn(move || for line in BufReader::new(output).lines() {
            let line = line.unwrap();
            let value = serde_json::from_str(&line).unwrap_or_else(|error| panic!("non-protocol stdout: {line}: {error}"));
            if tx.send(value).is_err() { break; }
        });
        Self { child, input, rows, seen: Vec::new(), next: 0 }
    }
    fn send(&mut self, value: Value) { writeln!(self.input.as_mut().unwrap(), "{value}").unwrap(); self.input.as_mut().unwrap().flush().unwrap(); }
    fn request(&mut self, method: &str, params: Value) -> u64 {
        self.next += 1; let id = self.next;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})); id
    }
    fn row(&mut self) -> Value {
        let value = self.rows.recv_timeout(Duration::from_secs(8)).expect("ACP response timed out");
        self.seen.push(value.clone()); value
    }
    fn reply(&mut self, id: u64) -> Value { loop { let row = self.row(); if row["id"] == id { return row; } } }
    fn call(&mut self, method: &str, params: Value) -> Value { let id = self.request(method, params); let row = self.reply(id); assert!(row.get("error").is_none(), "{row}"); row["result"].clone() }
    fn session(&mut self, work: &Path) -> String {
        assert_eq!(self.call("initialize", json!({"protocolVersion":1}))["agentInfo"]["name"], "ply");
        self.call("session/new", json!({"cwd":work,"mcpServers":[]}))["sessionId"].as_str().unwrap().into()
    }
    fn finish(mut self) { self.input.take(); let deadline = Instant::now()+Duration::from_secs(4); loop { if let Some(status)=self.child.try_wait().unwrap(){assert!(status.success());return;} assert!(Instant::now()<deadline,"EOF did not stop server");std::thread::sleep(Duration::from_millis(10)); } }
}
impl Drop for Peer { fn drop(&mut self) { let _=self.child.kill();let _=self.child.wait(); } }

fn fixture(work: &Path) {
    for (name, source) in [
        ("agent", r#"
import plugins from "ply:plugins";
import * as events from "ply:events";
let active=false;let idle=[];
export async function onLoad(){
 await events.define("tool_call");await events.define("tool_result");
 return {
  modelCatalog:()=>[{id:'["fixture","model"]',name:"Fixture",input:["text","image"],efforts:["off","high"],defaultEffort:"high",isDefault:true,profile:{model:"model"}}],
  hostIdle:async()=>{if(active)await new Promise(resolve=>idle.push(resolve));},
  hostChat:async(input)=>{
   active=true;
   try {
    const count=(input.snapshot?.count??0)+1;
    if(input.prompt.includes("tool")){
      const id="tool-"+count;await events.emit("agent.tool_call",{id,name:"worker",input:{}});
      const answer=await plugins.approval.ask({id,tool:"worker",input:{},cwd:process.cwd()});
      const allowed=answer.every(row=>row.decision==="approved");
      if(allowed)await plugins.worker({wait:input.prompt.includes("wait")});
      await events.emit("agent.tool_result",{id,name:"worker",output:allowed?"executed":"denied",isError:!allowed});
    }
    return {result:{text:`answer-${count}:${input.prompt}:${input.images.length}`},snapshot:{count}};
   }finally{active=false;for(const resolve of idle.splice(0))resolve();}
  }
 };
}


"#),
        ("approval", r#"
import * as events from "ply:events";
export async function onLoad(){await events.define("request");await events.define("toolRequest");return {useDefault:()=>{},ask:async(input)=>await events.emit("approval.toolRequest",input)};}
"#),
        ("worker", r#"
import {writeFile} from "node:fs/promises";
export async function onHandle(input){
 await writeFile("entered","yes");
 try{if(input.wait)await new Promise(resolve=>setTimeout(resolve,10000));await writeFile("executed","yes");}
 finally{await writeFile("cleaned","yes");}
}
"#),
    ] { std::fs::create_dir(work.join(name)).unwrap(); std::fs::write(work.join(name).join("main.ts"), source).unwrap(); }
}

#[test]
fn invalid_protocol_messages_do_not_stop_the_plugin() {
    let work=tempfile::tempdir().unwrap();fixture(work.path());let mut peer=Peer::start(work.path());
    writeln!(peer.input.as_mut().unwrap(),"{{").unwrap();peer.input.as_mut().unwrap().flush().unwrap();
    assert_eq!(peer.row()["error"]["code"],-32700);
    peer.send(json!({"jsonrpc":"1.0","id":0,"method":"initialize"}));
    assert_eq!(peer.row()["error"]["code"],-32600);
    let id=peer.request("initialize",json!({"protocolVersion":0}));assert_eq!(peer.reply(id)["error"]["code"],-32602);
    let session=peer.session(work.path());
    let id=peer.request("session/prompt",json!({"sessionId":session,"prompt":42}));assert_eq!(peer.reply(id)["error"]["code"],-32602);
    peer.call("session/prompt",json!({"sessionId":session,"prompt":[{"type":"text","text":"still alive"}]}));peer.finish();
}

#[test]
fn acp_negotiates_recovers_context_replays_history_and_keeps_host_methods_private() {
    let work=tempfile::tempdir().unwrap();fixture(work.path());let mut peer=Peer::start(work.path());
    let id=peer.request("session/new",json!({"cwd":work.path(),"mcpServers":[]}));assert_eq!(peer.reply(id)["error"]["code"],-32000);
    let session=peer.session(work.path());
    let bad=peer.request("tools/list",json!({}));assert_eq!(peer.reply(bad)["error"]["code"],-32601);
    let bad=peer.request("session/new",json!({"cwd":"/","mcpServers":[]}));assert_eq!(peer.reply(bad)["error"]["code"],-32602);
    let bad=peer.request("session/new",json!({"cwd":work.path(),"mcpServers":[{}]}));assert_eq!(peer.reply(bad)["error"]["code"],-32602);
    peer.call("session/set_config_option",json!({"sessionId":session,"configId":"reasoning_effort","value":"off"}));
    assert_eq!(peer.call("session/prompt",json!({"sessionId":session,"prompt":[{"type":"text","text":"first"},{"type":"image","data":"aGVsbG8=","mimeType":"image/png"}]}))["stopReason"],"end_turn");
    assert!(peer.seen.iter().any(|row| row["params"]["update"]["content"]["text"]=="answer-1:first:1"));
    let file=work.path().join(".ply/plugins/acp").join(format!("{session}.json"));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(file).unwrap().permissions().mode()&0o777,0o600);
    peer.finish();let mut peer=Peer::start(work.path());peer.call("initialize",json!({"protocolVersion":1}));
    peer.call("session/load",json!({"sessionId":session,"cwd":work.path(),"mcpServers":[]}));
    assert!(peer.seen.iter().any(|row| row["params"]["update"]["sessionUpdate"]=="user_message_chunk"));
    peer.call("session/prompt",json!({"sessionId":session,"prompt":[{"type":"text","text":"second"}]}));
    assert!(peer.seen.iter().any(|row| row["params"]["update"]["content"]["text"]=="answer-2:second:0"));peer.finish();
}

#[test]
fn acp_permission_denial_cancellation_and_eof_stop_actual_child_work() {
    let work=tempfile::tempdir().unwrap();fixture(work.path());let mut peer=Peer::start(work.path());let session=peer.session(work.path());
    for (allowed,wait) in [(false,false),(true,false),(true,true)] {
        for file in ["entered","executed","cleaned"]{let _=std::fs::remove_file(work.path().join(file));}
        let id=peer.request("session/prompt",json!({"sessionId":session,"prompt":[{"type":"text","text":if wait{"tool wait"}else{"tool"}}]}));
        let permission=loop{let row=peer.row();if row["method"]=="session/request_permission"{break row;}};
        peer.send(json!({"jsonrpc":"2.0","id":permission["id"],"result":{"outcome":{"outcome":"selected","optionId":if allowed{"allow"}else{"reject"}}}}));
        if wait {
            let deadline=Instant::now()+Duration::from_secs(3);while !work.path().join("entered").exists(){assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}
            peer.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}));
        }
        assert_eq!(peer.reply(id)["result"]["stopReason"],if wait{"cancelled"}else{"end_turn"});
        assert_eq!(work.path().join("executed").exists(),allowed&&!wait);
        if allowed {let deadline=Instant::now()+Duration::from_secs(3);while !work.path().join("cleaned").exists(){assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}}
    }
    peer.call("session/prompt",json!({"sessionId":session,"prompt":[{"type":"text","text":"after cancellation"}]}));
    assert!(peer.seen.iter().any(|row|row["params"]["update"]["sessionUpdate"]=="tool_call_update"));
    std::fs::remove_file(work.path().join("entered")).unwrap();std::fs::remove_file(work.path().join("cleaned")).unwrap();
    peer.request("session/prompt",json!({"sessionId":session,"prompt":[{"type":"text","text":"tool wait for EOF"}]}));
    let permission=loop{let row=peer.row();if row["method"]=="session/request_permission"{break row;}};
    peer.send(json!({"jsonrpc":"2.0","id":permission["id"],"result":{"outcome":{"outcome":"selected","optionId":"allow"}}}));
    let deadline=Instant::now()+Duration::from_secs(3);while !work.path().join("entered").exists(){assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}
    peer.finish();assert!(!work.path().join("executed").exists());assert!(work.path().join("cleaned").exists());
}

#[test]
fn native_agent_uses_local_model_tools_images_and_private_persistent_context_over_acp() {
    use std::io::Read;
    use std::net::TcpListener;
    let work=tempfile::tempdir().unwrap();let home=tempfile::tempdir().unwrap();
    std::fs::write(work.path().join("note.txt"),"source evidence").unwrap();
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();
    let base=format!("http://{}/v1",listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let (record,requests)=channel();
    let server=std::thread::spawn(move||{
        for stage in 0..3 {
            let deadline=Instant::now()+Duration::from_secs(15);
            let mut socket=loop{match listener.accept(){Ok((socket,_))=>break socket,Err(error) if error.kind()==std::io::ErrorKind::WouldBlock=>{assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));},Err(error)=>panic!("{error}")}};
            socket.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
            let mut raw=Vec::new();
            let (offset,length)=loop{
                let mut bytes=[0;4096];let count=socket.read(&mut bytes).unwrap();assert!(count>0);raw.extend_from_slice(&bytes[..count]);
                if let Some(end)=raw.windows(4).position(|part|part==b"\r\n\r\n"){
                    let headers=String::from_utf8_lossy(&raw[..end]);
                    let length=headers.lines().find_map(|line|line.split_once(':').filter(|(name,_)|name.eq_ignore_ascii_case("content-length")).map(|(_,value)|value.trim().parse::<usize>().unwrap())).unwrap();
                    break(end+4,length);
                }
            };
            while raw.len()<offset+length{let mut bytes=[0;4096];let count=socket.read(&mut bytes).unwrap();assert!(count>0);raw.extend_from_slice(&bytes[..count]);}
            record.send(serde_json::from_slice::<Value>(&raw[offset..offset+length]).unwrap()).unwrap();
            let delta=if stage==0{json!({"role":"assistant","reasoning_content":"private model reasoning","tool_calls":[{"index":0,"id":"read-note","type":"function","function":{"name":"read","arguments":"{\"path\":\"note.txt\"}"}}]})}else{json!({"role":"assistant","content":if stage==1{"first complete\n[[PLY_REPORT_COMPLETE]]"}else{"second complete\n[[PLY_REPORT_COMPLETE]]"}})};
            let body=format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"id":"local","choices":[{"index":0,"delta":delta,"finish_reason":null}]}),json!({"id":"local","choices":[{"index":0,"delta":{},"finish_reason":if stage==0{"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":100,"completion_tokens":20}}));
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let plugin=work.path().join("fixture");std::fs::create_dir(&plugin).unwrap();
    let profile=json!({"provider":"deepseek","model":"deepseek-v4-flash","apiKeyEnv":"DEEPSEEK_API_KEY","reasoningEffort":"high","definition":{"id":"deepseek-v4-flash","provider":"deepseek","name":"Local fixture","api":"openai-completions","baseUrl":base,"reasoning":true,"thinkingLevelMap":{"off":null,"high":"high"},"input":["text","image"],"contextWindow":131072,"maxTokens":4096,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}});
    std::fs::write(plugin.join("main.ts"),format!("import {{context,deepseek}} from 'ply:plugins';export async function onLoad(){{void deepseek;await context.useModel({profile});}}" )).unwrap();
    let launch=||{let mut command=Command::new(env!("CARGO_BIN_EXE_ply"));command.current_dir(work.path()).env("HOME",home.path()).env("DEEPSEEK_API_KEY","local-fixture-only").env("NO_PROXY","127.0.0.1,localhost").env("no_proxy","127.0.0.1,localhost").args(["acp","--plugin"]).arg(&plugin);Peer::spawn(command)};
    let mut peer=launch();let session=peer.session(work.path());
    let id=peer.request("session/prompt",json!({"sessionId":session,"prompt":[{"type":"text","text":"first prompt read the note"},{"type":"image","data":"aGVsbG8=","mimeType":"image/png"}]}));
    loop{let row=peer.row();if row["method"]=="session/request_permission"{peer.send(json!({"jsonrpc":"2.0","id":row["id"],"result":{"outcome":{"outcome":"selected","optionId":"allow"}}}));}if row["id"]==id{assert_eq!(row["result"]["stopReason"],"end_turn","{row}");break;}}
    assert!(peer.seen.iter().any(|row|row["params"]["update"]["title"]=="read"));
    assert!(!peer.seen.iter().any(|row|row.to_string().contains("private model reasoning")));
    peer.finish();
    let mut peer=launch();peer.call("initialize",json!({"protocolVersion":1}));peer.call("session/load",json!({"sessionId":session,"cwd":work.path(),"mcpServers":[]}));
    peer.call("session/prompt",json!({"sessionId":session,"prompt":[{"type":"text","text":"second prompt"}]}));peer.finish();server.join().unwrap();
    let records:Vec<_>=requests.try_iter().collect();assert_eq!(records.len(),3);
    assert!(records[0].to_string().contains("image_url"));assert!(records[1].to_string().contains("source evidence"));assert!(records[2].to_string().contains("first prompt"));
    let output=Command::new(env!("CARGO_BIN_EXE_ply")).current_dir(work.path()).env("HOME",home.path()).args(["call","read"]).arg(json!({"path":format!("/ply/state/{session}.json")}).to_string()).output().unwrap();
    assert!(!output.status.success(),"read plugin accessed ACP private state");assert!(!String::from_utf8_lossy(&output.stdout).contains("first prompt"));
}
