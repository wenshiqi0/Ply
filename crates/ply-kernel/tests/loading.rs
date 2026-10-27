//! What the kernel owns about loading and retiring plugins (RFC 0012):
//! named imports order onLoad, onDispose closes what onLoad opened, and
//! plugin code sees capabilities only, never fibers or load state.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ply_kernel::{CleanupStatus, DiskStorage, Exit, FiberEvent, FiberHook, Io, Kernel, KernelConfig, KernelError, MountMode, PluginSource};
use serde_json::json;

#[derive(Default)]
struct Records(Mutex<Vec<FiberEvent>>);
impl FiberHook for Records {
    fn on_event(&self, event: &FiberEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}
impl Records {
    fn all(&self) -> Vec<FiberEvent> {
        self.0.lock().unwrap().clone()
    }
    /// (realm, owner, status) of every cleanup, in order.
    fn cleanups(&self) -> Vec<(String, Option<String>, CleanupStatus, Option<String>)> {
        self.all().into_iter().filter_map(|e| match e {
            FiberEvent::Cleanup { realm, owner, status, error, .. } => Some((realm, owner, status, error)),
            _ => None,
        }).collect()
    }
    /// The onDispose cleanups (no owning fiber): (realm, status).
    fn disposals(&self) -> Vec<(String, CleanupStatus)> {
        self.cleanups().into_iter().filter(|c| c.1.is_none()).map(|c| (c.0, c.2)).collect()
    }
    fn spawns(&self) -> usize {
        self.all().iter().filter(|e| matches!(e, FiberEvent::Spawn { .. })).count()
    }
}

struct World {
    kernel: Kernel,
    records: Arc<Records>,
    work: tempfile::TempDir,
}

fn world() -> World {
    let work = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(work.path()).unwrap().display().to_string();
    let io = Io::new();
    io.mount(&root, Arc::new(DiskStorage::open(work.path()).unwrap()), MountMode::ReadWrite).unwrap();
    let kernel = Kernel::new(KernelConfig::new(io, root));
    let records = Arc::new(Records::default());
    kernel.add_hook(records.clone());
    World { kernel, records, work }
}

fn source(name: &str, code: &str) -> PluginSource {
    PluginSource::Embedded { name: name.into(), source: code.into() }
}

impl World {
    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.work.path().join(file)).unwrap_or_default()
    }
    async fn wait_for_file(&self, file: &str) {
        let path = self.work.path().join(file);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !path.exists() { tokio::time::sleep(Duration::from_millis(5)).await; }
        }).await.unwrap_or_else(|_| panic!("{file} never appeared"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn named_imports_load_first_and_default_imports_do_not_order() {
    let w = world();
    // Loaded before `slow`; each looks at slow's state as its onLoad starts.
    w.kernel.load(source("direct", r#"import fs from "node:fs";
import { slow } from "ply:plugins";
let seen: unknown;
export async function onLoad() { seen = { done: fs.existsSync("slow-done"), value: slow.value ?? null }; return { seen: () => seen }; }
"#)).await.unwrap();
    // The named import sits in a module of its own; type-only imports add nothing.
    w.kernel.load(PluginSource::Bundled { name: "nested".into(), files: vec![
        ("main.ts".into(), r#"import fs from "node:fs";
import { slowValue } from "./deps/slow";
let seen: unknown;
export async function onLoad() { seen = { done: fs.existsSync("slow-done"), value: slowValue() }; return { seen: () => seen }; }
"#.into()),
        ("deps/slow.ts".into(), "import { slow } from \"ply:plugins\";\nexport const slowValue = () => slow.value ?? null;\n".into()),
    ] }).await.unwrap();
    w.kernel.load(source("free", r#"import fs from "node:fs";
import plugins from "ply:plugins";
import type { slow } from "ply:plugins";
import { type slow as again } from "ply:plugins";
let seen: unknown;
export async function onLoad() {
  const before = { done: fs.existsSync("slow-done"), value: plugins.slow.value ?? null };
  // A call still waits for slow's onLoad.
  const called = await plugins.slow({});
  seen = { before, after: { done: fs.existsSync("slow-done"), value: plugins.slow.value ?? null, called } };
  return { seen: () => seen };
}
"#)).await.unwrap();
    w.kernel.load(source("slow", r#"import fs from "node:fs";
export async function onLoad() { await new Promise(resolve => setTimeout(resolve, 300)); fs.writeFileSync("slow-done", "yes"); return { value: 7 }; }
export async function onHandle(input: {}) { return "called"; }
"#)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), w.kernel.ready()).await.expect("ready hung").unwrap();
    for name in ["direct", "nested"] {
        assert_eq!(w.kernel.call_plugin_method(name, "seen", vec![]).await.unwrap(), json!({ "done": true, "value": 7 }), "{name}");
    }
    assert_eq!(
        w.kernel.call_plugin_method("free", "seen", vec![]).await.unwrap(),
        json!({ "before": { "done": false, "value": null }, "after": { "done": true, "value": 7, "called": "called" } })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_dependency_or_an_import_cycle_fails_the_dependent() {
    let w = world();
    w.kernel.load(source("needs_bad", r#"import fs from "node:fs";
import { bad } from "ply:plugins";
export async function onLoad() { fs.writeFileSync("needs-bad-ran", String(typeof bad)); }
"#)).await.unwrap();
    w.kernel.load(source("bad", "export async function onLoad() { throw new Error(\"bad setup\"); }\n")).await.unwrap();
    w.kernel.load(source("ring_a", "import { ring_b } from \"ply:plugins\";\nexport async function onLoad() {}\n")).await.unwrap();
    w.kernel.load(source("ring_b", "import { ring_a } from \"ply:plugins\";\nexport async function onLoad() {}\n")).await.unwrap();
    // Importing itself by name is no dependency.
    w.kernel.load(source("selfish", "import { selfish } from \"ply:plugins\";\nexport async function onLoad() { return { value: 1 }; }\n")).await.unwrap();
    let err = tokio::time::timeout(Duration::from_secs(10), w.kernel.ready()).await.expect("ready hung").unwrap_err().to_string();
    assert!(err.contains("needs_bad: onLoad failed: imported plugin bad did not load: bad: onLoad failed: Error: bad setup"), "{err}");
    assert!(!w.work.path().join("needs-bad-ran").exists(), "a dependent's onLoad must not start");
    // Which of the two closes the ring depends on scheduling; both fail.
    assert!(err.contains("ring_a: onLoad failed") && err.contains("ring_b: onLoad failed"), "{err}");
    assert!(
        err.contains("plugin import cycle: ring_a -> ring_b -> ring_a") || err.contains("plugin import cycle: ring_b -> ring_a -> ring_b"),
        "{err}"
    );
    assert!(!err.contains("selfish"), "{err}");
    w.kernel.wait_loaded("selfish").await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribing_waits_for_a_declaration_later_in_the_owners_onload() {
    let w = world();
    w.kernel.load(source("listener", r#"import * as events from "ply:events";
const heard: unknown[] = [];
let missing: string | null = null;
export async function onLoad() {
  await events.on("announcer.later", event => { heard.push(event); return "heard"; });
  try { await events.on("announcer.never", () => null); } catch (error) { missing = (error as Error).message; }
  return { heard: () => heard, missing: () => missing };
}
"#)).await.unwrap();
    w.kernel.load(source("announcer", r#"import * as events from "ply:events";
let later: string;
export async function onLoad() { await new Promise(resolve => setTimeout(resolve, 200)); later = await events.define("later"); }
export async function onHandle(input: { n: number }) { return await events.emit(later, input); }
"#)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), w.kernel.ready()).await.expect("ready hung").unwrap();
    assert_eq!(w.kernel.call("announcer", json!({ "n": 1 })).await.unwrap(), json!(["heard"]));
    let missing = w.kernel.call_plugin_method("listener", "missing", vec![]).await.unwrap();
    assert!(missing.as_str().is_some_and(|m| m.contains("unknown event announcer.never")), "{missing}");
    let emitted = w.records.all().into_iter().find_map(|e| match e {
        FiberEvent::Plugin { id, name, plugin, data, .. } if name == "announcer.later" => Some((id, plugin, data)),
        _ => None,
    }).unwrap();
    assert_eq!(emitted.1, "announcer");
    // The envelope names the emitting plugin only.
    assert_eq!(
        w.kernel.call_plugin_method("listener", "heard", vec![]).await.unwrap(),
        json!([{ "id": emitted.0, "name": "announcer.later", "source": { "plugin": "announcer" }, "data": { "n": 1 } }])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_code_sees_no_fiber_ctx_lifecycle_or_load_state() {
    let w = world();
    w.kernel.load(source("other", "export async function onLoad() { return { value: 1 }; }\nexport function onHandle(input: {}) { return 1; }\n")).await.unwrap();
    w.kernel.load(source("probe", r#"import plugins, { other } from "ply:plugins";
export async function onHandle(input: {}) {
  const lifecycle = await import("ply:lifecycle").then(() => "imported", error => String(error.message));
  return {
    fiber: typeof fiber, ctx: typeof ctx, globals: ["fiber", "ctx"].filter(name => name in globalThis),
    loaded: [typeof plugins.other.loaded, typeof other.loaded, "loaded" in other], value: other.value, lifecycle,
  };
}
"#)).await.unwrap();
    w.kernel.ready().await.unwrap();
    let seen = w.kernel.call("probe", json!({})).await.unwrap();
    assert_eq!(seen["fiber"], "undefined");
    assert_eq!(seen["ctx"], "undefined");
    assert_eq!(seen["globals"], json!([]));
    assert_eq!(seen["loaded"], json!(["undefined", "undefined", false]));
    assert_eq!(seen["value"], 1);
    assert!(seen["lifecycle"].as_str().unwrap().contains("ply:lifecycle"), "{seen}");
    // A static import of the removed module fails the load itself.
    let err = w.kernel.load(source("legacy", "import { onDispose } from \"ply:lifecycle\";\nexport function onHandle(input: {}) { return typeof onDispose; }\n")).await.unwrap_err();
    assert!(matches!(&err, KernelError::Load(m) if m.contains("ply:lifecycle")), "{err}");
    assert!(!w.kernel.plugins().iter().any(|p| p.name == "legacy"));
    // Scripts see the same globals.
    let script = w.work.path().join("probe.ts");
    std::fs::write(&script, "console.log(typeof fiber, typeof ctx);\n").unwrap();
    let outcome = w.kernel.run_script(&script, vec![]).await.unwrap();
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    assert_eq!(outcome.output.stdout, "undefined undefined\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn close_cleans_up_running_calls_then_disposes_each_plugin_once_last_loaded_first() {
    let w = world();
    let log = |name: &str| format!("export async function onDispose() {{ fs.appendFileSync(\"order.log\", \"dispose {name}\\n\"); }}\n");
    w.kernel.load(source("busy", &format!(r#"import fs from "node:fs";
export async function onHandle(input: {{}}) {{
  fs.writeFileSync("busy-entered", "yes");
  try {{ await new Promise(resolve => setTimeout(resolve, 10000)); }}
  finally {{ fs.appendFileSync("order.log", "finally busy\n"); }}
}}
{}"#, log("busy")))).await.unwrap();
    // Loaded third, second, first, they finish onLoad first, second, third.
    for (name, import) in [("third", "second"), ("second", "first"), ("first", "")] {
        let import = if import.is_empty() { String::new() } else { format!("import {{ {import} }} from \"ply:plugins\";\n") };
        w.kernel.load(source(name, &format!("import fs from \"node:fs\";\n{import}export async function onLoad() {{}}\n{}", log(name)))).await.unwrap();
    }
    w.kernel.ready().await.unwrap();
    let running = w.kernel.wake("busy", json!({})).unwrap();
    w.wait_for_file("busy-entered").await;
    // Parked on its timer, so what close cancels is a waiting call.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(10), w.kernel.close()).await.expect("close hung");
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    assert_eq!(w.read("order.log"), "finally busy\ndispose third\ndispose second\ndispose first\ndispose busy\n");
    assert!(matches!(w.records.all().iter().find_map(|e| match e {
        FiberEvent::Exit { id, exit, .. } if *id == running => Some(exit.clone()),
        _ => None,
    }), Some(Exit::Cancelled { .. })));
    let status = |realm: &str, owner: Option<&str>, status| (realm.to_string(), owner.map(str::to_string), status, None);
    use CleanupStatus::{Done, Started};
    assert_eq!(w.records.cleanups(), [
        status("busy", Some(&running), Started), status("busy", Some(&running), Done),
        status("third", None, Started), status("third", None, Done),
        status("second", None, Started), status("second", None, Done),
        status("first", None, Started), status("first", None, Done),
        status("busy", None, Started), status("busy", None, Done),
    ]);

    // Closing again disposes nothing twice, and nothing new starts.
    w.kernel.close().await;
    assert_eq!(w.records.disposals().len(), 8);
    let spawns = w.records.spawns();
    let err = w.kernel.call("busy", json!({})).await.unwrap_err();
    assert_eq!(err, KernelError::Cancelled("the kernel is closing".into()));
    assert_eq!(w.records.spawns(), spawns);
    assert_eq!(w.read("order.log").lines().count(), 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn dispose_failures_and_timeouts_are_reported_and_do_not_stop_the_others() {
    let w = world();
    // They finish onLoad fine, throws, hangs: disposal runs hangs, throws, fine.
    w.kernel.load(source("hangs", "import { throws } from \"ply:plugins\";\nexport async function onLoad() {}\nexport async function onDispose() { await new Promise(() => {}); }\n")).await.unwrap();
    w.kernel.load(source("throws", "import { fine } from \"ply:plugins\";\nexport async function onLoad() {}\nexport async function onDispose() { throw new Error(\"dispose failed\"); }\n")).await.unwrap();
    w.kernel.load(source("fine", "import fs from \"node:fs\";\nexport async function onLoad() {}\nexport function onHandle(input: {}) { return 1; }\nexport async function onDispose() { fs.writeFileSync(\"fine-disposed\", \"yes\"); }\n")).await.unwrap();
    w.kernel.ready().await.unwrap();
    assert_eq!(w.kernel.call("fine", json!({})).await.unwrap(), json!(1));
    let started = Instant::now();
    let closing = tokio::spawn({ let kernel = w.kernel.clone(); async move { kernel.close().await } });
    // While `hangs` is being disposed, fine is still loaded but nothing new starts.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(w.kernel.call("fine", json!({})).await.unwrap_err(), KernelError::Cancelled("the kernel is closing".into()));
    tokio::time::timeout(Duration::from_secs(10), closing).await.expect("close hung").unwrap();
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_secs(2) && elapsed < Duration::from_secs(5), "{elapsed:?}");
    assert_eq!(w.read("fine-disposed"), "yes");
    use CleanupStatus::{Done, Failed, Started, TimedOut};
    let disposals: Vec<(String, CleanupStatus, Option<String>)> =
        w.records.cleanups().into_iter().map(|(realm, owner, status, error)| { assert_eq!(owner, None); (realm, status, error) }).collect();
    let statuses: Vec<(&str, CleanupStatus)> = disposals.iter().map(|d| (d.0.as_str(), d.1)).collect();
    assert_eq!(statuses, [("hangs", Started), ("hangs", TimedOut), ("throws", Started), ("throws", Failed), ("fine", Started), ("fine", Done)]);
    assert!(disposals[3].2.as_deref().is_some_and(|e| e.contains("dispose failed")), "{disposals:?}");
    assert!(disposals[1].2.is_some(), "{disposals:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_onload_disposes_and_retires_its_realm_once() {
    let w = world();
    w.kernel.load(source("fragile", r#"import fs from "node:fs";
export async function onLoad() { console.log("loading fragile"); throw new Error("bad setup"); }
export async function onDispose() { fs.appendFileSync("fragile.log", "disposed\n"); }
"#)).await.unwrap();
    w.kernel.load(source("steady", "export async function onLoad() {}\nexport function onHandle(input: {}) { return \"steady\"; }\n")).await.unwrap();
    let err = w.kernel.ready().await.unwrap_err().to_string();
    assert!(err.contains("fragile: onLoad failed: Error: bad setup"), "{err}");
    w.wait_for_file("fragile.log").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while w.records.disposals().len() < 2 { tokio::time::sleep(Duration::from_millis(5)).await; }
    }).await.unwrap();
    assert_eq!(w.records.disposals(), [("fragile".to_string(), CleanupStatus::Started), ("fragile".to_string(), CleanupStatus::Done)]);
    // Its realm is gone, with what it printed.
    assert_eq!(w.kernel.take_output("fragile").await.unwrap_or_default().stdout, "");
    assert_eq!(w.kernel.call("steady", json!({})).await.unwrap(), json!("steady"));
    w.kernel.close().await;
    assert_eq!(w.read("fragile.log"), "disposed\n");
    let fragile: Vec<_> = w.records.disposals().into_iter().filter(|d| d.0 == "fragile").collect();
    assert_eq!(fragile.len(), 2);
    assert!(w.records.disposals().contains(&("steady".to_string(), CleanupStatus::Done)));
}
