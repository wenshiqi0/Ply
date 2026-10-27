//! The agent's model connection end to end: its pi.ts, pi-ai from the
//! repository's node_modules, the runtime's fetch and Rust HTTP/SSE, against
//! a loopback fixture. No provider request, external runtime, or billable
//! tokens.
use std::{collections::BTreeMap, io::{Read, Write}, net::TcpListener, sync::{Arc, Mutex}, time::{Duration, Instant}};
use serde_json::{Value, json};
use ply_jsc::{Caller, DiskStorage, HostBridge, Io, JsRuntime, Limits, ModuleSource, MountMode, RealmConfig};

#[derive(Default)]
struct Records(Mutex<Vec<(String, String)>>);
#[async_trait::async_trait]
impl HostBridge for Records {
    async fn call(&self, caller: Caller, op: &str, args: Value) -> Result<Value, String> {
        // The host attributes each emitted event to the call that emitted it.
        assert_eq!((op, args["name"].as_str()), ("kernel.event_emit", Some("models.provider")));
        self.0.lock().unwrap().push((caller.fiber.unwrap(), args["data"]["provider"].as_str().unwrap().to_owned()));
        Ok(json!([]))
    }
}

#[test]
fn deepseek_and_mimo_stream_concurrently_with_their_own_identity() {
    let keys = ["NO_PROXY", "no_proxy"];
    let previous: Vec<_> = keys.iter().map(|key| std::env::var_os(key)).collect();
    // This binary has one synchronous test; set env before creating threads.
    unsafe {
        std::env::set_var(keys[0], "127.0.0.1,localhost");
        std::env::set_var(keys[1], "127.0.0.1,localhost");
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut models = Vec::new();
        while models.len() < 2 && Instant::now() < deadline {
            let (mut socket, _) = match listener.accept() {
                Ok(client) => client,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => { std::thread::sleep(Duration::from_millis(2)); continue; }
                Err(error) => panic!("{error}"),
            };
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = Vec::new();
            let (header_end, length) = loop {
                let mut buf = [0; 4096]; let n = socket.read(&mut buf).unwrap(); assert!(n > 0); request.extend_from_slice(&buf[..n]);
                assert!(request.len() < 1 << 20);
                if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..end]);
                    let length: usize = header.lines().find_map(|line| line.split_once(':').filter(|(key, _)| key.eq_ignore_ascii_case("content-length")).map(|(_, value)| value.trim().parse().unwrap())).unwrap();
                    break (end + 4, length);
                }
            };
            while request.len() < header_end + length {
                let mut buf = [0; 4096]; let n = socket.read(&mut buf).unwrap(); assert!(n > 0); request.extend_from_slice(&buf[..n]);
            }
            let input: Value = serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
            assert_eq!(input["stream"], true);
            // Each provider sends the key from its profile's variable.
            let header = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            let model = input["model"].as_str().unwrap().to_owned();
            let expected = if model.starts_with("deepseek") { "authorization: bearer deepseek-fixture-secret" } else { "api-key: mimo-fixture-secret" };
            assert!(header.contains(expected), "{model}: {header}");
            models.push(model);
            let chunks = [
                json!({"id":"offline","choices":[{"index":0,"delta":{"role":"assistant","content":"你好，架构评审。"},"finish_reason":null}]}),
                json!({"id":"offline","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"r1","type":"function","function":{"name":"read","arguments":"{\"path\":"}}]},"finish_reason":null}]}),
                json!({"id":"offline","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"README.md\"}"}}]},"finish_reason":null}]}),
                json!({"id":"offline","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":128,"completion_tokens":32,"prompt_cache_hit_tokens":96}}),
            ];
            let body = chunks.iter().map(|chunk| format!("data: {chunk}\n\n")).collect::<String>() + "data: [DONE]\n\n";
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            // Split inside UTF-8 and tool-argument boundaries.
            for bytes in body.as_bytes().chunks(7) { socket.write_all(bytes).unwrap(); socket.flush().unwrap(); }
        }
        models.sort(); models
    });
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        // A directory plugin next to a node_modules, like the kernel mounts
        // one, with the agent's own model module.
        let directory = tempfile::tempdir().unwrap();
        let project = std::fs::canonicalize(directory.path()).unwrap();
        let plugin = project.join("plugin");
        std::fs::create_dir(&plugin).unwrap();
        std::fs::copy(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/agent/pi.ts"), plugin.join("pi.ts")).unwrap();
        let root = plugin.display().to_string();
        let file = plugin.join("model.ts");
        std::fs::write(&file, r#"
import { request } from "./pi";
import * as events from "ply:events";
export async function onHandle(input) {
  const result = await request({ operation: "complete", profile: input.profile, purpose: "task", maxTokens: 128,
    context: { systemPrompt: "Offline fixture", messages: [{ role: "user", content: "Review", timestamp: 0 }],
      tools: [{ name: "read", description: "Read", parameters: { type: "object", properties: { path: { type: "string" } }, required: ["path"] } }] } });
  await events.emit("models.provider", { provider: input.profile.provider });
  return result;
}
"#).unwrap();
        let io = Io::new(); io.mount(&root, Arc::new(DiskStorage::open(&plugin).unwrap()), MountMode::ReadOnly).unwrap();
        let modules = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../node_modules");
        io.mount(&project.join("node_modules").display().to_string(), Arc::new(DiskStorage::open(modules).unwrap()), MountMode::ReadOnly).unwrap();
        let records = Arc::new(Records::default()); let js = JsRuntime::start();
        // Keys reach plugins through process.env (RFC 0013).
        let env = BTreeMap::from([("DEEPSEEK_API_KEY".into(), "deepseek-fixture-secret".into()), ("MIMO_API_KEY".into(), "mimo-fixture-secret".into())]);
        let realm = js.create_realm(RealmConfig { name: "models".into(), io: io.clone(), cwd: root.clone(), env, argv: vec![],
            modules: ModuleSource { io, root }, limits: Limits::default(), bridge: Some(records.clone()) }).await.unwrap();
        let profile = |provider: &str, model: &str, key: &str, header: Option<&str>| json!({ "profile": {
            "provider": provider, "model": model, "apiKeyEnv": key, "apiKeyHeader": header,
            "definition": { "id": model, "provider": provider, "name": model, "api": "openai-completions", "baseUrl": base,
                "reasoning": false, "input": ["text"], "contextWindow": 4096, "maxTokens": 512,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 } } } });
        let (ds, mimo) = (profile("deepseek", "deepseek-v4-flash", "DEEPSEEK_API_KEY", None),
            profile("xiaomi", "mimo-v2.6-pro", "MIMO_API_KEY", Some("api-key")));
        let entry = std::fs::canonicalize(&file).unwrap().display().to_string();
        let (a,b) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(js.run_fiber(realm, &entry, &ds, "ds"), js.run_fiber(realm, &entry, &mimo, "mimo"))
        }).await.expect("model call hung");
        for result in [a.unwrap(), b.unwrap()] {
            assert_eq!(result["stopReason"], "toolUse");
            let blocks = result["content"].as_array().unwrap();
            assert!(blocks.iter().any(|b| b["type"] == "text" && b["text"] == "你好，架构评审。"));
            assert!(blocks.iter().any(|b| b["type"] == "toolCall" && b["name"] == "read" && b["arguments"]["path"] == "README.md"));
            assert_eq!(result["usage"]["input"], 32); assert_eq!(result["usage"]["cacheRead"], 96);
            assert_eq!(result["usage"]["output"], 32); assert_eq!(result["usage"]["totalTokens"], 160);
            assert!(!result.to_string().contains("fixture-secret"));
        }
        let mut owners = records.0.lock().unwrap().clone(); owners.sort();
        assert_eq!(owners, [("ds".into(), "deepseek".into()), ("mimo".into(), "xiaomi".into())]);
        js.close_realm(realm).await.unwrap().unwrap();
    });
    drop(runtime);
    assert_eq!(server.join().unwrap(), ["deepseek-v4-flash", "mimo-v2.6-pro"]);
    for (key, value) in keys.into_iter().zip(previous) { unsafe { if let Some(value) = value { std::env::set_var(key, value); } else { std::env::remove_var(key); } } }
}
