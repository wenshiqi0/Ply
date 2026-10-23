//! The kernel end to end: plugins as functions, fibers and their records.

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

async fn world() -> World {
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
    for plugin in PluginSource::defaults() {
        kernel.load(plugin).await.unwrap();
    }
    World { kernel, events, work, plugins: tempfile::tempdir().unwrap(), root }
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
    let names: Vec<String> = w.kernel.plugins().into_iter().map(|p| p.name).collect();
    assert_eq!(names, ["bash", "create", "delete", "edit", "read"]);
    let edit = w.kernel.plugins().into_iter().find(|p| p.name == "edit").unwrap();
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
    assert_eq!(edit.input.to_json_schema()["required"], json!(["path", "old_string", "new_string"]));

    let read = w.kernel.call("read", json!({ "path": "notes.txt", "offset": 2, "limit": 2 })).await.unwrap();
    assert_eq!(read["content"], "     2\tbeta\n     3\tgamma");
    assert_eq!((read["total_lines"].clone(), read["truncated"].clone()), (json!(4), json!(true)));

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

    let err = w.kernel.call("delete", json!({ "path": "src" })).await.unwrap_err();
    assert!(err.to_string().contains("set recursive"), "{err}");
    w.kernel.call("delete", json!({ "path": "src", "recursive": true })).await.unwrap();
    assert!(!w.work.path().join("src").exists());

    let bash = w.kernel.call("bash", json!({ "command": "echo made > made.txt; cat made.txt; echo oops >&2; exit 2" })).await.unwrap();
    assert_eq!(bash, json!({ "exit_code": 2, "signal": null, "timed_out": false, "stdout": "made\n", "stderr": "oops\n" }));
    let slow = w.kernel.call("bash", json!({ "command": "sleep 5", "timeout_ms": 200 })).await.unwrap();
    assert_eq!((slow["timed_out"].clone(), slow["signal"].clone()), (json!(true), json!("SIGKILL")));

    assert_eq!(w.kernel.call("read", json!({})).await.unwrap_err().to_string(), "Error: 'path' must be a non-empty string");
    assert!(matches!(w.kernel.call("nope", json!({})).await, Err(KernelError::UnknownPlugin(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn awaiting_a_plugin_is_a_child_fiber_and_every_fiber_is_recorded() {
    let w = world().await;
    w.kernel
        .load(w.plugin(
            "count_lines",
            r#"
/** Count lines of a file two ways. */
export async function handle(input: { path: string }): Promise<{ read: number; wc: number }> {
  const [read, wc] = await Promise.all([
    tools.read({ path: input.path }),
    tools.bash({ command: `wc -l < ${input.path}` }),
  ]);
  return { read: read.total_lines, wc: Number(wc.stdout.trim()) };
}
"#,
        ))
        .await
        .unwrap();
    let r = w.kernel.call("count_lines", json!({ "path": "notes.txt" })).await.unwrap();
    assert_eq!(r, json!({ "read": 4, "wc": 4 }));

    let spawns = w.events.spawns();
    let (outer, parent, _, input) = spawns.iter().find(|s| s.2 == "count_lines").unwrap().clone();
    assert_eq!((parent, input), (None, json!({ "path": "notes.txt" })));
    let children: Vec<_> = spawns.iter().filter(|s| s.1.as_deref() == Some(outer.as_str())).collect();
    let mut child_plugins: Vec<&str> = children.iter().map(|s| s.2.as_str()).collect();
    child_plugins.sort();
    assert_eq!(child_plugins, ["bash", "read"]);
    for child in &children {
        assert!(matches!(w.events.exit_of(&child.0), Some(Exit::Done { .. })));
    }
    assert_eq!(w.events.exit_of(&outer), Some(Exit::Done { output: r }));

    // A failure is recorded with its error.
    let _ = w.kernel.call("read", json!({ "path": "missing.txt" })).await.unwrap_err();
    let failed = w.events.spawns().into_iter().rev().find(|s| s.2 == "read").unwrap().0;
    let Some(Exit::Failed { error, .. }) = w.events.exit_of(&failed) else { panic!() };
    assert!(error.starts_with("Error: ENOENT"), "{error}");

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
    let w = world().await;
    w.kernel
        .load(w.plugin(
            "counter",
            r#"
let seen = 0;        // shared by every fiber of this plugin
let running = 0;
let overlap = 0;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
export async function handle(input: { wait: number }): Promise<{ seen: number; overlap: number }> {
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
    let err = w.kernel.load(w.plugin("edit", "export const handle = () => 1;\n")).await.unwrap_err();
    assert_eq!(err.to_string(), "a plugin named `edit` is already loaded");
    let err = w.kernel.load(w.plugin("plain", "export const x = 1;\n")).await.unwrap_err();
    assert!(err.to_string().contains("does not export a `handle` function"), "{err}");
    let err = w.kernel.load(w.plugin("bad name", "export const handle = () => 1;\n")).await.unwrap_err();
    assert!(err.to_string().contains("must match"), "{err}");
    let err = w.kernel.load(PluginSource::Dir(w.plugins.path().join("missing"))).await.unwrap_err();
    assert!(matches!(err, KernelError::Load(_)));
    let err = w.kernel.load(w.plugin("broken", "export function handle() { throw new Error('at load') }\nthrow new Error('module init failed');\n")).await.unwrap_err();
    assert!(err.to_string().contains("module init failed"), "{err}");
    assert_eq!(w.kernel.plugins().len(), 5);

    w.kernel.load(w.plugin("forever", "export const handle = (): Promise<unknown> => tools.forever({});\n")).await.unwrap();
    let err = w.kernel.call("forever", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("nested deeper than 16"), "{err}");
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
        "const r = await tools.read({ path: process.argv[2] });\nsetTimeout(() => console.log('lines', r.total_lines), 20);\n",
    )
    .unwrap();
    let outcome = w.kernel.run_script(&script, vec!["notes.txt".into()]).await.unwrap();
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    assert_eq!(outcome.output.stdout, "lines 4\n");
}
