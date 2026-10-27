//! Cancellation lets the cancelled calls clean up: what they await rejects
//! with an AbortError and their catch/finally run, within a deadline.
use std::sync::{Arc, Mutex};
use std::time::Duration;
use serde_json::json;
use ply_kernel::{CleanupStatus, DiskStorage, Exit, FiberEvent, FiberHook, Io, Kernel, KernelConfig, MountMode, PluginSource};

#[derive(Default)]
struct Events(Mutex<Vec<FiberEvent>>);
impl FiberHook for Events {
    fn on_event(&self, event: &FiberEvent) { self.0.lock().unwrap().push(event.clone()); }
}
impl Events {
    fn all(&self) -> Vec<FiberEvent> { self.0.lock().unwrap().clone() }
    fn spawned(&self, plugin: &str) -> Option<String> {
        self.all().into_iter().find_map(|e| match e { FiberEvent::Spawn { fiber, .. } if fiber.plugin == plugin => Some(fiber.id), _ => None })
    }
    fn exits(&self, id: &str) -> Vec<Exit> {
        self.all().into_iter().filter_map(|e| match e { FiberEvent::Exit { id: i, exit, .. } if i == id => Some(exit), _ => None }).collect()
    }
    fn cleanups(&self, owner: &str) -> Vec<CleanupStatus> {
        self.all().into_iter().filter_map(|e| match e { FiberEvent::Cleanup { owner: Some(o), status, .. } if o == owner => Some(status), _ => None }).collect()
    }
}

struct World { kernel: Kernel, events: Arc<Events>, work: tempfile::TempDir, plugins: tempfile::TempDir }

impl World {
    fn new() -> World {
        let work = tempfile::tempdir().unwrap();
        let root = work.path().display().to_string();
        let io = Io::new();
        io.mount(&root, Arc::new(DiskStorage::open(work.path()).unwrap()), MountMode::ReadWrite).unwrap();
        let kernel = Kernel::new(KernelConfig::new(io, root));
        let events = Arc::new(Events::default());
        kernel.add_hook(events.clone());
        World { kernel, events, work, plugins: tempfile::tempdir().unwrap() }
    }
    fn plugin(&self, name: &str, source: &str) -> PluginSource {
        let dir = self.plugins.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.ts"), source).unwrap();
        PluginSource::Dir(dir)
    }
    async fn until(&self, what: &str, done: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async { while !done() { tokio::time::sleep(Duration::from_millis(5)).await; } })
            .await.unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_parent_runs_finally_in_the_parent_and_its_child() {
    let w = World::new();
    w.kernel.load(w.plugin("parent", r#"import fs from "node:fs";
import plugins from "ply:plugins";
export async function onHandle() {
  try { return await plugins.worker({}); }
  catch (error) { fs.writeFileSync("parent-caught", `${error.name}:${error.code}`); throw error; }
  finally { fs.writeFileSync("parent-finally", "ran"); }
}
"#)).await.unwrap();
    w.kernel.load(w.plugin("worker", r#"import fs from "node:fs";
export async function onHandle() {
  try { await new Promise(resolve => setTimeout(resolve, 30000)); }
  finally { await fs.promises.writeFile("worker-finally", "ran"); }
}
"#)).await.unwrap();
    w.kernel.ready().await.unwrap();
    let parent = w.kernel.wake("parent", json!({})).unwrap();
    w.until("the child", || w.events.spawned("worker").is_some()).await;
    let child = w.events.spawned("worker").unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(w.kernel.cancel_fiber(&parent));
    w.until("both cleanups", || [&parent, &child].iter().all(|id| w.events.cleanups(id).len() == 2)).await;
    for id in [&parent, &child] {
        let exits = w.events.exits(id);
        assert_eq!(exits.len(), 1, "{exits:?}");
        assert!(matches!(exits[0], Exit::Cancelled { .. }), "{exits:?}");
        assert_eq!(w.events.cleanups(id), vec![CleanupStatus::Started, CleanupStatus::Done]);
    }
    assert_eq!(std::fs::read_to_string(w.work.path().join("parent-caught")).unwrap(), "AbortError:ERR_PLY_CANCELLED");
    assert_eq!(std::fs::read_to_string(w.work.path().join("parent-finally")).unwrap(), "ran");
    assert_eq!(std::fs::read_to_string(w.work.path().join("worker-finally")).unwrap(), "ran");
}

#[tokio::test(flavor = "multi_thread")]
async fn cleanup_may_call_methods_and_io_but_not_start_calls() {
    let w = World::new();
    w.kernel.load(w.plugin("store", r#"import fs from "node:fs";
export async function onLoad() {
  return { async release(name) { fs.writeFileSync("released-" + name, "yes"); return true; } };
}
export function onHandle() { return "spawned"; }
"#)).await.unwrap();
    w.kernel.load(w.plugin("holder", r#"import fs from "node:fs";
import { store } from "ply:plugins";
export async function onHandle() {
  try { await new Promise(resolve => setTimeout(resolve, 30000)); }
  finally {
    await store.release("lease");
    try { await store({}); fs.writeFileSync("spawned", "yes"); }
    catch (error) { fs.writeFileSync("spawn-refused", `${error.name}:${error.code}`); }
  }
}
"#)).await.unwrap();
    w.kernel.ready().await.unwrap();
    let holder = w.kernel.wake("holder", json!({})).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(w.kernel.cancel_fiber(&holder));
    w.until("the cleanup", || w.events.cleanups(&holder).len() == 2).await;
    assert_eq!(w.events.cleanups(&holder), vec![CleanupStatus::Started, CleanupStatus::Done]);
    assert!(w.work.path().join("released-lease").exists());
    assert!(!w.work.path().join("spawned").exists());
    assert_eq!(std::fs::read_to_string(w.work.path().join("spawn-refused")).unwrap(), "AbortError:ERR_PLY_CANCELLED");
    assert!(w.events.spawned("store").is_none(), "a cleaning call started a new call");
}
