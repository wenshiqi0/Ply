//! The runtime's Web globals (RFC 0013): URL from the Rust `url` crate,
//! Headers, streams, and fetch over Rust HTTP against a loopback server.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use ply_jsc::{DiskStorage, Io, JsRuntime, Limits, ModuleSource, MountMode, RealmConfig, RealmId};

/// One request per connection: `/echo` describes the request, `/redirect`
/// sends it to the same server under another origin, `/stream?n=` writes n
/// chunks slowly, `/large` is 17 MiB.
fn serve(listener: TcpListener) {
    let port = listener.local_addr().unwrap().port();
    for socket in listener.incoming() {
        let Ok(mut socket) = socket else { return };
        std::thread::spawn(move || {
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            let header_end = loop {
                let n = socket.read(&mut buf).unwrap_or(0);
                if n == 0 { return; }
                request.extend_from_slice(&buf[..n]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") { break end + 4; }
            };
            let head = String::from_utf8_lossy(&request[..header_end]).to_string();
            let header = |name: &str| head.lines().find_map(|line| line.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()));
            let length: usize = header("content-length").map_or(0, |v| v.parse().unwrap());
            while request.len() < header_end + length {
                let n = socket.read(&mut buf).unwrap();
                request.extend_from_slice(&buf[..n]);
            }
            let mut line = head.lines().next().unwrap().split(' ');
            let (method, target) = (line.next().unwrap().to_string(), line.next().unwrap().to_string());
            let reply = |socket: &mut TcpStream, status: &str, extra: &str, body: &[u8]| {
                let _ = write!(socket, "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = socket.write_all(body);
            };
            match target.split('?').next().unwrap() {
                "/echo" => {
                    let body = json!({ "method": method, "body": String::from_utf8_lossy(&request[header_end..]),
                        "type": header("content-type"), "auth": header("authorization") });
                    reply(&mut socket, "200 OK", "Content-Type: application/json\r\n", body.to_string().as_bytes());
                }
                "/redirect" => reply(&mut socket, "302 Found", &format!("Location: http://localhost:{port}/echo\r\n"), b""),
                "/stream" => {
                    let n: usize = target.split("n=").nth(1).unwrap().parse().unwrap();
                    let _ = write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {n}\r\nConnection: close\r\n\r\n");
                    for _ in 0..n {
                        if socket.write_all(b"x").and_then(|_| socket.flush()).is_err() { return; }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
                "/large" => reply(&mut socket, "200 OK", "", &vec![b'a'; 17 << 20]),
                _ => reply(&mut socket, "404 Not Found", "", b""),
            }
        });
    }
}

fn server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    std::thread::spawn(move || serve(listener));
    base
}

/// A realm whose module root holds `main.ts`, working on `work` at /work.
async fn realm(rt: &JsRuntime, plugin: &std::path::Path, work: &std::path::Path, source: &str) -> (RealmId, String) {
    let root = std::fs::canonicalize(plugin).unwrap().display().to_string();
    std::fs::write(plugin.join("main.ts"), source).unwrap();
    let code = Io::new();
    code.mount(&root, Arc::new(DiskStorage::open(plugin).unwrap()), MountMode::ReadOnly).unwrap();
    let io = Io::new();
    io.mount("/work", Arc::new(DiskStorage::open(work).unwrap()), MountMode::ReadWrite).unwrap();
    let realm = rt.create_realm(RealmConfig {
        name: "web".into(), io, cwd: "/work".into(), env: BTreeMap::new(), argv: vec![],
        modules: ModuleSource { io: code, root: root.clone() }, limits: Limits::default(), bridge: None,
    }).await.unwrap();
    (realm, format!("{root}/main.ts"))
}

#[tokio::test(flavor = "multi_thread")]
async fn web_globals_follow_the_platform() {
    let base = server();
    let (plugin, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let rt = JsRuntime::start();
    let (realm, main) = realm(&rt, plugin.path(), work.path(), r#"
export async function probe(base: string) {
  const out: Record<string, unknown> = {};
  const u = new URL("../a/b?x=1&y=2#h", "https://user:pw@example.com:8080/p/q");
  u.searchParams.append("z", "a b");
  out.url = { href: u.href, origin: u.origin, host: u.host, pathname: u.pathname, search: u.search, hash: u.hash };
  u.search = "?k=v";
  u.port = "443";
  out.params = { entries: [...u.searchParams], href: u.href };
  out.invalid = (() => { try { new URL("not a url"); return "parsed"; } catch (e: any) { return `${e.name}:${e.code}`; } })();
  out.form = new URLSearchParams({ q: "ä &", n: "1" }).toString();

  const h = new Headers({ "Content-Type": "text/plain" });
  h.append("x-a", "1"); h.append("X-A", "2"); h.append("set-cookie", "a=1"); h.append("set-cookie", "b=2");
  out.headers = { a: h.get("x-a"), cookies: h.getSetCookie(), entries: [...h] };

  const res = await fetch(base + "/echo", { method: "POST", body: JSON.stringify({ hi: 1 }),
    headers: { "content-type": "application/json", authorization: "Bearer t" } });
  out.echo = { status: res.status, ok: res.ok, json: await res.json(), type: res.headers.get("content-type"), used: res.bodyUsed };
  const redirected = await fetch(base + "/redirect", { headers: { authorization: "Bearer t" } });
  out.redirect = { redirected: redirected.redirected, url: redirected.url.replace(/:\d+/, ":port"), body: await redirected.json() };

  const streamed = await fetch(base + "/stream?n=3");
  const copy = streamed.clone();
  const chunks: string[] = [];
  const decoder = new TextDecoder();
  for await (const chunk of streamed.body as any) chunks.push(decoder.decode(chunk));
  out.stream = { joined: chunks.join(""), copy: await copy.text() };

  const controller = new AbortController();
  const slow = await fetch(base + "/stream?n=100", { signal: controller.signal });
  const reader = slow.body!.getReader();
  await reader.read();
  controller.abort();
  out.abort = await reader.read().then(() => "read", (e: any) => e.name);
  out.abortBefore = await fetch(base + "/echo", { signal: AbortSignal.abort() }).then(() => "sent", (e: any) => e.name);
  out.large = await fetch(base + "/large").then(r => r.arrayBuffer()).then(() => "read", (e: any) => `${e.name}:${e.message}:${e.cause?.message}`);
  out.refused = await fetch("http://127.0.0.1:1/").then(() => "ok", (e: any) => `${e.name}:${e.message}`);
  out.scheme = await fetch("file:///etc/hosts").then(() => "ok", (e: any) => e.name);
  return out;
}
"#).await;
    rt.run_main(realm, &main).await.unwrap();
    let out = rt.call(realm, &main, "probe", vec![json!(base)]).await.unwrap();
    assert_eq!(out["url"], json!({ "href": "https://user:pw@example.com:8080/a/b?x=1&y=2&z=a+b#h", "origin": "https://example.com:8080",
        "host": "example.com:8080", "pathname": "/a/b", "search": "?x=1&y=2&z=a+b", "hash": "#h" }));
    assert_eq!(out["params"], json!({ "entries": [["k", "v"]], "href": "https://user:pw@example.com/a/b?k=v#h" }));
    assert_eq!(out["invalid"], "TypeError:ERR_INVALID_URL");
    assert_eq!(out["form"], "q=%C3%A4+%26&n=1");
    assert_eq!(out["headers"], json!({ "a": "1, 2", "cookies": ["a=1", "b=2"],
        "entries": [["content-type", "text/plain"], ["set-cookie", "a=1"], ["set-cookie", "b=2"], ["x-a", "1, 2"]] }));
    assert_eq!(out["echo"], json!({ "status": 200, "ok": true, "type": "application/json", "used": true,
        "json": { "method": "POST", "body": "{\"hi\":1}", "type": "application/json", "auth": "Bearer t" } }));
    // Credentials never follow a redirect to another origin.
    assert_eq!(out["redirect"], json!({ "redirected": true, "url": "http://localhost:port/echo",
        "body": { "method": "GET", "body": "", "type": Value::Null, "auth": Value::Null } }));
    assert_eq!(out["stream"], json!({ "joined": "xxx", "copy": "xxx" }));
    assert_eq!(out["abort"], "AbortError");
    assert_eq!(out["abortBefore"], "AbortError");
    assert_eq!(out["large"], "TypeError:terminated:HTTP response exceeds 16 MiB");
    assert!(out["refused"].as_str().unwrap().starts_with("TypeError:fetch failed"), "{}", out["refused"]);
    assert_eq!(out["scheme"], "TypeError");
    rt.close_realm(realm).await.unwrap().unwrap();
}

/// A cancelled call's fetch ends with the call's AbortError, and its catch
/// still runs (RFC 0012).
#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_call_aborts_its_fetch() {
    let base = server();
    let (plugin, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let rt = JsRuntime::start();
    let (realm, main) = realm(&rt, plugin.path(), work.path(), r#"
import fs from "node:fs";
export async function onHandle(input: { base: string }) {
  const response = await fetch(input.base + "/stream?n=400");
  fs.writeFileSync("reading", "yes");
  try { await response.text(); fs.writeFileSync("outcome", "finished"); }
  catch (e: any) { fs.writeFileSync("outcome", `${e.name}:${e.code}`); }
}
"#).await;
    rt.run_main(realm, &main).await.unwrap();
    let input = json!({ "base": base });
    let mut call = Box::pin(rt.run_fiber(realm, &main, &input, "reader"));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !work.path().join("reading").exists() {
            tokio::select! { result = &mut call => panic!("premature: {result:?}"), _ = tokio::time::sleep(Duration::from_millis(10)) => {} }
        }
    }).await.unwrap();
    drop(call);
    tokio::time::timeout(Duration::from_secs(3), async {
        while !work.path().join("outcome").exists() { tokio::time::sleep(Duration::from_millis(10)).await; }
    }).await.unwrap();
    assert_eq!(std::fs::read_to_string(work.path().join("outcome")).unwrap(), "AbortError:ERR_PLY_CANCELLED");
    rt.close_realm(realm).await.unwrap().unwrap();
}
