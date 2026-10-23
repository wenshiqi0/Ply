//! The JS outlet end to end: TypeScript plugins with node APIs over the IO
//! layer, isolation, scheduling, fibers sharing a realm, and the host-side
//! checks that hold even when plugin code tampers with the realm.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use wendy_jsc::{Caller, DiskStorage, HostBridge, Io, JsRuntime, Limits, MemoryStorage, ModuleSource, MountMode, RealmConfig, RealmId};

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

/// Records which fiber each `tools.x(...)` was awaited from.
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
        r#"
let calls = 0; // shared by every fiber of this plugin
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
export async function handle(input: { n: number; wait: number }) {
  calls++;
  await sleep(input.wait);                  // the fibers interleave here
  const lazy = tools.other({ n: input.n }); // nothing runs yet
  const first = await lazy;
  const again = await lazy;                 // same handle: not run again
  let failed = "";
  try { await tools.fails({}); } catch (e: any) { failed = e.message; }
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
    // A module without `handle` is not a plugin.
    let plain = w.plugin("plain.ts", "export const x = 1;\n");
    let err = w.rt.run_fiber(realm, &plain, &json!({}), "fiber-c").await.unwrap_err();
    assert!(err.message.contains("does not export a handle function"), "{}", err.message);
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

    assert_eq!(w.rt.call(realm, &main, "lose", vec![]).await.unwrap(), json!(1));
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
