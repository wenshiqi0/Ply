//! The kernel end to end: plugins with onHandle and onLoad, fibers and
//! their records, ctx and loaded.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use ply_kernel::{
    DiskStorage, Exit, FiberEvent, FiberHook, Io, JsonlRecorder, Kernel, KernelConfig, KernelError, MountMode, PluginSource, Type,
};

#[derive(Default)]
struct Events(Mutex<Vec<FiberEvent>>);

impl FiberHook for Events {
    fn on_event(&self, event: &FiberEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

impl Events {
    fn all(&self) -> Vec<FiberEvent> {
        self.0.lock().unwrap().clone()
    }

    /// (id, parent, plugin, input) of every spawn.
    fn spawns(&self) -> Vec<(String, Option<String>, String, Value)> {
        self.all()
            .into_iter()
            .filter_map(|e| match e {
                FiberEvent::Spawn { fiber, .. } => Some((fiber.id, fiber.parent, fiber.plugin, fiber.input)),
                _ => None,
            })
            .collect()
    }

    fn exit_of(&self, id: &str) -> Option<Exit> {
        self.all().into_iter().find_map(|e| match e {
            FiberEvent::Exit { id: i, exit, .. } if i == id => Some(exit),
            _ => None,
        })
    }
}

struct World {
    kernel: Kernel,
    events: Arc<Events>,
    work: tempfile::TempDir,
    plugins: tempfile::TempDir,
    root: String,
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_scripts_retire_their_realm_and_cancel_its_timers() {
    let w = world_with(vec![]).await;
    let script = w.work.path().join("temporary.ts");
    std::fs::write(&script, "import fs from 'node:fs'; setTimeout(() => fs.writeFileSync('late.txt', 'leaked'), 100); throw new Error('stop');").unwrap();
    let outcome = w.kernel.run_script(&script, vec![]).await.unwrap();
    assert!(outcome.result.is_err());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!w.work.path().join("late.txt").exists());
}

/// A kernel over a temp workspace, with `plugins` loaded (not yet ready).
async fn world_with(plugins: Vec<PluginSource>) -> World {
    let work = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(work.path()).unwrap().display().to_string();
    std::fs::write(work.path().join("notes.txt"), "alpha\nbeta\ngamma\nbeta\n").unwrap();
    let io = Io::new();
    // The workspace is mounted at its own host path, so bash and the file
    // plugins see the same paths.
    io.mount(&root, Arc::new(DiskStorage::open(work.path()).unwrap()), MountMode::ReadWrite).unwrap();
    let kernel = Kernel::new(KernelConfig::new(io, root.clone()));
    let events = Arc::new(Events::default());
    kernel.add_hook(events.clone());
    for plugin in plugins {
        kernel.load(plugin).await.unwrap();
    }
    World { kernel, events, work, plugins: tempfile::tempdir().unwrap(), root }
}

/// The default plugins, loaded and ready.
async fn world() -> World {
    let w = world_with(PluginSource::defaults()).await;
    w.kernel.ready().await.unwrap();
    w
}

impl World {
    /// A plugin directory named `name` with `source` as its main.ts.
    fn plugin(&self, name: &str, source: &str) -> PluginSource {
        let dir = self.plugins.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.ts"), source).unwrap();
        PluginSource::Dir(dir)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn default_plugins_describe_themselves_and_work_on_the_workspace() {
    let w = world().await;
    let plugins = w.kernel.plugins();
    let names: Vec<(&str, bool)> = plugins.iter().map(|p| (p.name.as_str(), p.callable)).collect();
    assert_eq!(
        names,
        [("agent", true), ("bash", true), ("context", false), ("create", true), ("deepseek", false), ("edit", true), ("read", true), ("remove", true), ("skill", false)]
    );
    let catalog: Vec<String> = w.kernel.catalog().into_iter().map(|t| t.name).collect();
    assert_eq!(catalog, ["agent", "bash", "create", "edit", "read", "remove"]);

    let edit = plugins.iter().find(|p| p.name == "edit").unwrap();
    assert!(edit.description.starts_with("Replace an exact string in a file."), "{}", edit.description);
    let Type::Object { fields } = &edit.input else { panic!("{:?}", edit.input) };
    let described: Vec<(&str, bool, Option<&str>)> =
        fields.iter().map(|f| (f.name.as_str(), f.optional, f.description.as_deref())).collect();
    assert_eq!(
        described,
        [
            ("path", false, None),
            ("old_string", false, Some("Exact text to replace.")),
            ("new_string", false, Some("Replacement text.")),
            ("replace_all", true, Some("Replace every occurrence."))
        ]
    );

    let created = w.kernel.call("create", json!({ "path": "src/lib/new.txt", "content": "fresh\n" })).await.unwrap();
    assert_eq!(created, json!({ "path": format!("{}/src/lib/new.txt", w.root), "bytes": 6, "overwritten": false }));
    let err = w.kernel.call("create", json!({ "path": "notes.txt", "content": "x" })).await.unwrap_err();
    assert!(matches!(&err, KernelError::Fiber(m) if m.contains("already exists")), "{err}");

    let err = w.kernel.call("edit", json!({ "path": "notes.txt", "old_string": "beta", "new_string": "BETA" })).await.unwrap_err();
    assert_eq!(err.to_string(), "Error: old_string occurs 2 times in notes.txt; add context or set replace_all");
    let edited = w
        .kernel
        .call("edit", json!({ "path": "notes.txt", "old_string": "beta", "new_string": "$& B", "replace_all": true }))
        .await
        .unwrap();
    assert_eq!(edited["replacements"], 2);
    assert_eq!(std::fs::read_to_string(w.work.path().join("notes.txt")).unwrap(), "alpha\n$& B\ngamma\n$& B\n");

    let err = w.kernel.call("remove", json!({ "path": "src" })).await.unwrap_err();
    assert!(err.to_string().contains("set recursive"), "{err}");
    w.kernel.call("remove", json!({ "path": "src", "recursive": true })).await.unwrap();
    assert!(!w.work.path().join("src").exists());

    let bash = w.kernel.call("bash", json!({ "command": "echo made > made.txt; cat made.txt; echo oops >&2; exit 2" })).await.unwrap();
    assert_eq!(bash, json!({ "exit_code": 2, "signal": null, "timed_out": false, "stdout": "made\n", "stderr": "oops\n" }));
    let slow = w.kernel.call("bash", json!({ "command": "sleep 5", "timeout_ms": 200 })).await.unwrap();
    assert_eq!((slow["timed_out"].clone(), slow["signal"].clone()), (json!(true), Value::Null));

    assert_eq!(w.kernel.call("remove", json!({})).await.unwrap_err().to_string(), "Error: 'path' must be a non-empty string");
    let err = w.kernel.call("agent", json!({ "prompt": " " })).await.unwrap_err();
    assert_eq!(err.to_string(), "Error: 'prompt' must be a non-empty string");
    assert!(matches!(w.kernel.call("nope", json!({})).await, Err(KernelError::UnknownPlugin(_))));
    // Plugins without onHandle cannot be called.
    let err = w.kernel.call("context", json!({})).await.unwrap_err();
    assert_eq!(err.to_string(), "context has no onHandle; it cannot be called");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_default_tools_and_the_model_are_in_the_context() {
    let w = world().await;
    let model = w.kernel.call_ctx("currentModel", vec![]).await.unwrap();
    assert_eq!(model, json!({ "provider": "deepseek", "model": "deepseek-v4-flash", "apiKeyEnv": "DEEPSEEK_API_KEY" }));
    let id = w.kernel.call_ctx("openConversation", vec![json!("be brief")]).await.unwrap();
    let long = "x".repeat(50_000);
    let context = w
        .kernel
        .call_ctx(
            "appendMessages",
            vec![
                id.clone(),
                json!([
                    { "role": "user", "content": "hi" },
                    { "role": "toolResult", "content": [{ "type": "text", "text": long }] }
                ]),
            ],
        )
        .await
        .unwrap();
    assert_eq!(context["systemPrompt"], "be brief");
    let mut offered: Vec<&str> = context["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    offered.sort();
    assert_eq!(offered, ["agent", "bash", "create", "edit", "read", "remove"]);
    assert_eq!(context["tools"][0]["parameters"]["type"], "object");
    let trimmed = context["messages"][1]["content"][0]["text"].as_str().unwrap();
    assert!(trimmed.len() < 21_000 && trimmed.contains("[30000 characters omitted]"), "{}", trimmed.len());
    w.kernel.call_ctx("closeConversation", vec![id.clone()]).await.unwrap();
    let err = w.kernel.call_ctx("appendMessages", vec![id, json!([])]).await.unwrap_err();
    assert!(err.to_string().contains("no open conversation"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn onload_mounts_on_ctx_waits_on_others_and_later_mounts_win() {
    // `early` loads first and its onLoad waits for `late`, loaded after it.
    let w = world_with(vec![]).await;
    let early = w.plugin(
        "early",
        r#"import plugins from "ply:plugins";

let seen: string[] = [];
export async function onLoad() {
  await plugins.late.loaded;
  seen = await ctx.greet("early");
  return { seen: () => seen, shared: "from early" };
}
"#,
    );
    let late = w.plugin(
        "late",
        r#"
const greeted: string[] = [];
let counter = 0;
export async function onLoad() {
  return {
    version: 2,
    shared: "from late",
    greet(name: string) { greeted.push(name); return greeted; },
    bump() { return ++counter; },
  };
}
/** Uses what others mounted. */
export async function onHandle(input: null): Promise<{ shared: string; version: number; seen: string[]; bumps: number[] }> {
  const bumps = await Promise.all([ctx.bump(), ctx.bump()]);
  return { shared: ctx.shared, version: ctx.version, seen: await ctx.seen(), bumps };
}
"#,
    );
    w.kernel.load(early).await.unwrap();
    w.kernel.load(late).await.unwrap();
    w.kernel.ready().await.unwrap();
    let r = w.kernel.call("late", Value::Null).await.unwrap();
    // `early` finished its onLoad after `late`, so its `shared` wins.
    assert_eq!(r, json!({ "shared": "from early", "version": 2, "seen": ["early"], "bumps": [1, 2] }));

    // After ready, waiting for a plugin that is not there fails at once.
    let lonely = w.plugin("lonely", "import plugins from \"ply:plugins\";\nexport async function onLoad() { await plugins.ghost.loaded; }\n");
    w.kernel.load(lonely).await.unwrap();
    let err = tokio::time::timeout(Duration::from_secs(5), w.kernel.ready()).await.expect("ready hung").unwrap_err();
    assert!(err.to_string().contains("lonely: onLoad failed") && err.to_string().contains("no plugin named `ghost`"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn awaiting_a_plugin_is_a_child_fiber_and_every_fiber_is_recorded() {
    let w = world().await;
    w.kernel
        .load(w.plugin(
            "count_lines",
            r#"import plugins from "ply:plugins";

/** Count lines of a file two ways. */
export async function onHandle(input: { path: string }): Promise<{ lines: number; betas: number }> {
  const [wc, grep] = await Promise.all([
    plugins.bash({ command: `wc -l < ${input.path}` }),
    plugins.bash({ command: `grep -c beta ${input.path}` }),
  ]);
  return { lines: Number(wc.stdout.trim()), betas: Number(grep.stdout.trim()) };
}
"#,
        ))
        .await
        .unwrap();
    w.kernel.ready().await.unwrap();
    let r = w.kernel.call("count_lines", json!({ "path": "notes.txt" })).await.unwrap();
    assert_eq!(r, json!({ "lines": 4, "betas": 2 }));

    let spawns = w.events.spawns();
    let (outer, parent, _, input) = spawns.iter().find(|s| s.2 == "count_lines").unwrap().clone();
    assert_eq!((parent, input), (None, json!({ "path": "notes.txt" })));
    let children: Vec<_> = spawns.iter().filter(|s| s.1.as_deref() == Some(outer.as_str())).collect();
    let child_plugins: Vec<&str> = children.iter().map(|s| s.2.as_str()).collect();
    assert_eq!(child_plugins, ["bash", "bash"]);
    for child in &children {
        assert!(matches!(w.events.exit_of(&child.0), Some(Exit::Done { .. })));
    }
    assert_eq!(w.events.exit_of(&outer), Some(Exit::Done { output: r }));

    // A failure is recorded with its error.
    let _ = w.kernel.call("remove", json!({ "path": "missing.txt" })).await.unwrap_err();
    let failed = w.events.spawns().into_iter().rev().find(|s| s.2 == "remove").unwrap().0;
    let Some(Exit::Failed { error, .. }) = w.events.exit_of(&failed) else { panic!() };
    assert_eq!(error, "Error: missing.txt: no such file or directory");

    // The host may start a fiber nobody awaits.
    let woken = w.kernel.wake("create", json!({ "path": "woken.txt", "content": "later" })).unwrap();
    for _ in 0..100 {
        if w.events.exit_of(&woken).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(std::fs::read_to_string(w.work.path().join("woken.txt")).unwrap(), "later");
    let detached = w.events.all().into_iter().any(|e| matches!(e, FiberEvent::Spawn { fiber, .. } if fiber.id == woken && fiber.detached));
    assert!(detached);
}

#[tokio::test(flavor = "multi_thread")]
async fn loaded_publishes_ctx_before_waking_dependants() {
    let w = world_with(vec![]).await;
    for n in 0..12 {
        w.kernel
            .load(w.plugin(
                &format!("waiter_{n}"),
                &format!(
                    "import plugins from \"ply:plugins\";\nexport async function onLoad() {{ await plugins.owner.loaded; const value = await ctx.identify({n}); return {{ seen_{n}: () => value }}; }}\n"
                ),
            ))
            .await
            .unwrap();
    }
    w.kernel
        .load(w.plugin("owner", "export async function onLoad() { await new Promise(r => setTimeout(r, 20)); return { identify: (n: number) => n + 100 }; }\n"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), w.kernel.ready()).await.expect("onLoad waiters stalled").unwrap();
    for n in 0..12 {
        assert_eq!(w.kernel.call_ctx(&format!("seen_{n}"), vec![]).await.unwrap(), json!(n + 100));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fibers_of_one_plugin_run_concurrently_and_share_its_closures() {
    let w = world_with(vec![]).await;
    w.kernel
        .load(w.plugin(
            "counter",
            r#"
let seen = 0;        // shared by every fiber of this plugin
let running = 0;
let overlap = 0;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
export async function onHandle(input: { wait: number }): Promise<{ seen: number; overlap: number }> {
  seen++;
  running++;
  overlap = Math.max(overlap, running);
  await sleep(input.wait);
  running--;
  return { seen, overlap };
}
"#,
        ))
        .await
        .unwrap();
    w.kernel.ready().await.unwrap();
    let tasks: Vec<_> = (0..5)
        .map(|i| {
            let kernel = w.kernel.clone();
            tokio::spawn(async move { kernel.call("counter", json!({ "wait": 50 + i })).await.unwrap() })
        })
        .collect();
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.unwrap());
    }
    assert!(results.iter().all(|r| r["seen"] == 5 && r["overlap"] == 5), "{results:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_that_collide_or_are_not_plugins_do_not_load() {
    let w = world().await;
    let err = w.kernel.load(w.plugin("edit", "export const onHandle = () => 1;\n")).await.unwrap_err();
    assert_eq!(err.to_string(), "a plugin named `edit` is already loaded");
    let err = w.kernel.load(w.plugin("plain", "export const x = 1;\n")).await.unwrap_err();
    assert!(err.to_string().contains("exports neither an `onHandle` nor an `onLoad`"), "{err}");
    let err = w.kernel.load(w.plugin("bad name", "export const onHandle = () => 1;\n")).await.unwrap_err();
    assert!(err.to_string().contains("must match"), "{err}");
    let err = w.kernel.load(PluginSource::Dir(w.plugins.path().join("missing"))).await.unwrap_err();
    assert!(matches!(err, KernelError::Load(_)));
    let err = w.kernel.load(w.plugin("broken", "export function onHandle() { return 1 }\nthrow new Error('module init failed');\n")).await.unwrap_err();
    assert!(err.to_string().contains("module init failed"), "{err}");
    assert_eq!(w.kernel.plugins().len(), 9);

    // Every realm lists the loaded plugins; each callable one says what it is as a tool.
    w.kernel
        .load(w.plugin(
            "lister",
            "import plugins from \"ply:plugins\";\n/** List tools. */\nexport const onHandle = (name: string) => ({ names: Object.keys(plugins), has: name in plugins, description: plugins[name].description, wraps: plugins.lister.wrapsInput, params: plugins.lister.parameters, contextParams: plugins.context.parameters ?? null });\n",
        ))
        .await
        .unwrap();
    let listed = w.kernel.call("lister", json!("edit")).await.unwrap();
    assert_eq!(listed["names"], json!(["agent", "bash", "context", "create", "deepseek", "edit", "lister", "read", "remove", "skill"]));
    assert_eq!(listed["has"], true);
    assert!(listed["description"].as_str().unwrap().starts_with("Replace an exact string"));
    assert_eq!(listed["wraps"], true);
    assert_eq!(listed["params"], json!({ "type": "object", "properties": { "input": { "type": "string" } }, "required": ["input"] }));
    assert_eq!(listed["contextParams"], Value::Null);

    w.kernel.load(w.plugin("forever", "import plugins from \"ply:plugins\";\nexport const onHandle = (): Promise<unknown> => plugins.forever({});\n")).await.unwrap();
    let err = w.kernel.call("forever", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("nested deeper than 16"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn structured_records_use_the_calling_fibers_identity_and_ancestry() {
    let w = world_with(vec![]).await;
    w.kernel
        .load(w.plugin(
            "noter",
            "export async function onHandle() { const info = await fiber.info(); await fiber.record({ type: 'note', id: 'forged' }); return info; }\n",
        ))
        .await
        .unwrap();
    w.kernel
        .load(w.plugin("parent", "import plugins from \"ply:plugins\";\nexport async function onHandle() { return await plugins.noter({}); }\n"))
        .await
        .unwrap();
    w.kernel.ready().await.unwrap();
    let info = w.kernel.call("parent", json!({})).await.unwrap();
    let spawned = w.events.spawns();
    let parent = spawned.iter().find(|s| s.2 == "parent").unwrap();
    let child = spawned.iter().find(|s| s.2 == "noter").unwrap();
    assert_eq!(info["id"], child.0);
    assert_eq!(info["parent"], parent.0);
    assert_eq!(info["ancestors"], json!([parent.0]));
    assert!(w.events.all().iter().any(|event| matches!(event,
        FiberEvent::Record { id, data, .. } if id == &child.0 && data["id"] == "forged"
    )));

    // A script outside onHandle has no fiber whose record it could emit.
    let script = w.plugins.path().join("outside.ts");
    std::fs::write(&script, "await fiber.record({ type: 'outside' });\n").unwrap();
    let outcome = w.kernel.run_script(&script, vec![]).await.unwrap();
    assert!(outcome.result.unwrap_err().message.contains("requires a running fiber"));
}

#[tokio::test(flavor = "multi_thread")]
async fn records_are_the_events_as_json_lines_and_scripts_can_await_plugins() {
    let w = world().await;
    let record = w.work.path().join("records/fibers.jsonl");
    w.kernel.add_hook(JsonlRecorder::open(&record).unwrap());
    w.kernel.call("create", json!({ "path": "a.txt", "content": "a" })).await.unwrap();
    let lines: Vec<FiberEvent> =
        std::fs::read_to_string(&record).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2);
    assert!(matches!(&lines[0], FiberEvent::Spawn { fiber, .. } if fiber.plugin == "create"));
    assert!(matches!(&lines[1], FiberEvent::Exit { exit: Exit::Done { .. }, .. }));

    let script = w.plugins.path().join("script.ts");
    std::fs::write(
        &script,
        "import plugins from \"ply:plugins\";\nconst r = await plugins.bash({ command: `wc -l < ${process.argv[2]}` });\nconst model = await ctx.currentModel();\nsetTimeout(() => console.log('lines', r.stdout.trim(), model.provider), 20);\n",
    )
    .unwrap();
    let outcome = w.kernel.run_script(&script, vec!["notes.txt".into()]).await.unwrap();
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    assert_eq!(outcome.output.stdout, "lines 4 deepseek\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_propagates_to_children_without_stopping_siblings() {
    let w = world_with(vec![]).await;
    w.kernel.load(w.plugin("parent", "import plugins from \"ply:plugins\";\nexport async function onHandle(input) { return await plugins.worker(input); }")).await.unwrap();
    w.kernel.load(w.plugin("worker", r#"
import fs from "node:fs";
export async function onHandle(input) {
  await new Promise(resolve => setTimeout(resolve, 180));
  fs.writeFileSync(input.path, "completed");
  return input.path;
}
"#)).await.unwrap();
    w.kernel.ready().await.unwrap();
    let root = w.kernel.wake("parent", json!({ "path": "cancelled.txt" })).unwrap();
    let child = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some((id, _, _, _)) = w.events.spawns().into_iter().find(|(_, parent, plugin, _)| parent.as_deref() == Some(&root) && plugin == "worker") { break id; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert!(w.kernel.cancel_fiber(&root));
    let sibling = w.kernel.call("worker", json!({ "path": "sibling.txt" })).await.unwrap();
    assert_eq!(sibling, json!("sibling.txt"));
    tokio::time::timeout(Duration::from_secs(2), async {
        while w.events.exit_of(&child).is_none() { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert!(matches!(w.events.exit_of(&root), Some(Exit::Cancelled { .. })));
    assert!(matches!(w.events.exit_of(&child), Some(Exit::Cancelled { .. })));
    assert!(!w.work.path().join("cancelled.txt").exists());
    assert!(w.work.path().join("sibling.txt").exists());
    for id in [&root, &child] {
        assert_eq!(w.events.all().iter().filter(|e| matches!(e, FiberEvent::Exit { id: actual, .. } if actual == id)).count(), 1);
    }
    assert!(!w.kernel.cancel_fiber(&root));
}

#[tokio::test(flavor = "multi_thread")]
async fn event_hook_panics_are_isolated_and_budget_exits_are_typed() {
    struct BrokenHook;
    impl FiberHook for BrokenHook { fn on_event(&self, _: &FiberEvent) { panic!("intentional broken hook"); } }
    let mut config = KernelConfig::new(Io::new(), "/");
    config.limits.turn_cpu = Duration::from_millis(100);
    let kernel = Kernel::new(config);
    let events = Arc::new(Events::default());
    kernel.add_hook(Arc::new(BrokenHook)); kernel.add_hook(events.clone());
    kernel.load(PluginSource::Embedded { name: "cpu".into(), source: "export async function onHandle(input) { await null; if (input.spin) while(true) {} return 'ok'; }".into() }).await.unwrap();
    kernel.ready().await.unwrap();
    let error = kernel.call("cpu", json!({ "spin": true })).await.unwrap_err();
    assert!(matches!(error, KernelError::BudgetExceeded { resource: ply_kernel::BudgetResource::Cpu, .. }));
    assert_eq!(kernel.call("cpu", json!({})).await.unwrap(), json!("ok"));
    let spawns = events.spawns(); assert_eq!(spawns.len(), 2);
    assert!(matches!(events.exit_of(&spawns[0].0), Some(Exit::BudgetExceeded { resource: ply_kernel::BudgetResource::Cpu, .. })));
    assert!(matches!(events.exit_of(&spawns[1].0), Some(Exit::Done { .. })));
}

#[tokio::test(flavor = "multi_thread")]
async fn cumulative_child_limit_is_atomic_and_rejects_before_spawn() {
    let w = world_with(vec![]).await;
    w.kernel.load(w.plugin("quota_parent", r#"import plugins from "ply:plugins";

export async function onHandle() {
  await fiber.limitChildren("quota_worker", 2);
  const results = await Promise.allSettled([plugins.quota_worker({}), plugins.quota_worker({}), plugins.quota_worker({})]);
  let widened = false;
  try { await fiber.limitChildren("quota_worker", 3); widened = true; } catch {}
  const budget = await fiber.limitChildren("quota_worker", 2, 0);
  return { results: results.map(result => result.status), widened, budget };
}
"#)).await.unwrap();
    w.kernel.load(w.plugin("quota_worker", "export async function onHandle() { await new Promise(resolve => setTimeout(resolve, 10)); return 'ok'; }")).await.unwrap();
    w.kernel.ready().await.unwrap();
    let result = w.kernel.call("quota_parent", json!({})).await.unwrap();
    let results = result["results"].as_array().unwrap();
    assert_eq!(results.iter().filter(|status| **status == "fulfilled").count(), 2);
    assert_eq!(results.iter().filter(|status| **status == "rejected").count(), 1);
    assert_eq!(result["widened"], false);
    assert_eq!(result["budget"], json!({ "limit": 2, "started": 2, "remaining": 0 }));
    assert_eq!(w.events.spawns().iter().filter(|(_, _, plugin, _)| plugin == "quota_worker").count(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn bash_runs_ply_sh_over_virtual_storage_and_resolves_host_programs() {
    let w = world().await;
    w.kernel.io().mount("/virtual", Arc::new(ply_kernel::MemoryStorage::empty()), MountMode::ReadWrite).unwrap();
    let output = w.kernel.call("bash", json!({ "command": "mkdir -p /virtual/sub; printf 'beta\nalpha\n' > /virtual/sub/data; cat /virtual/sub/data | sort; echo $SHELL" })).await.unwrap();
    assert_eq!(output["stdout"], "alpha\nbeta\nply-sh\n");
    assert_eq!(output["exit_code"], 0);
    let denied = w.kernel.call("bash", json!({ "command": "__ply_missing_host_command > external-marker" })).await.unwrap();
    assert_eq!(denied["exit_code"], 127);
    assert!(std::fs::read_to_string(w.work.path().join("external-marker")).unwrap().is_empty());
    let native = w.kernel.call("bash", json!({ "command": "/usr/bin/true" })).await.unwrap();
    assert_eq!(native["exit_code"], 0);
    let memory = w.kernel.call("bash", json!({ "command": "/usr/bin/true", "cwd": "/virtual" })).await.unwrap();
    assert_eq!(memory["exit_code"], 126);
    let timed = w.kernel.call("bash", json!({ "command": "echo entered; while :; do :; done; echo late > late-marker", "timeout_ms": 80 })).await.unwrap();
    assert_eq!(timed["timed_out"], true);
    assert_eq!(timed["stdout"], "entered\n");
    assert_eq!(timed["signal"], Value::Null);
    assert!(!w.work.path().join("late-marker").exists());
    let bounded = w.kernel.call("bash", json!({ "command": "seq 1 10000000", "timeout_ms": 10000 })).await.unwrap();
    assert_eq!(bounded["exit_code"], 1);
    assert!(bounded["stdout"].as_str().unwrap().len() <= 1 << 20);
    assert!(bounded["stderr"].as_str().unwrap().contains("buffer limit"));
    let root = w.kernel.wake("bash", json!({ "command": "echo started > native-entered; while :; do :; done; echo bad > native-late" })).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !w.work.path().join("native-entered").exists() { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert!(w.kernel.cancel_fiber(&root));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(w.events.exit_of(&root), Some(Exit::Cancelled { .. })));
    assert!(!w.work.path().join("native-late").exists());
    let host = w.kernel.wake("bash", json!({ "command": "/bin/sh -c 'echo started > host-entered; /bin/sleep 1; echo bad > host-late'" })).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !w.work.path().join("host-entered").exists() { tokio::time::sleep(Duration::from_millis(5)).await; }
    }).await.unwrap();
    assert!(w.kernel.cancel_fiber(&host));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(matches!(w.events.exit_of(&host), Some(Exit::Cancelled { .. })));
    assert!(!w.work.path().join("host-late").exists());
    assert_eq!(w.kernel.call("bash", json!({ "command": "echo alive" })).await.unwrap()["stdout"], "alive\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn bash_before_hook_is_awaited_can_veto_and_has_its_own_fiber() {
    let w = world_with(PluginSource::defaults()).await;
    w.kernel.load(w.plugin("guard", r#"import plugins from "ply:plugins";

import * as events from "ply:events";
import fs from "node:fs";
export async function onLoad() {
  await plugins.bash.loaded;
  await events.on("bash.beforeExecute", async event => {
    const mine = await fiber.info();
    await new Promise(resolve => setTimeout(resolve, 20));
    await fiber.record({ type: "before_seen", mine: mine.id, parent: mine.parent, source: event.source.fiber, command: event.data.command });
    setTimeout(() => fs.writeFileSync("late-success-hook", "bad"), 100);
    return { allow: !event.data.command.includes("deny"), reason: "guard rule" };
  });
}
"#)).await.unwrap();
    w.kernel.ready().await.unwrap();
    let (a, b) = tokio::join!(
        w.kernel.call("bash", json!({ "command": "echo alpha > alpha" })),
        w.kernel.call("bash", json!({ "command": "echo beta > beta" }))
    );
    a.unwrap(); b.unwrap();
    assert_eq!(std::fs::read_to_string(w.work.path().join("alpha")).unwrap(), "alpha\n");
    assert_eq!(std::fs::read_to_string(w.work.path().join("beta")).unwrap(), "beta\n");
    let error = w.kernel.call("bash", json!({ "command": "echo deny > denied" })).await.unwrap_err();
    assert!(error.to_string().contains("guard rule"), "{error}");
    assert!(!w.work.path().join("denied").exists(), "veto must happen before redirection");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!w.work.path().join("late-success-hook").exists());
    let events = w.events.all();
    let callbacks: Vec<_> = events.iter().filter_map(|event| match event {
        FiberEvent::Spawn { fiber, .. } if fiber.event.as_deref() == Some("bash.beforeExecute") => Some(fiber),
        _ => None,
    }).collect();
    assert_eq!(callbacks.len(), 3);
    for callback in callbacks {
        assert_eq!(callback.plugin, "guard");
        let record = events.iter().find_map(|event| match event {
            FiberEvent::Record { id, data, .. } if id == &callback.id && data["type"] == "before_seen" => Some(data),
            _ => None,
        }).unwrap();
        assert_eq!(record["mine"], callback.id);
        assert_eq!(record["parent"], json!(callback.parent));
        assert_eq!(record["source"], record["parent"]);
        assert_eq!(callback.input["data"]["command"], record["command"]);
        assert!(matches!(w.events.exit_of(&callback.id), Some(Exit::Done { .. })));
    }
    assert_eq!(events.iter().filter(|e| matches!(e, FiberEvent::Plugin { name, .. } if name == "bash.beforeExecute")).count(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn hook_errors_deadlines_and_cancellation_prevent_command_side_effects() {
    let w = world_with(PluginSource::defaults()).await;
    w.kernel.load(w.plugin("policy", r#"import plugins from "ply:plugins";

import fs from "node:fs";
import * as events from "ply:events";
export async function onLoad() {
  await plugins.bash.loaded;
  await events.on("bash.beforeExecute", async event => {
    if (event.data.command.includes("throw")) throw new Error("policy failed");
    if (event.data.command.includes("slow")) {
      await new Promise(resolve => setTimeout(resolve, 500));
      fs.writeFileSync("late-hook", "bad");
    }
  });
}
"#)).await.unwrap();
    w.kernel.ready().await.unwrap();
    let error = w.kernel.call("bash", json!({ "command": "echo throw > throw-marker" })).await.unwrap_err();
    assert!(error.to_string().contains("policy failed"));
    let error = w.kernel.call("bash", json!({ "command": "echo slow > timeout-marker", "timeout_ms": 80 })).await.unwrap_err();
    assert!(error.to_string().contains("deadline"), "{error}");
    let root = w.kernel.wake("bash", json!({ "command": "echo slow > cancel-marker" })).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(w.kernel.cancel_fiber(&root));
    tokio::time::sleep(Duration::from_millis(650)).await;
    for name in ["throw-marker", "timeout-marker", "cancel-marker", "late-hook"] { assert!(!w.work.path().join(name).exists(), "{name}"); }
    assert!(matches!(w.events.exit_of(&root), Some(Exit::Cancelled { .. })));
    assert_eq!(w.kernel.call("bash", json!({ "command": "echo healthy" })).await.unwrap()["stdout"], "healthy\n");
    for event in w.events.all() {
        if let FiberEvent::Spawn { fiber, .. } = event {
            assert!(w.events.exit_of(&fiber.id).is_some(), "missing exit for {}", fiber.id);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn custom_events_are_owned_registered_in_onload_and_can_unsubscribe() {
    let w = world_with(vec![]).await;
    w.kernel.load(w.plugin("signals", r#"
import * as events from "ply:events";
let tick;
export async function onLoad() { tick = await events.define("tick"); return { eventList: () => events.list() }; }
export async function onHandle(input) {
  if (input.late) return await events.define("late");
  return await events.emit(tick, input);
}
"#)).await.unwrap();
    w.kernel.load(w.plugin("watcher", r#"import plugins from "ply:plugins";

import * as events from "ply:events";
export async function onLoad() {
  await plugins.signals.loaded;
  const id = await events.on("signals.tick", async event => { await null; return event.data.value + 1; });
  return { stopWatching: () => events.off(id) };
}
"#)).await.unwrap();
    w.kernel.load(w.plugin("intruder", r#"
import * as events from "ply:events";
export async function onHandle() { return await events.emit("signals.tick", { value: 99 }); }
"#)).await.unwrap();
    w.kernel.ready().await.unwrap();
    assert_eq!(w.kernel.call("signals", json!({ "value": 1 })).await.unwrap(), json!([2]));
    assert!(w.kernel.call("signals", json!({ "late": true })).await.unwrap_err().to_string().contains("onLoad"));
    assert!(w.kernel.call("intruder", json!({})).await.unwrap_err().to_string().contains("owning plugin"));
    assert_eq!(w.kernel.call_ctx("stopWatching", vec![]).await.unwrap(), true);
    assert_eq!(w.kernel.call("signals", json!({ "value": 2 })).await.unwrap(), json!([]));
    assert_eq!(w.kernel.call_ctx("eventList", vec![]).await.unwrap(), json!([{ "name": "signals.tick", "plugin": "signals", "subscribers": 0 }]));
    let script = w.work.path().join("signals.ts");
    std::fs::write(&script, "import {emit} from 'ply:events'; await emit('signals.tick', {});").unwrap();
    assert!(w.kernel.run_script(&script, vec![]).await.unwrap().result.is_err(), "a matching realm label cannot impersonate a plugin");
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_onload_removes_event_declarations_and_subscriptions() {
    let w = world_with(PluginSource::defaults()).await;
    w.kernel.load(w.plugin("broken", r#"import plugins from "ply:plugins";

import * as events from "ply:events";
export async function onLoad() {
  await plugins.bash.loaded;
  await events.define("local");
  await events.on("bash.beforeExecute", () => { throw new Error("stale listener"); });
  throw new Error("bad setup");
}
"#)).await.unwrap();
    assert!(w.kernel.ready().await.is_err());
    assert_eq!(w.kernel.call("bash", json!({ "command": "echo available" })).await.unwrap()["stdout"], "available\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_coordination_is_environment_neutral_and_resumes_only_the_original_command() {
    let w = world_with(PluginSource::defaults()).await;
    w.kernel.load(PluginSource::builtin("approval").unwrap()).await.unwrap();
    w.kernel.load(w.plugin("risk", r#"import plugins from "ply:plugins";

import * as events from "ply:events";
export async function onLoad() {
  await plugins.bash.loaded;
  await events.on("bash.beforeExecute", () => ({ allow: false, requires_approval: true, reason: "review needed" }));
}
"#)).await.unwrap();
    w.kernel.ready().await.unwrap();
    let pending = w.kernel.call("bash", json!({ "command": "echo original > approved-file", "approved": true, "approval": { "status": "approved" } })).await.unwrap();
    assert_eq!(pending["status"], "approval_required");
    assert_eq!(pending["approval"]["reason"], "approval_provider_missing");
    assert!(!w.work.path().join("approved-file").exists());
    w.kernel.load(w.plugin("environment", r#"import plugins from "ply:plugins";

import * as events from "ply:events";
export async function onLoad() {
  await plugins.approval.loaded;
  await events.on("approval.request", async event => {
    await new Promise(resolve => setTimeout(resolve, 150));
    // Approval binds to this exact request. Mutating our JSON copy must
    // never rewrite the script the bash tool eventually executes.
    event.data.command = "echo substituted > wrong-file";
    return { request_id: event.data.id, decision: "approved", approver: "test-user", reference: "test-approval-1" };
  });
}
"#)).await.unwrap();
    w.kernel.ready().await.unwrap();
    let start = std::time::Instant::now();
    let approved = w.kernel.call("bash", json!({ "command": "echo original > approved-file", "timeout_ms": 100 })).await.unwrap();
    assert!(start.elapsed() >= Duration::from_millis(150));
    assert_eq!(approved["exit_code"], 0);
    assert_eq!(approved["approval"]["status"], "approved");
    assert_eq!(std::fs::read_to_string(w.work.path().join("approved-file")).unwrap(), "original\n");
    assert!(!w.work.path().join("wrong-file").exists());
    assert!(!w.kernel.catalog().iter().any(|tool| tool.name == "approval" || tool.name == "environment"));
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_needs_a_matching_explicit_decision_and_honors_rejection_and_cancel() {
    let w = world_with(PluginSource::defaults()).await;
    w.kernel.load(PluginSource::builtin("approval").unwrap()).await.unwrap();
    w.kernel.load(w.plugin("gate", r#"import plugins from "ply:plugins";

import * as events from "ply:events";
export async function onLoad() {
  await plugins.bash.loaded;
  await events.on("bash.beforeExecute", () => ({ allow: false, requires_approval: true }));
}
"#)).await.unwrap();
    w.kernel.load(w.plugin("approver", r#"import plugins from "ply:plugins";

import fs from "node:fs";
import * as events from "ply:events";
export async function onLoad() {
  await plugins.approval.loaded;
  await events.on("approval.request", async event => {
    const command = event.data.command;
    if (command.includes("cancel")) {
      fs.writeFileSync("approval-entered", "yes");
      await new Promise(resolve => setTimeout(resolve, 500));
      fs.writeFileSync("late-approval", "bad");
    }
    if (command.includes("provider_error")) throw new Error("approval service unavailable");
    if (command.includes("mismatch")) return { request_id: "wrong-request", decision: "approved" };
    if (command.includes("reject")) return { request_id: event.data.id, decision: "rejected", reason: "user declined" };
    return { request_id: event.data.id, decision: "pending", reference: "external-ticket" };
  });
}
"#)).await.unwrap();
    w.kernel.ready().await.unwrap();
    for name in ["pending", "mismatch", "provider_error"] {
        let pending = w.kernel.call("bash", json!({ "command": format!("echo {name} > {name}-approval") })).await.unwrap();
        assert_eq!(pending["status"], "approval_required");
        assert!(!w.work.path().join(format!("{name}-approval")).exists());
    }
    let rejected = w.kernel.call("bash", json!({ "command": "echo reject > rejected-approval" })).await.unwrap_err();
    assert!(rejected.to_string().contains("approval rejected"));
    assert!(!w.work.path().join("rejected-approval").exists());
    let root = w.kernel.wake("bash", json!({ "command": "echo cancel > cancelled-approval" })).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !w.work.path().join("approval-entered").exists() { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert!(w.kernel.cancel_fiber(&root));
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(matches!(w.events.exit_of(&root), Some(Exit::Cancelled { .. })));
    assert!(!w.work.path().join("cancelled-approval").exists());
    assert!(!w.work.path().join("late-approval").exists());
}
