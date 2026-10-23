//! The JS outlet end to end: TypeScript plugins, node APIs over wendy-io,
//! traced IO, gate denials as node errors, isolation, scheduling, and the
//! host-side checks that hold even when plugin code tampers with the realm.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use wendy_io::{Access, AllowAll, Answer, Approver, Gate, Io, IoRequest, MountMode, Policy, Rule, RuleAction};
use wendy_jsc::{JsRuntime, Limits, ModuleSource, RealmConfig, RealmId};
use wendy_storage::{DiskStorage, MemoryStorage};
use wendy_trace::{MemoryHook, Scope, TraceEvent, Tracer, Verdict};

struct World {
    rt: JsRuntime,
    io: Io,
    code: Io,
    hook: Arc<MemoryHook>,
    data: tempfile::TempDir,
    plugins: tempfile::TempDir,
    root: String,
}

fn world(gate: Arc<dyn Gate>) -> World {
    let tracer = Tracer::new();
    let hook = MemoryHook::new();
    tracer.add_hook(hook.clone());
    let io = Io::new(tracer.clone(), gate);
    let data = tempfile::tempdir().unwrap();
    std::fs::write(data.path().join("notes.txt"), "line one\nline two\n").unwrap();
    std::fs::create_dir(data.path().join("sub")).unwrap();
    std::fs::write(data.path().join("sub/a.json"), r#"{"n": 41}"#).unwrap();
    io.mount("/work", Arc::new(DiskStorage::open(data.path()).unwrap()), MountMode::ReadWrite).unwrap();
    io.mount("/tmp", Arc::new(MemoryStorage::empty()), MountMode::ReadWrite).unwrap();
    // Plugin code lives in its own read-only namespace, mounted at its host
    // path so module ids read like files.
    let plugins = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(plugins.path()).unwrap().display().to_string();
    let code = Io::new(tracer, Arc::new(AllowAll));
    code.mount(&root, Arc::new(DiskStorage::open(plugins.path()).unwrap()), MountMode::ReadOnly).unwrap();
    World { rt: JsRuntime::start(), io, code, hook, data, plugins, root }
}

impl World {
    fn plugin(&self, name: &str, source: &str) -> String {
        let path = self.plugins.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        format!("{}/{name}", self.root)
    }

    async fn realm_with(&self, name: &str, limits: Limits) -> RealmId {
        self.rt
            .create_realm(RealmConfig {
                name: name.into(),
                io: self.io.clone(),
                cwd: "/work".into(),
                env: BTreeMap::from([("HOME".into(), "/work".into()), ("MODE".into(), "test".into())]),
                argv: vec!["main.ts".into()],
                modules: ModuleSource { io: self.code.clone(), root: self.root.clone() },
                limits,
                bridge: None,
            })
            .await
            .unwrap()
    }

    async fn realm(&self, name: &str) -> RealmId {
        self.realm_with(name, Limits::default()).await
    }

    fn events(&self, kind: &str) -> Vec<TraceEvent> {
        self.hook.events().into_iter().filter(|e| e.kind == kind).collect()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_typescript_with_node_apis_and_traces_every_io() {
    let w = world(Arc::new(AllowAll));
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
    let exports = w.rt.run_main(realm, &main).await.unwrap();
    assert_eq!(exports, json!(["run"]));
    let report = w.rt.call(realm, &main, "run", vec![json!("notes.txt")]).await.unwrap();
    assert_eq!(report, json!({ "lines": 2, "total": 42 }));

    let out = w.rt.take_output(realm).await;
    assert_eq!(out.stdout, "entries: [ 'notes.txt', 'out', 'sub' ] test [ 'main.ts' ]\n");
    assert_eq!(std::fs::read_to_string(w.data.path().join("out/deep/r.txt")).unwrap(), "héllo");

    // Every IO is attributed to the realm and the node API that did it.
    let io: Vec<(String, String, String, String)> = w
        .hook
        .events()
        .into_iter()
        .filter(|e| e.scope == Scope::Io)
        .map(|e| {
            (
                e.data["actor"]["runtime"].as_str().unwrap().to_string(),
                e.data["actor"]["session"].as_str().unwrap().to_string(),
                e.data["actor"]["label"].as_str().unwrap().to_string(),
                format!("{} {}", e.data["op"].as_str().unwrap(), e.data["path"].as_str().unwrap()),
            )
        })
        .collect();
    let has = |label: &str, what: &str| io.iter().any(|(rt, s, l, w)| rt == "js" && s == "reporter" && l == label && w == what);
    assert!(has("fs.readFileSync", "read /work/notes.txt"), "{io:#?}");
    assert!(has("fs.promises.readFile", "read /work/sub/a.json"), "{io:#?}");
    assert!(has("fs.promises.writeFile", "write /tmp/report.json"), "{io:#?}");
    assert!(has("fs.mkdirSync", "mkdir /work/out/deep"), "{io:#?}");
    assert!(has("fs.writeFileSync", "write /work/out/deep/r.txt"), "{io:#?}");
    assert!(has("fs.promises.readdir", "list /work"), "{io:#?}");
    // Loading the plugin's own code is a traced read too.
    assert!(has("module.load", &format!("read {main}")), "{io:#?}");
    assert!(has("module.load", &format!("read {}/lib/summary.ts", w.root)), "{io:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_denials_surface_as_node_errors() {
    let policy = Policy::new()
        .default_write(RuleAction::Allow)
        .rule(Rule::new(Some(Access::Write), "/work/sub", RuleAction::Deny));
    let w = world(Arc::new(policy));
    let main = w.plugin(
        "main.ts",
        r#"
import fs from "node:fs";
export async function attempt() {
  const results: Record<string, string> = {};
  for (const [name, f] of Object.entries({
    denied: () => fs.writeFileSync("sub/x.txt", "x"),
    missing: () => fs.readFileSync("nope.txt"),
    outside: () => fs.writeFileSync("/etc/passwd", "x"),
    isdir: () => fs.rmSync("sub"),
  })) {
    try { f(); results[name] = "ok"; } catch (e: any) { results[name] = `${e.code} | ${e.message}`; }
  }
  try { await fs.promises.unlink("sub/a.json"); } catch (e: any) { results.asyncDenied = e.code; }
  return results;
}
"#,
    );
    let realm = w.realm("prober").await;
    w.rt.run_main(realm, &main).await.unwrap();
    let r = w.rt.call(realm, &main, "attempt", vec![]).await.unwrap();
    assert_eq!(r["denied"], "EACCES | EACCES: permission denied, open 'sub/x.txt'");
    assert_eq!(r["missing"], "ENOENT | ENOENT: no such file or directory, open 'nope.txt'");
    assert_eq!(r["outside"], "EACCES | EACCES: permission denied, open '/etc/passwd'");
    assert_eq!(r["isdir"], "EISDIR | EISDIR: illegal operation on a directory, rm 'sub'");
    assert_eq!(r["asyncDenied"], "EACCES");
    assert!(w.data.path().join("sub/a.json").exists());
    let denials = w.hook.events().into_iter().filter(|e| e.scope == Scope::Io && e.verdict == Verdict::Deny).count();
    assert_eq!(denials, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn realms_are_isolated_but_share_mounts() {
    let w = world(Arc::new(AllowAll));
    let a = w.realm("a").await;
    let b = w.realm("b").await;
    w.rt.eval(a, "globalThis.secret = 'a-only'; require = undefined; 1").await.unwrap();
    assert_eq!(w.rt.eval(b, "typeof globalThis.secret").await.unwrap(), json!("undefined"));
    // No host function is reachable from plugin code.
    let globals = w.rt.eval(b, "Object.getOwnPropertyNames(globalThis).filter(n => /host|wendy/i.test(n))").await.unwrap();
    assert_eq!(globals, json!([]));
    // But both see the same mounted storage.
    let main = w.plugin("w.ts", "import fs from 'node:fs'; export const put = () => fs.writeFileSync('/tmp/shared', 'from a');");
    w.rt.run_main(a, &main).await.unwrap();
    w.rt.call(a, &main, "put", vec![]).await.unwrap();
    let main_b = w.plugin("r.ts", "import fs from 'node:fs'; export const get = () => fs.readFileSync('/tmp/shared', 'utf8');");
    w.rt.run_main(b, &main_b).await.unwrap();
    assert_eq!(w.rt.call(b, &main_b, "get", vec![]).await.unwrap(), json!("from a"));
}

#[tokio::test(flavor = "multi_thread")]
async fn async_work_interleaves_on_the_event_loop() {
    let w = world(Arc::new(AllowAll));
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

#[tokio::test(flavor = "multi_thread")]
async fn modules_cannot_leave_the_plugin_root_or_import_packages() {
    let w = world(Arc::new(AllowAll));
    let main = w.plugin(
        "main.ts",
        r#"
export async function probe() {
  const out: string[] = [];
  for (const spec of ["/etc/hosts", "../../../../etc/hosts", "../does-not-exist", "lodash", "node:child_process", "node:net", "./missing"]) {
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
            "ERR_MODULE_NOT_FOUND"
        ])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn errors_and_exit_codes_reach_the_host() {
    let w = world(Arc::new(AllowAll));
    let main = w.plugin(
        "e.ts",
        "export function boom(): never { throw new TypeError('bad input'); }\nexport function quit() { process.exit(3); }\nexport const syntax = 1;\n",
    );
    let realm = w.realm("e").await;
    w.rt.run_main(realm, &main).await.unwrap();
    let err = w.rt.call(realm, &main, "boom", vec![]).await.unwrap_err();
    assert!(err.message.starts_with("TypeError: bad input"), "{}", err.message);
    let err = w.rt.call(realm, &main, "quit", vec![]).await.unwrap_err();
    assert_eq!(err.exit_code, Some(3));
    let bad = w.plugin("bad.ts", "export const = ;");
    let err = w.rt.run_main(realm, &bad).await.unwrap_err();
    assert!(err.message.contains("bad.ts"), "{}", err.message);
}

/// The two escapes found in the first JS sandbox: a plugin replaced the
/// global JSON.stringify the prelude used, and so (1) loaded a module from
/// outside its root and (2) rewrote the API label in the trace.
#[tokio::test(flavor = "multi_thread")]
async fn monkeypatching_globals_no_longer_reaches_the_host() {
    let w = world(Arc::new(AllowAll));
    std::fs::create_dir_all(w.plugins.path().join("../outside-plugin-root")).unwrap();
    let secret = w.plugins.path().join("../outside-plugin-root/secret.js");
    std::fs::write(&secret, "export const secret = 'host file';").unwrap();
    w.plugin("dummy.ts", "export const secret = 'dummy';");
    let main = w.plugin(
        "main.ts",
        r#"
import fs from "node:fs";
export async function attack() {
  const real = JSON.stringify;
  JSON.stringify = (v: any) => real(v).replace("dummy.ts", "../outside-plugin-root/secret.js").replace("fs.readFileSync", "forged-label");
  (Object.prototype as any).toJSON = function () { return this; };
  const m: any = await import("./dummy.ts");
  fs.readFileSync("notes.txt", "utf8");
  return m.secret;
}
"#,
    );
    let realm = w.realm("attacker").await;
    w.rt.run_main(realm, &main).await.unwrap();
    assert_eq!(w.rt.call(realm, &main, "attack", vec![]).await.unwrap(), json!("dummy"));
    let labels: Vec<Value> = w.hook.events().iter().filter(|e| e.scope == Scope::Io).map(|e| e.data["actor"]["label"].clone()).collect();
    assert!(labels.contains(&json!("fs.readFileSync")) && !labels.contains(&json!("forged-label")), "{labels:?}");
}

/// Whatever the prelude sends, the host re-checks. These are the calls a
/// prelude rewritten by plugin code could make.
#[tokio::test(flavor = "multi_thread")]
async fn the_host_rejects_tampered_calls_and_traces_them() {
    let w = world(Arc::new(AllowAll));
    let secret = w.plugins.path().join("../outside-plugin-root-2/secret.js");
    std::fs::create_dir_all(secret.parent().unwrap()).unwrap();
    std::fs::write(&secret, "export const secret = 1;").unwrap();
    let realm = w.realm("tampered").await;
    let escape = format!("{}/../outside-plugin-root-2/secret.js", w.root);

    let call = |op: &'static str, args: Value| {
        let rt = &w.rt;
        async move { rt.raw_host_call(realm, op, args).await }
    };
    // Loading code wendy did not resolve, from anywhere.
    for id in [escape.as_str(), "/etc/hosts", &format!("{}/main.ts", w.root)] {
        let r = call("module.load", json!({ "id": id })).await;
        assert_eq!(r["__error"]["code"], "ERR_WENDY_INVALID_CALL", "{id}: {r}");
    }
    // Resolving relative to an importer that was never loaded.
    let r = call("module.resolve", json!({ "spec": "./hosts", "from": "/etc/x.js" })).await;
    assert_eq!(r["__error"]["code"], "ERR_WENDY_INVALID_CALL");
    // A resolve that would leave the root fails before any storage is asked.
    let reads_before = w.events("io.stat").len();
    let r = call("module.resolve", json!({ "spec": "../outside-plugin-root-2/secret.js", "from": null })).await;
    assert_eq!(r["__error"]["code"], "ERR_MODULE_OUTSIDE_ROOT");
    assert_eq!(w.events("io.stat").len(), reads_before);
    // Forged trace labels, wrong-kind labels, foreign settles and phases.
    let r = call("fs.readFile", json!({ "path": "notes.txt", "api": "forged-label" })).await;
    assert_eq!(r["__error"]["code"], "ERR_WENDY_INVALID_CALL");
    let r = call("fs.readFile", json!({ "path": "notes.txt", "api": "fs.promises.readFile" })).await;
    assert_eq!(r["__error"]["code"], "ERR_WENDY_INVALID_CALL", "a sync call cannot claim an async API");
    let r = call("settle", json!({ "id": 999, "ok": true, "value": "forged" })).await;
    assert_eq!(r["__error"]["code"], "ERR_WENDY_INVALID_CALL");
    let r = call("ipo.phase", json!({ "op": "op-of-someone-else", "phase": "process" })).await;
    assert_eq!(r["__error"]["code"], "ERR_WENDY_INVALID_CALL");
    // An honest call still works.
    let r = call("fs.readFile", json!({ "path": "notes.txt", "encoding": "utf8", "api": "fs.readFileSync" })).await;
    assert_eq!(r, json!("line one\nline two\n"));

    let violations = w.events("js.violation");
    assert_eq!(violations.len(), 8, "{violations:#?}");
    assert!(violations.iter().all(|v| v.verdict == Verdict::Deny && v.data["realm"] == "tampered"));
    assert!(w.hook.events().iter().all(|e| e.data["actor"]["label"] != "forged-label"));
}

struct HeldApprover(tokio::sync::Semaphore);

#[async_trait::async_trait]
impl Approver for HeldApprover {
    async fn approve(&self, _req: &IoRequest) -> Answer {
        let _permit = self.0.acquire().await;
        Answer::Once
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_realm_waiting_on_an_approval_does_not_stall_the_others() {
    let approver = Arc::new(HeldApprover(tokio::sync::Semaphore::new(0)));
    let policy = Policy::new().default_write(RuleAction::Ask).approver(approver.clone());
    let w = Arc::new(world(Arc::new(policy)));
    let main = w.plugin(
        "p.ts",
        "import fs from 'node:fs';\nexport const save = () => { fs.writeFileSync('/work/held.txt', 'x'); return 'saved'; };\nexport const ping = () => 'pong';\n",
    );
    let a = w.realm("writer").await;
    let b = w.realm("other").await;
    w.rt.run_main(a, &main).await.unwrap();
    w.rt.run_main(b, &main).await.unwrap();

    let held = {
        let w = w.clone();
        let main = main.clone();
        tokio::spawn(async move { w.rt.call(a, &main, "save", vec![]).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    // `writer` is blocked inside writeFileSync; `other` answers anyway.
    let pong = tokio::time::timeout(Duration::from_secs(2), w.rt.call(b, &main, "ping", vec![])).await;
    assert_eq!(pong.expect("other realm stalled").unwrap(), json!("pong"));
    assert!(!held.is_finished());
    approver.0.add_permits(1);
    assert_eq!(held.await.unwrap().unwrap(), json!("saved"));
}

#[tokio::test(flavor = "multi_thread")]
async fn runaway_turns_are_terminated_and_the_realm_survives() {
    let w = world(Arc::new(AllowAll));
    let main = w.plugin(
        "spin.ts",
        "export function spin() { while (true) {} }\nexport async function spinLater() { await null; for (;;) {} }\nexport const ok = () => 'still here';\n",
    );
    let limits = Limits { turn_cpu: Duration::from_millis(300), ..Limits::default() };
    let realm = w.realm_with("spinner", limits).await;
    let bystander = w.realm("bystander").await;
    w.rt.run_main(realm, &main).await.unwrap();
    w.rt.run_main(bystander, &main).await.unwrap();

    let err = w.rt.call(realm, &main, "spin", vec![]).await.unwrap_err();
    assert!(err.message.contains("terminated") && err.message.contains("CPU"), "{}", err.message);
    let err = w.rt.call(realm, &main, "spinLater", vec![]).await.unwrap_err();
    assert!(err.message.contains("terminated"), "{}", err.message);
    assert_eq!(w.rt.call(realm, &main, "ok", vec![]).await.unwrap(), json!("still here"));
    assert_eq!(w.rt.call(bystander, &main, "ok", vec![]).await.unwrap(), json!("still here"));
    assert_eq!(w.events("js.terminated").len(), 2);
    assert!(w.events("js.violation").is_empty());

    // The memory ceiling uses the same watchdog.
    let tiny = Limits { turn_cpu: Duration::from_secs(30), memory_mb: Some(1) };
    let greedy = w.realm_with("greedy", tiny).await;
    w.rt.run_main(greedy, &main).await.unwrap();
    let err = w.rt.call(greedy, &main, "spin", vec![]).await.unwrap_err();
    assert!(err.message.contains("memory"), "{}", err.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn unhandled_rejections_are_reported_and_idle_waits_for_timers() {
    let w = world(Arc::new(AllowAll));
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
    let reported = w.events("js.unhandled_rejection");
    assert_eq!(reported.len(), 1);
    assert!(reported[0].data["reason"].as_str().unwrap().contains("nobody caught this"));
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread")]
async fn child_processes_go_through_the_io_layer() {
    let w = world(Arc::new(AllowAll));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let main = w.plugin(
        "proc.ts",
        r#"
import { execSync, execFile, exec, spawnSync, spawn } from "node:child_process";
import { promisify } from "node:util";
import { exec as wendyExec } from "wendy";
export async function run() {
  const out: Record<string, unknown> = {};
  out.sync = execSync("echo $((6 * 7))", { encoding: "utf8" });
  out.file = await new Promise((res) => execFile("printf", ["%s-%s", "a", "b"], (err, stdout) => res([err, stdout])));
  out.promisified = await promisify(exec)("echo hi >&2; echo out");
  try { execSync("exit 4", { stdio: "pipe" }); } catch (e: any) { out.failed = [e.status, e.message.split("\n")[0]]; }
  const r = spawnSync("cat", [], { input: "piped", encoding: "utf8" });
  out.spawnSync = [r.status, r.stdout];
  try { spawn("ls"); } catch (e: any) { out.spawn = e.code; }
  const w = await wendyExec("mkdir -p built && echo artifact > built/a.txt && echo done");
  out.wendy = [w.code, w.stdout, w.changes, w.changesComplete];
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
    assert_eq!(
        r["wendy"],
        json!([0, "done\n", [
            { "op": "mkdir", "path": "/work/built", "dir": true },
            { "op": "write", "path": "/work/built/a.txt", "dir": false }
        ], true])
    );
    let labels: Vec<String> = w.events("io.exec").iter().map(|e| e.data["actor"]["label"].as_str().unwrap().to_string()).collect();
    assert_eq!(
        labels,
        [
            "child_process.execSync",
            "child_process.execFile",
            "child_process.exec",
            "child_process.execSync",
            "child_process.spawnSync",
            "wendy.exec"
        ]
    );
}
