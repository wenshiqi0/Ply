//! The JS outlet end to end: TypeScript plugins with node APIs over the IO
//! layer, isolation, scheduling, fibers sharing a realm, and the host-side
//! checks that hold even when plugin code tampers with the realm.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use wendy_jsc::{Caller, CleanupStatus, DiskStorage, FiberEvent, HostBridge, Io, JsRuntime, Limits, MemoryStorage, ModuleSource, MountMode, RealmConfig, RealmId};

struct World {
    rt: JsRuntime,
    io: Io,
    code: Io,
    data: tempfile::TempDir,
    plugins: tempfile::TempDir,
    root: String,
}

fn world() -> World {
    let io = Io::new();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(data.path().join("notes.txt"), "line one\nline two\n").unwrap();
    std::fs::create_dir(data.path().join("sub")).unwrap();
    std::fs::write(data.path().join("sub/a.json"), r#"{"n": 41}"#).unwrap();
    io.mount("/work", Arc::new(DiskStorage::open(data.path()).unwrap()), MountMode::ReadWrite).unwrap();
    io.mount("/tmp", Arc::new(MemoryStorage::empty()), MountMode::ReadWrite).unwrap();
    io.mount("/ro", Arc::new(MemoryStorage::empty()), MountMode::ReadOnly).unwrap();
    // Plugin code lives in its own read-only namespace, mounted at its host
    // path so module ids read like files.
    let plugins = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(plugins.path()).unwrap().display().to_string();
    let code = Io::new();
    code.mount(&root, Arc::new(DiskStorage::open(plugins.path()).unwrap()), MountMode::ReadOnly).unwrap();
    World { rt: JsRuntime::start(), io, code, data, plugins, root }
}

impl World {
    fn plugin(&self, name: &str, source: &str) -> String {
        let path = self.plugins.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        format!("{}/{name}", self.root)
    }

    async fn realm_with(&self, name: &str, limits: Limits, bridge: Option<Arc<dyn HostBridge>>) -> RealmId {
        self.rt
            .create_realm(RealmConfig {
                name: name.into(),
                io: self.io.clone(),
                cwd: "/work".into(),
                env: BTreeMap::from([("HOME".into(), "/work".into()), ("MODE".into(), "test".into())]),
                argv: vec!["main.ts".into()],
                modules: ModuleSource { io: self.code.clone(), root: self.root.clone() },
                limits,
                bridge,
            })
            .await
            .unwrap()
    }

    async fn realm(&self, name: &str) -> RealmId {
        self.realm_with(name, Limits::default(), None).await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_realm_reaps_pending_calls_and_never_reuses_its_id() {
    let w = world();
    let realm = w.realm("temporary").await;
    let call = w.rt.eval(realm, "setTimeout(() => console.log('late'), 10000); new Promise(() => {})");
    tokio::pin!(call);
    tokio::select! {
        result = &mut call => panic!("call should be pending: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(20)) => {}
    }
    tokio::time::timeout(Duration::from_secs(2), w.rt.close_realm(realm)).await.unwrap().unwrap().unwrap();
    assert!(call.await.is_err());
    assert!(w.rt.eval(realm, "1").await.is_err());
    let next = w.realm("replacement").await;
    assert_ne!(realm, next);
    assert_eq!(w.rt.eval(next, "2").await.unwrap(), json!(2));
    w.rt.close_realm(next).await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_realm_interrupts_a_blocking_exec() {
    let w = world();
    let main = w.plugin("blocking.ts", r#"
import { execFileSync } from "node:child_process";
export function block() { execFileSync("/bin/sh", ["-c", "printf started > entered; exec /bin/sleep 30"]); }
"#);
    let realm = w.realm("blocking").await;
    w.rt.run_main(realm, &main).await.unwrap();
    let call = w.rt.call(realm, &main, "block", vec![]);
    tokio::pin!(call);
    let started = async {
        while !w.data.path().join("entered").exists() { tokio::time::sleep(Duration::from_millis(5)).await; }
    };
    tokio::select! {
        result = &mut call => panic!("exec should still be running: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(3), started) => result.unwrap(),
    }
    tokio::time::timeout(Duration::from_secs(2), w.rt.close_realm(realm)).await.unwrap().unwrap().unwrap();
    assert!(call.await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_typescript_with_node_apis_over_the_mounts() {
    let w = world();
    let main = w.plugin(
        "main.ts",
        r#"
import fs from "node:fs";
import { readFile, writeFile, readdir } from "node:fs/promises";
import * as path from "node:path";
import { summarize } from "./lib/summary.ts";

interface Report { lines: number; total: number }

export async function run(target: string): Promise<Report> {
  const text = fs.readFileSync(path.join(process.cwd(), target), "utf8");
  const json = JSON.parse(await readFile("sub/a.json", "utf8"));
  const report: Report = { lines: summarize(text), total: json.n + 1 };
  await writeFile("/tmp/report.json", JSON.stringify(report));
  fs.mkdirSync("out/deep", { recursive: true });
  fs.writeFileSync("out/deep/r.txt", Buffer.from("héllo"));
  console.log("entries:", await readdir("."), process.env.MODE, process.argv.slice(1));
  return report;
}
"#,
    );
    w.plugin("lib/summary.ts", "export const summarize = (t: string): number => t.trim().split('\\n').length;\n");

    let realm = w.realm("reporter").await;
    assert_eq!(w.rt.run_main(realm, &main).await.unwrap(), json!(["run"]));
    let report = w.rt.call(realm, &main, "run", vec![json!("notes.txt")]).await.unwrap();
    assert_eq!(report, json!({ "lines": 2, "total": 42 }));
    let out = w.rt.take_output(realm).await;
    assert_eq!(out.stdout, "entries: [ 'notes.txt', 'out', 'sub' ] test [ 'main.ts' ]\n");
    assert_eq!(std::fs::read_to_string(w.data.path().join("out/deep/r.txt")).unwrap(), "héllo");
    assert_eq!(w.io.read("/tmp/report.json").await.unwrap(), br#"{"lines":2,"total":42}"#);
}

#[tokio::test(flavor = "multi_thread")]
async fn io_errors_surface_as_node_errors() {
    let w = world();
    let main = w.plugin(
        "main.ts",
        r#"
import fs from "node:fs";
export async function attempt() {
  const results: Record<string, string> = {};
  for (const [name, f] of Object.entries({
    readOnly: () => fs.writeFileSync("/ro/x.txt", "x"),
    missing: () => fs.readFileSync("nope.txt"),
    outside: () => fs.writeFileSync("/etc/passwd", "x"),
    isdir: () => fs.rmSync("sub"),
  })) {
    try { f(); results[name] = "ok"; } catch (e: any) { results[name] = `${e.code} | ${e.message}`; }
  }
  try { await fs.promises.unlink("missing.txt"); } catch (e: any) { results.asyncMissing = e.code; }
  return results;
}
"#,
    );
    let realm = w.realm("prober").await;
    w.rt.run_main(realm, &main).await.unwrap();
    let r = w.rt.call(realm, &main, "attempt", vec![]).await.unwrap();
    assert_eq!(r["readOnly"], "EROFS | EROFS: read-only file system, open '/ro/x.txt'");
    assert_eq!(r["missing"], "ENOENT | ENOENT: no such file or directory, open 'nope.txt'");
    assert_eq!(r["outside"], "EACCES | EACCES: permission denied, open '/etc/passwd'");
    assert_eq!(r["isdir"], "EISDIR | EISDIR: illegal operation on a directory, rm 'sub'");
    assert_eq!(r["asyncMissing"], "ENOENT");
}

#[tokio::test(flavor = "multi_thread")]
async fn realms_are_isolated_but_share_mounts() {
    let w = world();
    let a = w.realm("a").await;
    let b = w.realm("b").await;
    w.rt.eval(a, "globalThis.secret = 'a-only'; require = undefined; 1").await.unwrap();
    assert_eq!(w.rt.eval(b, "typeof globalThis.secret").await.unwrap(), json!("undefined"));
    // No host function is reachable from plugin code.
    let globals = w.rt.eval(b, "Object.getOwnPropertyNames(globalThis).filter(n => /host|wendy/i.test(n))").await.unwrap();
    assert_eq!(globals, json!([]));
    let main = w.plugin("w.ts", "import fs from 'node:fs'; export const put = () => fs.writeFileSync('/tmp/shared', 'from a');");
    w.rt.run_main(a, &main).await.unwrap();
    w.rt.call(a, &main, "put", vec![]).await.unwrap();
    let main_b = w.plugin("r.ts", "import fs from 'node:fs'; export const get = () => fs.readFileSync('/tmp/shared', 'utf8');");
    w.rt.run_main(b, &main_b).await.unwrap();
    assert_eq!(w.rt.call(b, &main_b, "get", vec![]).await.unwrap(), json!("from a"));
}

#[tokio::test(flavor = "multi_thread")]
async fn async_work_interleaves_on_the_event_loop() {
    let w = world();
    let main = w.plugin(
        "sched.ts",
        r#"
const log: string[] = [];
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function worker(name: string, delays: number[]) {
  for (const d of delays) { await sleep(d); log.push(`${name}:${d}`); }
}
export async function race() {
  setTimeout(() => log.push("timeout:5"), 5);
  const t = setTimeout(() => log.push("cancelled"), 1);
  clearTimeout(t);
  queueMicrotask(() => log.push("micro"));
  let ticks = 0;
  const iv = setInterval(() => { if (++ticks === 3) clearInterval(iv); }, 2);
  await Promise.all([worker("a", [10, 30]), worker("b", [20, 5])]);
  log.push(`interval:${ticks}`);
  return log;
}
"#,
    );
    let realm = w.realm("sched").await;
    w.rt.run_main(realm, &main).await.unwrap();
    let log = w.rt.call(realm, &main, "race", vec![]).await.unwrap();
    assert_eq!(log, json!(["micro", "timeout:5", "a:10", "b:20", "b:5", "a:30", "interval:3"]));
}

/// Records which fiber each `plugins.x(...)` was awaited from.
#[derive(Default)]
struct Recorder(Mutex<Vec<(Option<String>, Value)>>);

#[async_trait::async_trait]
impl HostBridge for Recorder {
    async fn call(&self, caller: Caller, op: &str, args: Value) -> Result<Value, String> {
        assert_eq!(op, "kernel.spawn");
        self.0.lock().unwrap().push((caller.fiber, args.clone()));
        tokio::time::sleep(Duration::from_millis(10)).await;
        match args["plugin"].as_str() {
            Some("fails") => Err("it failed".into()),
            Some("panics") => panic!("intentional host bridge panic"),
            _ => Ok(json!({ "echo": args["input"] })),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fibers_share_the_realm_and_each_await_names_its_fiber() {
    let w = world();
    let recorder = Arc::new(Recorder::default());
    let main = w.plugin(
        "fib.ts",
        r#"import plugins from "wendy:plugins";

let calls = 0; // shared by every fiber of this plugin
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
export async function onHandle(input: { n: number; wait: number }) {
  calls++;
  await sleep(input.wait);                  // the fibers interleave here
  const lazy = plugins.other({ n: input.n }); // nothing runs yet
  const first = await lazy;
  const again = await lazy;                 // same handle: not run again
  let failed = "";
  try { await plugins.fails({}); } catch (e: any) { failed = e.message; }
  return { n: input.n, echo: first.echo, same: first === again, failed, calls };
}
"#,
    );
    let realm = w.realm_with("fib", Limits::default(), Some(recorder.clone())).await;
    let (slow, fast) = (json!({ "n": 1, "wait": 40 }), json!({ "n": 2, "wait": 5 }));
    let (a, b) = tokio::join!(w.rt.run_fiber(realm, &main, &slow, "fiber-a"), w.rt.run_fiber(realm, &main, &fast, "fiber-b"));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a, json!({ "n": 1, "echo": { "n": 1 }, "same": true, "failed": "it failed", "calls": 2 }));
    assert_eq!(b["echo"], json!({ "n": 2 }));
    let calls = recorder.0.lock().unwrap().clone();
    let mut seen: Vec<(String, String)> =
        calls.iter().map(|(fiber, args)| (fiber.clone().unwrap(), format!("{}{}", args["plugin"], args["input"]))).collect();
    seen.sort();
    assert_eq!(
        seen,
        [
            ("fiber-a".to_string(), r#""fails"{}"#.to_string()),
            ("fiber-a".to_string(), r#""other"{"n":1}"#.to_string()),
            ("fiber-b".to_string(), r#""fails"{}"#.to_string()),
            ("fiber-b".to_string(), r#""other"{"n":2}"#.to_string()),
        ]
    );
    // A module without `onHandle` cannot run a fiber.
    let plain = w.plugin("plain.ts", "export const x = 1;\n");
    let err = w.rt.run_fiber(realm, &plain, &json!({}), "fiber-c").await.unwrap_err();
    assert!(err.message.contains("does not export an onHandle function"), "{}", err.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn modules_cannot_leave_the_plugin_root_or_import_packages() {
    let w = world();
    let main = w.plugin(
        "main.ts",
        r#"
export async function probe() {
  const out: string[] = [];
  for (const spec of ["/etc/hosts", "../../../../etc/hosts", "../does-not-exist", "lodash", "node:child_process", "node:net", "wendy", "./missing"]) {
    try { await import(spec); out.push("loaded " + spec); } catch (e: any) { out.push(e.code); }
  }
  return out;
}
"#,
    );
    let realm = w.realm("m").await;
    w.rt.run_main(realm, &main).await.unwrap();
    let r = w.rt.call(realm, &main, "probe", vec![]).await.unwrap();
    // Outside the root is refused on the path alone, existing or not.
    assert_eq!(
        r,
        json!([
            "ERR_MODULE_OUTSIDE_ROOT",
            "ERR_MODULE_OUTSIDE_ROOT",
            "ERR_MODULE_OUTSIDE_ROOT",
            "ERR_MODULE_NOT_FOUND",
            "loaded node:child_process",
            "ERR_UNKNOWN_BUILTIN_MODULE",
            "ERR_MODULE_NOT_FOUND",
            "ERR_MODULE_NOT_FOUND"
        ])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_and_exit_codes_reach_the_host() {
    let w = world();
    let main = w.plugin(
        "e.ts",
        "export function boom(): never { throw new TypeError('bad input'); }\nexport function quit() { process.exit(3); }\n",
    );
    let realm = w.realm("e").await;
    w.rt.run_main(realm, &main).await.unwrap();
    let err = w.rt.call(realm, &main, "boom", vec![]).await.unwrap_err();
    assert_eq!(err.message, "TypeError: bad input");
    assert!(err.stack.unwrap().contains("boom"));
    let err = w.rt.call(realm, &main, "quit", vec![]).await.unwrap_err();
    assert_eq!(err.exit_code, Some(3));
    let bad = w.plugin("bad.ts", "export const = ;");
    let err = w.rt.run_main(realm, &bad).await.unwrap_err();
    assert!(err.message.contains("bad.ts"), "{}", err.message);
}

/// The escape found in the first JS sandbox: a plugin replaced the global
/// JSON.stringify the prelude used, and loaded a module from outside its
/// root.
#[tokio::test(flavor = "multi_thread")]
async fn monkeypatching_globals_does_not_reach_the_host() {
    let w = world();
    std::fs::create_dir_all(w.plugins.path().join("../outside-plugin-root")).unwrap();
    std::fs::write(w.plugins.path().join("../outside-plugin-root/secret.js"), "export const secret = 'host file';").unwrap();
    w.plugin("dummy.ts", "export const secret = 'dummy';");
    let main = w.plugin(
        "main.ts",
        r#"
export async function attack() {
  const real = JSON.stringify;
  JSON.stringify = (v: any) => real(v).replace("dummy.ts", "../outside-plugin-root/secret.js");
  (Object.prototype as any).toJSON = function () { return this; };
  const m: any = await import("./dummy.ts");
  return m.secret;
}
"#,
    );
    let realm = w.realm("attacker").await;
    w.rt.run_main(realm, &main).await.unwrap();
    assert_eq!(w.rt.call(realm, &main, "attack", vec![]).await.unwrap(), json!("dummy"));
}

/// Whatever the prelude sends, the host re-checks. These are the calls a
/// prelude rewritten by plugin code could make.
#[tokio::test(flavor = "multi_thread")]
async fn the_host_rejects_tampered_calls() {
    let w = world();
    let secret = w.plugins.path().join("../outside-plugin-root-2/secret.js");
    std::fs::create_dir_all(secret.parent().unwrap()).unwrap();
    std::fs::write(&secret, "export const secret = 1;").unwrap();
    w.plugin("main.ts", "export const x = 1;\n");
    let realm = w.realm("tampered").await;
    let escape = format!("{}/../outside-plugin-root-2/secret.js", w.root);
    let code = |r: Value| r["__error"]["code"].as_str().unwrap_or("").to_string();

    // Loading code wendy did not resolve, from anywhere.
    for id in [escape.as_str(), "/etc/hosts", &format!("{}/main.ts", w.root)] {
        assert_eq!(code(w.rt.raw_host_call(realm, "module.load", json!({ "id": id })).await), "ERR_WENDY_INVALID_CALL", "{id}");
    }
    // Resolving relative to an importer that was never loaded.
    let r = w.rt.raw_host_call(realm, "module.resolve", json!({ "spec": "./hosts", "from": "/etc/x.js" })).await;
    assert_eq!(code(r), "ERR_WENDY_INVALID_CALL");
    let r = w.rt.raw_host_call(realm, "module.resolve", json!({ "spec": "../outside-plugin-root-2/secret.js", "from": null })).await;
    assert_eq!(code(r), "ERR_MODULE_OUTSIDE_ROOT");
    // Settling a call that is not this realm's.
    let r = w.rt.raw_host_call(realm, "settle", json!({ "id": 999, "ok": true, "value": "forged" })).await;
    assert_eq!(code(r), "ERR_WENDY_INVALID_CALL");
    // An honest call still works.
    let r = w.rt.raw_host_call(realm, "fs.readFile", json!({ "path": "notes.txt", "encoding": "utf8" })).await;
    assert_eq!(r, json!("line one\nline two\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_blocked_realm_does_not_stall_the_others() {
    let w = Arc::new(world());
    let main = w.plugin(
        "p.ts",
        "import { execSync } from 'node:child_process';\nexport const slow = () => execSync('sleep 1; echo done', { encoding: 'utf8' });\nexport const ping = () => 'pong';\n",
    );
    let a = w.realm("slow").await;
    let b = w.realm("other").await;
    w.rt.run_main(a, &main).await.unwrap();
    w.rt.run_main(b, &main).await.unwrap();

    let held = {
        let (w, main) = (w.clone(), main.clone());
        tokio::spawn(async move { w.rt.call(a, &main, "slow", vec![]).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    // `slow` is blocked inside execSync; `other` answers anyway.
    let pong = tokio::time::timeout(Duration::from_millis(500), w.rt.call(b, &main, "ping", vec![])).await;
    assert_eq!(pong.expect("other realm stalled").unwrap(), json!("pong"));
    assert!(!held.is_finished());
    assert_eq!(held.await.unwrap().unwrap(), json!("done\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn runaway_turns_are_terminated_and_the_realm_survives() {
    let w = world();
    let main = w.plugin(
        "spin.ts",
        "export function spin() { while (true) {} }\nexport async function spinLater() { await null; for (;;) {} }\nexport const ok = () => 'still here';\n",
    );
    let limits = Limits { turn_cpu: Duration::from_millis(300), ..Limits::default() };
    let realm = w.realm_with("spinner", limits, None).await;
    let bystander = w.realm("bystander").await;
    w.rt.run_main(realm, &main).await.unwrap();
    w.rt.run_main(bystander, &main).await.unwrap();

    let err = w.rt.call(realm, &main, "spin", vec![]).await.unwrap_err();
    assert!(err.message.contains("terminated") && err.message.contains("CPU"), "{}", err.message);
    let err = w.rt.call(realm, &main, "spinLater", vec![]).await.unwrap_err();
    assert!(err.message.contains("terminated"), "{}", err.message);
    assert_eq!(w.rt.call(realm, &main, "ok", vec![]).await.unwrap(), json!("still here"));
    assert_eq!(w.rt.call(bystander, &main, "ok", vec![]).await.unwrap(), json!("still here"));
    assert!(w.rt.take_output(realm).await.stderr.contains("script terminated"));

    // The memory ceiling uses the same watchdog.
    let tiny = Limits { turn_cpu: Duration::from_secs(30), memory_mb: Some(1) };
    let greedy = w.realm_with("greedy", tiny, None).await;
    w.rt.run_main(greedy, &main).await.unwrap();
    let err = w.rt.call(greedy, &main, "spin", vec![]).await.unwrap_err();
    assert!(err.message.contains("memory"), "{}", err.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn unhandled_rejections_are_reported_and_idle_waits_for_timers() {
    let w = world();
    let main = w.plugin(
        "late.ts",
        r#"
import fs from "node:fs";
export function lose() { Promise.reject(new Error("nobody caught this")); return 1; }
setTimeout(() => fs.writeFileSync("/work/late.txt", "written after main"), 50);
"#,
    );
    let realm = w.realm("late").await;
    w.rt.run_main(realm, &main).await.unwrap();
    assert!(!w.data.path().join("late.txt").exists());
    tokio::time::timeout(Duration::from_secs(5), w.rt.idle(realm)).await.expect("never went idle");
    assert!(w.data.path().join("late.txt").exists());

    let error = w.rt.call(realm, &main, "lose", vec![]).await.unwrap_err();
    assert!(error.message.contains("nobody caught this"));
    let out = w.rt.take_output(realm).await;
    assert!(out.stderr.contains("Uncaught (in promise) Error: nobody caught this"), "{}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn child_processes_run_in_disk_mounts() {
    let w = world();
    let main = w.plugin(
        "proc.ts",
        r#"
import { execSync, execFile, exec, spawnSync, spawn } from "node:child_process";
import { promisify } from "node:util";
export async function run() {
  const out: Record<string, unknown> = {};
  out.sync = execSync("echo $((6 * 7))", { encoding: "utf8" });
  out.file = await new Promise((res) => execFile("printf", ["%s-%s", "a", "b"], (err, stdout) => res([err, stdout])));
  out.promisified = await promisify(exec)("echo hi >&2; echo out");
  try { execSync("exit 4"); } catch (e: any) { out.failed = [e.status, e.message.split("\n")[0]]; }
  const r = spawnSync("cat", [], { input: "piped", encoding: "utf8" });
  out.spawnSync = [r.status, r.stdout];
  try { spawn("ls"); } catch (e: any) { out.spawn = e.code; }
  try { execSync("true", { cwd: "/tmp" }); } catch (e: any) { out.memoryMount = e.code; }
  execSync("echo made > made.txt");
  return out;
}
"#,
    );
    let realm = w.realm("procs").await;
    w.rt.run_main(realm, &main).await.unwrap();
    let r = w.rt.call(realm, &main, "run", vec![]).await.unwrap();
    assert_eq!(r["sync"], "42\n");
    assert_eq!(r["file"], json!([null, "a-b"]));
    assert_eq!(r["promisified"], json!({ "stdout": "out\n", "stderr": "hi\n" }));
    assert_eq!(r["failed"], json!([4, "Command failed: exit 4"]));
    assert_eq!(r["spawnSync"], json!([0, "piped"]));
    assert_eq!(r["spawn"], "ERR_WENDY_UNSUPPORTED");
    assert_eq!(r["memoryMount"], "ENOTSUP");
    assert_eq!(std::fs::read_to_string(w.data.path().join("made.txt")).unwrap(), "made\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn fiber_shared_promises_keep_the_consumers_identity() {
    let w = world();
    let recorder = Arc::new(Recorder::default());
    let main = w.plugin("shared-owner.ts", r#"import plugins from "wendy:plugins";

let shared;
export async function onHandle(input) {
  shared ??= new Promise(resolve => setTimeout(resolve, 20));
  await shared;
  await plugins.other({ n: input.n, boundary: "await" });
  await shared.then(() => plugins.other({ n: input.n, boundary: "then" }));
  await Promise.all([shared, Promise.resolve(1)]);
  await plugins.other({ n: input.n, boundary: "all" });
  await new Promise(resolve => queueMicrotask(resolve));
  await plugins.other({ n: input.n, boundary: "microtask" });
  async function* values() { await shared; yield input.n; }
  for await (const n of values()) await plugins.other({ n, boundary: "iterator" });
  return input.n;
}
"#);
    let realm = w.realm_with("shared-owner", Limits::default(), Some(recorder.clone())).await;
    let (one, two) = (json!({ "n": 1 }), json!({ "n": 2 }));
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(w.rt.run_fiber(realm, &main, &one, "owner-a"), w.rt.run_fiber(realm, &main, &two, "owner-b"))
    }).await.expect("shared consumers hung");
    assert_eq!(result.0.unwrap(), json!(1)); assert_eq!(result.1.unwrap(), json!(2));
    let calls = recorder.0.lock().unwrap();
    assert_eq!(calls.len(), 10);
    for (owner, args) in calls.iter() {
        assert_eq!(owner.as_deref(), Some(if args["input"]["n"] == 1 { "owner-a" } else { "owner-b" }), "{args}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fiber_cpu_failure_does_not_fail_a_waiting_sibling() {
    let w = world();
    let main = w.plugin("fiber-isolation.ts", r#"
let completed = 0;
export async function onHandle(input) {
  if (input.spin) { await null; while (true) {} }
  await new Promise(resolve => setTimeout(resolve, 300));
  return ++completed;
}
"#);
    let limits = Limits { turn_cpu: Duration::from_millis(100), ..Limits::default() };
    let realm = w.realm_with("fiber-isolation", limits, None).await;
    let (slow, spin) = (json!({}), json!({ "spin": true }));
    let (waiting, spinning) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(w.rt.run_fiber(realm, &main, &slow, "waiting"), w.rt.run_fiber(realm, &main, &spin, "spinning"))
    }).await.expect("CPU failure stranded a sibling");
    assert_eq!(waiting.unwrap(), json!(1));
    assert_eq!(spinning.unwrap_err().kind, wendy_jsc::FailureKind::CpuLimit);
    assert_eq!(w.rt.run_fiber(realm, &main, &json!({}), "later").await.unwrap(), json!(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn fiber_errors_are_isolated_even_in_timers_and_host_tasks() {
    let w = world();
    let main = w.plugin("faults.ts", r#"import plugins from "wendy:plugins";

export async function onHandle(input) {
  if (input.mode === "throw") { await null; throw new Error("isolated throw"); }
  if (input.mode === "timer") setTimeout(() => { throw new Error("isolated timer"); }, 1);
  if (input.mode === "unhandled") Promise.reject(new Error("isolated rejection"));
  if (input.mode === "panic") await plugins.panics({});
  await new Promise(resolve => setTimeout(resolve, 60));
  return "healthy";
}
"#);
    let realm = w.realm_with("faults", Limits::default(), Some(Arc::new(Recorder::default()))).await;
    for mode in ["throw", "timer", "unhandled", "panic"] {
        let (good, bad) = (json!({}), json!({ "mode": mode }));
        let (healthy, failed) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(w.rt.run_fiber(realm, &main, &good, "healthy"), w.rt.run_fiber(realm, &main, &bad, "faulty"))
        }).await.expect("fault stranded a sibling");
        assert_eq!(healthy.unwrap(), json!("healthy"), "{mode}");
        assert!(failed.is_err(), "{mode}: {failed:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fiber_cancellation_rejects_shared_waiters_and_removes_owned_timers() {
    let w = world();
    let main = w.plugin("cancel-shared.ts", r#"
import fs from "node:fs";
let shared;
const babelHelpers = "a plugin may use this name";
export async function onHandle(input) {
  shared ??= new Promise(resolve => setTimeout(() => { fs.writeFileSync("leaked.txt", "bad"); resolve(); }, 200));
  try { await shared; return babelHelpers; } catch (error) { return "shared cancelled"; }
}
"#);
    let realm = w.realm("cancel-shared").await;
    let input = json!({});
    let mut owner = Box::pin(w.rt.run_fiber(realm, &main, &input, "owner"));
    tokio::select! { result = &mut owner => panic!("premature result: {result:?}"), _ = tokio::time::sleep(Duration::from_millis(20)) => {} }
    let mut waiter = Box::pin(w.rt.run_fiber(realm, &main, &input, "waiter"));
    tokio::select! { result = &mut waiter => panic!("premature waiter: {result:?}"), _ = tokio::time::sleep(Duration::from_millis(10)) => {} }
    drop(owner);
    let result = tokio::time::timeout(Duration::from_secs(2), waiter).await.expect("cancelled shared promise stranded its waiter").unwrap();
    assert_eq!(result, json!("shared cancelled"));
    tokio::time::sleep(Duration::from_millis(220)).await;
    assert!(!w.data.path().join("leaked.txt").exists());
    tokio::time::timeout(Duration::from_secs(1), w.rt.idle(realm)).await.expect("cancelled realm never became idle");
}

#[tokio::test(flavor = "multi_thread")]
async fn fiber_disposal_runs_after_cancellation_and_cpu_termination() {
    let w = world();
    let main = w.plugin("dispose.ts", r#"
import { onDispose } from "wendy:lifecycle";
let active = new Set();
export function count() { return active.size; }
export async function onHandle(input) {
  active.add(input.id);
  const unwatch = onDispose(async () => { await null; active.delete(input.id); });
  if (input.spin) { await null; while (true) {} }
  await new Promise(resolve => setTimeout(resolve, 200));
  active.delete(input.id); unwatch();
}
"#);
    let realm = w.realm_with("dispose", Limits { turn_cpu: Duration::from_millis(100), ..Limits::default() }, None).await;
    let input = json!({ "id": "cancelled" });
    let mut pending = Box::pin(w.rt.run_fiber(realm, &main, &input, "cancelled"));
    tokio::select! { result = &mut pending => panic!("premature: {result:?}"), _ = tokio::time::sleep(Duration::from_millis(20)) => {} }
    drop(pending);
    tokio::time::timeout(Duration::from_secs(2), w.rt.idle(realm)).await.unwrap();
    assert_eq!(w.rt.call(realm, &main, "count", vec![]).await.unwrap(), json!(0));
    let error = w.rt.run_fiber(realm, &main, &json!({ "id": "spin", "spin": true }), "spin").await.unwrap_err();
    assert_eq!(error.kind, wendy_jsc::FailureKind::CpuLimit);
    tokio::time::timeout(Duration::from_secs(2), w.rt.idle(realm)).await.unwrap();
    assert_eq!(w.rt.call(realm, &main, "count", vec![]).await.unwrap(), json!(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn fiber_cancellation_interrupts_cpu_and_blocking_exec_without_waiting_for_the_budget() {
    let w = world();
    let main = w.plugin("interrupt.ts", r#"
import fs from "node:fs";
import { execFileSync } from "node:child_process";
export async function onHandle(input) {
  if (input.mode === "cpu") { fs.writeFileSync("entered.txt", "cpu"); while (true) {} }
  if (input.mode === "exec") execFileSync("/bin/sh", ["-c", "printf exec > entered.txt; exec /bin/sleep 30"]);
  return "available";
}
"#);
    let realm = w.realm_with("interrupt", Limits { turn_cpu: Duration::from_secs(20), ..Limits::default() }, None).await;
    for mode in ["cpu", "exec"] {
        let _ = std::fs::remove_file(w.data.path().join("entered.txt"));
        let input = json!({ "mode": mode });
        let mut pending = Box::pin(w.rt.run_fiber(realm, &main, &input, mode));
        let entered = async {
            while !w.data.path().join("entered.txt").exists() { tokio::time::sleep(Duration::from_millis(2)).await; }
        };
        tokio::select! { result = &mut pending => panic!("premature: {result:?}"), result = tokio::time::timeout(Duration::from_secs(3), entered) => result.unwrap() }
        if mode == "cpu" {
            let mut queued = Box::pin(w.rt.eval(realm, "console.log('late cancelled work')"));
            tokio::select! { result = &mut queued => panic!("queued call ran during a CPU loop: {result:?}"), _ = tokio::time::sleep(Duration::from_millis(10)) => {} }
            drop(queued);
        }
        drop(pending);
        let result = tokio::time::timeout(Duration::from_secs(2), w.rt.run_fiber(realm, &main, &json!({}), "healthy")).await.expect("cancel waited for the 20s CPU / 30s exec limit").unwrap();
        assert_eq!(result, json!("available"));
        assert!(!w.rt.take_output(realm).await.stdout.contains("late cancelled work"));
    }
}


#[derive(Default)]
struct CleanupObserver(Mutex<Vec<FiberEvent>>);

#[async_trait::async_trait]
impl HostBridge for CleanupObserver {
    async fn call(&self, _: Caller, _: &str, _: Value) -> Result<Value, String> {
        Err("no kernel calls expected".into())
    }
    fn on_event(&self, event: FiberEvent) { self.0.lock().unwrap().push(event); }
}

#[tokio::test(flavor = "multi_thread")]
async fn disposal_deadline_interrupts_cpu_sync_exec_and_async_waits() {
    let w = world();
    let main = w.plugin("cleanup-deadline.ts", r#"
import fs from "node:fs";
import { execFileSync } from "node:child_process";
import { onDispose } from "wendy:lifecycle";
export function onHandle(input) {
  onDispose(async () => {
    fs.writeFileSync(input.mode + "-entered", "yes");
    if (input.mode === "cpu") while (true) {}
    if (input.mode === "exec") execFileSync("/bin/sleep", ["30"]);
    if (input.mode === "async") await new Promise(resolve => setTimeout(resolve, 30000));
    fs.writeFileSync(input.mode + "-late", "must not run");
  });
  onDispose(() => fs.writeFileSync(input.mode + "-tail", "must not run"));
  throw new Error("start disposal");
}
"#);
    let observer = Arc::new(CleanupObserver::default());
    let realm = w.realm_with("cleanup-deadline", Limits { turn_cpu: Duration::from_secs(20), ..Limits::default() }, Some(observer.clone())).await;
    let mut cleanup_ids = std::collections::HashSet::new();
    for mode in ["cpu", "exec", "async"] {
        let started = std::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(4), async {
            w.rt.run_fiber(realm, &main, &json!({ "mode": mode }), mode).await.unwrap_err();
            w.rt.idle(realm).await;
        }).await.expect("cleanup must not wait for the 20s CPU / 30s IO limit");
        assert!(started.elapsed() >= Duration::from_millis(1900));
        assert!(w.data.path().join(format!("{mode}-entered")).exists());
        assert!(!w.data.path().join(format!("{mode}-late")).exists());
        assert!(!w.data.path().join(format!("{mode}-tail")).exists());
        let events: Vec<_> = {
            let events = observer.0.lock().unwrap();
            events.iter().filter_map(|event| match event {
                FiberEvent::Cleanup { id, owner, status, error, .. } if owner.as_deref() == Some(mode) => Some((*id, *status, error.clone())),
                _ => None,
            }).collect()
        };
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0].1, CleanupStatus::Started);
        assert_eq!(events[1].1, CleanupStatus::TimedOut);
        assert_eq!(events[0].0, events[1].0);
        assert!(cleanup_ids.insert(events[0].0));
        assert!(events[1].2.as_ref().unwrap().contains("two-second deadline"));
        assert_eq!(w.rt.eval(realm, "'still available'").await.unwrap(), json!("still available"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn disposal_failures_are_observed_and_do_not_skip_later_callbacks() {
    let w = world();
    let main = w.plugin("cleanup-errors.ts", r#"
import fs from "node:fs";
import { onDispose } from "wendy:lifecycle";
export function onHandle(input) {
  onDispose(() => { if (input.fail) throw new Error("first cleanup failed"); });
  onDispose(async () => { await null; fs.writeFileSync(input.name, "second cleanup ran"); });
  throw new Error("start disposal");
}
"#);
    let observer = Arc::new(CleanupObserver::default());
    let realm = w.realm_with("cleanup-errors", Limits::default(), Some(observer.clone())).await;
    for (name, fail, expected) in [("failed", true, CleanupStatus::Failed), ("done", false, CleanupStatus::Done)] {
        w.rt.run_fiber(realm, &main, &json!({ "name": name, "fail": fail }), name).await.unwrap_err();
        tokio::time::timeout(Duration::from_secs(1), w.rt.idle(realm)).await.unwrap();
        assert!(w.data.path().join(name).exists());
        let events = observer.0.lock().unwrap();
        let events: Vec<_> = events.iter().filter_map(|event| match event {
            FiberEvent::Cleanup { owner, status, error, .. } if owner.as_deref() == Some(name) => Some((*status, error.clone())),
            _ => None,
        }).collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, CleanupStatus::Started);
        assert_eq!(events[1].0, expected);
        if fail { assert!(events[1].1.as_ref().unwrap().contains("first cleanup failed")); }
        else { assert_eq!(events[1].1, None); }
    }
}


#[tokio::test(flavor = "multi_thread")]
async fn successful_disposal_retires_pending_timers() {
    let w = world();
    let main = w.plugin("cleanup-complete.ts", r#"
import fs from "node:fs";
import { onDispose } from "wendy:lifecycle";
export function onHandle() {
  onDispose(() => {
    setInterval(() => fs.writeFileSync("interval-late", "leaked"), 1000);
    setTimeout(() => fs.writeFileSync("timeout-late", "leaked"), 5000);
    new Promise(resolve => setTimeout(resolve, 5000)).then(() => fs.writeFileSync("promise-late", "leaked"));
  });
  throw new Error("start disposal");
}
"#);
    let observer = Arc::new(CleanupObserver::default());
    let realm = w.realm_with("cleanup-complete", Limits::default(), Some(observer.clone())).await;
    w.rt.run_fiber(realm, &main, &json!({}), "owner").await.unwrap_err();
    tokio::time::timeout(Duration::from_secs(1), w.rt.idle(realm)).await.expect("completed cleanup left timer resources alive");
    for file in ["interval-late", "timeout-late", "promise-late"] {
        assert!(!w.data.path().join(file).exists());
    }
    let statuses: Vec<_> = observer.0.lock().unwrap().iter().filter_map(|event| match event {
        FiberEvent::Cleanup { owner, status, .. } if owner.as_deref() == Some("owner") => Some(*status),
        _ => None,
    }).collect();
    assert_eq!(statuses, vec![CleanupStatus::Started, CleanupStatus::Done]);
    assert_eq!(w.rt.eval(realm, "'available'").await.unwrap(), json!("available"));
}


#[tokio::test(flavor = "multi_thread")]
async fn successful_disposal_cancels_unawaited_host_tasks() {
    use std::sync::atomic::{AtomicBool, Ordering};
    #[derive(Default)]
    struct WaitingHost { started: AtomicBool, active: AtomicBool, events: Mutex<Vec<FiberEvent>> }
    struct Active<'a>(&'a AtomicBool);
    impl Drop for Active<'_> { fn drop(&mut self) { self.0.store(false, Ordering::SeqCst); } }
    #[async_trait::async_trait]
    impl HostBridge for WaitingHost {
        async fn call(&self, _: Caller, _: &str, args: Value) -> Result<Value, String> {
            if args["plugin"] == "ready" {
                while !self.started.load(Ordering::SeqCst) { tokio::task::yield_now().await; }
                return Ok(Value::Null);
            }
            self.active.store(true, Ordering::SeqCst);
            let _active = Active(&self.active);
            self.started.store(true, Ordering::SeqCst);
            std::future::pending().await
        }
        fn on_event(&self, event: FiberEvent) { self.events.lock().unwrap().push(event); }
    }
    let w = world();
    let main = w.plugin("cleanup-host.ts", r#"import plugins from "wendy:plugins";

import { onDispose } from "wendy:lifecycle";
export function onHandle() {
  onDispose(async () => {
    plugins.never({}).catch(() => {});
    await plugins.ready({});
  });
  throw new Error("start disposal");
}
"#);
    let host = Arc::new(WaitingHost::default());
    let realm = w.realm_with("cleanup-host", Limits::default(), Some(host.clone())).await;
    w.rt.run_fiber(realm, &main, &json!({}), "owner").await.unwrap_err();
    if tokio::time::timeout(Duration::from_secs(1), w.rt.idle(realm)).await.is_err() {
        panic!("completed cleanup left a host task pending: started={} active={} output={:?}", host.started.load(Ordering::SeqCst), host.active.load(Ordering::SeqCst), w.rt.take_output(realm).await);
    }
    assert!(host.started.load(Ordering::SeqCst));
    tokio::time::timeout(Duration::from_secs(1), async {
        while host.active.load(Ordering::SeqCst) { tokio::task::yield_now().await; }
    }).await.expect("host task cancellation did not drop its future");
    let statuses: Vec<_> = host.events.lock().unwrap().iter().filter_map(|event| match event {
        FiberEvent::Cleanup { status, .. } => Some(*status),
        _ => None,
    }).collect();
    assert_eq!(statuses, vec![CleanupStatus::Started, CleanupStatus::Done]);
}
