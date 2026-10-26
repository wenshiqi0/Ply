//! Jev's native System One API over loopback HTTP: no supplier calls or keys.
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use ply_kernel::{
    DiskStorage, FiberEvent, FiberHook, Io, Kernel, KernelConfig, MountMode, PluginSource,
};

#[derive(Default)]
struct Records(Mutex<Vec<FiberEvent>>);
impl FiberHook for Records {
    fn on_event(&self, event: &FiberEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

const FAKE_MODEL: &str = r#"
async function modelRequest(req) {
  if (req.operation === "describe") return { contextWindow: 32768, maxTokens: 1024 };
  const task = req.context.messages.find(m => m.role === "user").content;
  const results = req.context.messages.filter(m => m.role === "toolResult");
  return { role: "assistant", content: results.length
    ? [{ type: "text", text: "done\n[[PLY_REPORT_COMPLETE]]" }]
    : [{ type: "toolCall", id: "shell", name: "bash", arguments: { command: task.includes("timeout") ? "echo timeout > agent-timeout" : "echo allow > agent-created", timeout_ms: task.includes("timeout") ? 300 : 10000 } }],
    stopReason: results.length ? "stop" : "toolUse", usage: { input: 10, output: 5, cacheRead: 90, cacheWrite: 0, reasoning: 0, totalTokens: 105 } };
}
"#;

#[test]
fn jev_native_contract_guards_commands_and_accounts_for_its_model_calls() {
    let state = tempfile::tempdir().unwrap();
    std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let keys = [
        "TYPESAFE_API_KEY",
        "NO_PROXY",
        "no_proxy",
        "PLY_CHECKPOINT_DIR",
    ];
    let previous: Vec<_> = keys.iter().map(std::env::var_os).collect();
    unsafe {
        std::env::set_var(keys[0], "jev-fixture-secret");
        std::env::set_var(keys[1], "127.0.0.1,localhost");
        std::env::set_var(keys[2], "127.0.0.1,localhost");
        std::env::set_var(keys[3], state.path());
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (server_requests, server_stop) = (requests.clone(), stop.clone());
    let server = std::thread::spawn(move || {
        let mut handlers = Vec::new();
        while !server_stop.load(Ordering::Relaxed) {
            let (mut socket, _) = match listener.accept() {
                Ok(client) => client,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            let requests = server_requests.clone();
            handlers.push(std::thread::spawn(move || {
                socket.set_nonblocking(false).unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut bytes = Vec::new();
                let (header_end, length) = loop {
                    let mut buf = [0;4096]; let n = socket.read(&mut buf).unwrap();
                    if n == 0 { return; } bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        assert!(headers.starts_with("POST /v1/systemone HTTP/1.1"));
                        assert!(headers.to_ascii_lowercase().contains("authorization: bearer jev-fixture-secret"));
                        let length: usize = headers.lines().find_map(|line| line.split_once(':').filter(|(k,_)| k.eq_ignore_ascii_case("content-length")).map(|(_,v)| v.trim().parse().unwrap())).unwrap();
                        break (end+4,length);
                    }
                };
                while bytes.len() < header_end+length {
                    let mut buf=[0;4096];let n=socket.read(&mut buf).unwrap();if n==0{return;}bytes.extend_from_slice(&buf[..n]);
                }
                let input: Value = serde_json::from_slice(&bytes[header_end..header_end+length]).unwrap();
                assert_eq!(input["model"], "jev-latest");
                assert!(input.get("messages").is_none() && input.get("stream").is_none());
                assert_eq!(input["state"]["execution_backend"], "ply-sh");
                assert!(!input.to_string().contains("jev-fixture-secret"));
                let questions=input["questions"].as_object().unwrap();assert_eq!(questions.len(),4);
                for question in questions.values() { assert_eq!(question["type"],"noul");assert!(question["criteria"].get("true").is_some() && question["criteria"].get("false").is_some()); }
                let command=input["state"]["command"].as_str().unwrap().to_owned();
                requests.lock().unwrap().push(command.clone());
                if command.contains("timeout") || command.contains("cancel") { std::thread::sleep(Duration::from_secs(2)); }
                let mut answers=json!({
                    "destructive":{"type":"noul","noul":0.01}, "sensitive_data":{"type":"noul","noul":0.01},
                    "security_change":{"type":"noul","noul":0.01}, "scope_escape":{"type":"noul","noul":0.01}
                });
                if command.contains("deny") { answers["destructive"]["noul"]=json!(0.97); }
                if command.contains("review") { answers["sensitive_data"]["noul"]=json!(0.5); }
                if command.contains("bad_answer") { answers["scope_escape"]["noul"]=json!(1.2); }
                if command.contains("missing_answer") { answers.as_object_mut().unwrap().remove("scope_escape"); }
                let mut response=json!({"model":"jev-1.14.0","answers":answers,"usage":{"input_tokens":120,"output_tokens":12}});
                if command.contains("no_usage") {response.as_object_mut().unwrap().remove("usage");}
                let (status, body)=if command.contains("api_error") { (401, "{\"error\":\"jev-fixture-secret\"}".to_string()) }
                    else if command.contains("bad_json") { (200, "not-json jev-fixture-secret".to_string()) }
                    else {(200,response.to_string())};
                let _=write!(socket,"HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            }));
        }
        for handler in handlers {
            handler.join().unwrap();
        }
    });
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let work=tempfile::tempdir().unwrap();let root=std::fs::canonicalize(work.path()).unwrap().display().to_string();
        let io=Io::new();io.mount(&root,Arc::new(DiskStorage::open(work.path()).unwrap()),MountMode::ReadWrite).unwrap();
        let mut config=KernelConfig::new(io,root);
        config.env.insert("JEV_BASE_URL".into(),base);
        config.env.insert("JEV_TIMEOUT_MS".into(),"1000".into());
        assert!(!config.env.contains_key("TYPESAFE_API_KEY"));
        let kernel=Kernel::new(config);let records=Arc::new(Records::default());kernel.add_hook(records.clone());
        for plugin in PluginSource::defaults() {
            let plugin=match plugin {
                PluginSource::Embedded{name,source} if name=="agent" => PluginSource::Embedded{name,source:source.replacen("import { request as modelRequest } from \"ply:models\";",FAKE_MODEL,1)},
                other=>other,
            };
            kernel.load(plugin).await.unwrap();
        }
        kernel.load(PluginSource::builtin("jev").unwrap()).await.unwrap();kernel.ready().await.unwrap();
        assert!(!kernel.catalog().iter().any(|tool|tool.name=="jev"));
        let output=kernel.call("bash",json!({"command":"echo allow > allowed"})).await.unwrap();assert_eq!(output["exit_code"],0);
        assert_eq!(std::fs::read_to_string(work.path().join("allowed")).unwrap(),"allow\n");
        for name in ["deny","review","bad_answer","missing_answer","no_usage","api_error","bad_json"] {
            let pending=kernel.call("bash",json!({"command":format!("echo {name} > {name}-marker")})).await.unwrap();
            assert_eq!(pending["status"],"approval_required");
            assert_eq!(pending["approval"]["request"]["assessments"][0]["requires_approval"],true);
            assert!(!pending.to_string().contains("jev-fixture-secret"));
            assert!(!work.path().join(format!("{name}-marker")).exists());
        }
        let before=requests.lock().unwrap().len();
        let oversized=kernel.call("bash",json!({"command":"x".repeat(20000)})).await.unwrap();
        assert_eq!(oversized["status"],"approval_required");
        assert!(oversized.to_string().contains("state_too_large"));
        assert_eq!(requests.lock().unwrap().len(),before);
        let output=kernel.call("agent",json!({"prompt":"guard allow","allowed_tools":["bash"],"max_delegation_depth":0})).await.unwrap();
        assert_eq!(output["text"],"done");assert_eq!(output["usage"]["calls"],2);assert_eq!(output["total_usage"]["calls"],3);
        assert_eq!(output["total_usage"]["input_tokens"],320);assert_eq!(output["total_usage"]["output_tokens"],22);assert_eq!(output["total_usage"]["total_tokens"],342);
        assert_eq!(output["total_usage"]["complete"],true);assert_eq!(output["total_usage"]["cache_read_tokens"],Value::Null);assert_eq!(output["total_usage"]["cache_hit_rate"],Value::Null);
        let error=kernel.call("agent",json!({"prompt":"guard allow","allowed_tools":["bash"],"max_turns":1})).await.unwrap_err();
        assert!(error.to_string().contains("resume_id"));
        let checkpoint=records.0.lock().unwrap().iter().rev().find_map(|event|match event {FiberEvent::Record{data,..} if data["type"]=="agent_checkpoint"=>Some(data["resume_id"].clone()),_=>None}).unwrap();
        let resumed=kernel.call("agent",json!({"resume_id":checkpoint})).await.unwrap();
        assert_eq!(resumed["previous_total_usage"]["calls"],2);assert_eq!(resumed["previous_total_usage"]["cache_read_tokens"],Value::Null);
        let timed=kernel.call("agent",json!({"prompt":"guard timeout","allowed_tools":["bash"]})).await.unwrap();
        assert_eq!(timed["status"],"approval_required");assert_eq!(timed["usage"]["calls"],1);assert_eq!(timed["total_usage"]["calls"],2);assert_eq!(timed["total_usage"]["missing_calls"],1);assert_eq!(timed["total_usage"]["complete"],false);assert_eq!(timed["total_usage"]["cache_hit_rate"],Value::Null);
        assert!(!work.path().join("agent-timeout").exists());
        let before=kernel.call_ctx("jevStatus",vec![]).await.unwrap();
        let root=kernel.wake("bash",json!({"command":"echo cancel > cancel-marker"})).unwrap();
        tokio::time::timeout(Duration::from_secs(3),async {while !requests.lock().unwrap().iter().any(|cmd|cmd.contains("cancel")){tokio::task::yield_now().await;}}).await.unwrap();
        assert!(kernel.cancel_fiber(&root));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let status=kernel.call_ctx("jevStatus",vec![]).await.unwrap();
        assert_eq!(status["missing_calls"].as_u64().unwrap(),before["missing_calls"].as_u64().unwrap()+1);
        assert_eq!(status["complete"],false);assert!(!work.path().join("cancel-marker").exists());
        let events=records.0.lock().unwrap();
        assert!(!serde_json::to_string(&*events).unwrap().contains("jev-fixture-secret"));
        assert!(events.iter().any(|event|matches!(event,FiberEvent::Spawn{fiber,..} if fiber.plugin=="jev" && fiber.event.as_deref()==Some("bash.beforeExecute"))));
        let completed:Vec<_>=events.iter().filter_map(|event|match event{FiberEvent::Record{data,..} if data["type"]=="jev_check"=>Some(data),_=>None}).collect();
        assert!(completed.iter().any(|data|data["result"]["decision"]=="deny"));assert!(completed.iter().any(|data|data["result"]["decision"]=="review"));
        assert!(completed.iter().any(|data|data["result"]["model"]=="jev-1.14.0"));
        drop(events);drop(kernel);
    });
    drop(runtime);
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
    for (key, value) in keys.into_iter().zip(previous) {
        unsafe {
            if let Some(value) = value {
                std::env::set_var(key, value)
            } else {
                std::env::remove_var(key)
            }
        }
    }
}
