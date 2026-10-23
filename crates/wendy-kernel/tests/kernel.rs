//! The kernel end to end: plugins with onHandle and onLoad, fibers and
//! their records, ctx and loaded.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use wendy_kernel::{
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
        [("agent", true), ("bash", true), ("context", false), ("create", true), ("deepseek", false), ("edit", true), ("read", true), ("remove", true)]
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
    assert_eq!((slow["timed_out"].clone(), slow["signal"].clone()), (json!(true), json!("SIGKILL")));

    assert_eq!(w.kernel.call("remove", json!({})).await.unwrap_err().to_string(), "Error: 'path' must be a non-empty string");
    let err = w.kernel.call("agent", json!({ "prompt": " " })).await.unwrap_err();
    assert_eq!(err.to_string(), "Error: 'prompt' must be a non-empty string");
    assert!(matches!(w.kernel.call("nope", json!({})).await, Err(KernelError::UnknownPlugin(_))));
    // Plugins without onHandle cannot be called.
    let err = w.kernel.call("context", json!({})).await.unwrap_err();
    assert_eq!(err.to_string(), "context has no onHandle; it cannot be called");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_six_tools_and_the_model_are_in_the_context() {
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
        r#"
let seen: string[] = [];
export async function onLoad() {
  await tools.late.loaded;
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
    let lonely = w.plugin("lonely", "export async function onLoad() { await tools.ghost.loaded; }\n");
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
            r#"
/** Count lines of a file two ways. */
export async function onHandle(input: { path: string }): Promise<{ lines: number; betas: number }> {
  const [wc, grep] = await Promise.all([
    tools.bash({ command: `wc -l < ${input.path}` }),
    tools.bash({ command: `grep -c beta ${input.path}` }),
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
    assert_eq!(w.kernel.plugins().len(), 8);

    // Every realm lists the loaded plugins; each callable one says what it is as a tool.
    w.kernel
        .load(w.plugin(
            "lister",
            "/** List tools. */\nexport const onHandle = (name: string) => ({ names: Object.keys(tools), has: name in tools, description: tools[name].description, wraps: tools.lister.wrapsInput, params: tools.lister.parameters, contextParams: tools.context.parameters ?? null });\n",
        ))
        .await
        .unwrap();
    let listed = w.kernel.call("lister", json!("edit")).await.unwrap();
    assert_eq!(listed["names"], json!(["agent", "bash", "context", "create", "deepseek", "edit", "lister", "read", "remove"]));
    assert_eq!(listed["has"], true);
    assert!(listed["description"].as_str().unwrap().starts_with("Replace an exact string"));
    assert_eq!(listed["wraps"], true);
    assert_eq!(listed["params"], json!({ "type": "object", "properties": { "input": { "type": "string" } }, "required": ["input"] }));
    assert_eq!(listed["contextParams"], Value::Null);

    w.kernel.load(w.plugin("forever", "export const onHandle = (): Promise<unknown> => tools.forever({});\n")).await.unwrap();
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
        .load(w.plugin("parent", "export async function onHandle() { return await tools.noter({}); }\n"))
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
        "const r = await tools.bash({ command: `wc -l < ${process.argv[2]}` });\nconst model = await ctx.currentModel();\nsetTimeout(() => console.log('lines', r.stdout.trim(), model.provider), 20);\n",
    )
    .unwrap();
    let outcome = w.kernel.run_script(&script, vec!["notes.txt".into()]).await.unwrap();
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    assert_eq!(outcome.output.stdout, "lines 4 deepseek\n");
}
