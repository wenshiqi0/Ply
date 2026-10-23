//! The kernel end to end: built-in tools as IPO operations over wendy-io,
//! plugins calling and waking each other, approvals as operations.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use wendy_io::{AllowAll, Answer, Approver, Gate, Io, IoRequest, MountMode, Policy, RuleAction};
use wendy_kernel::{Kernel, KernelConfig, KernelError, PluginSource};
use wendy_storage::DiskStorage;
use wendy_trace::{MemoryHook, Scope, TraceEvent, Tracer, Verdict};

struct World {
    kernel: Kernel,
    hook: Arc<MemoryHook>,
    work: tempfile::TempDir,
    root: String,
}

async fn world_with(gate: impl FnOnce(&Tracer) -> Arc<dyn Gate>, max_depth: usize) -> World {
    let tracer = Tracer::new();
    let hook = MemoryHook::new();
    tracer.add_hook(hook.clone());
    let work = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(work.path()).unwrap().display().to_string();
    std::fs::write(work.path().join("notes.txt"), "alpha\nbeta\ngamma\nbeta\n").unwrap();
    let io = Io::new(tracer.clone(), gate(&tracer));
    // The workspace is mounted at its own host path, so bash and the file
    // tools see the same paths.
    io.mount(&root, Arc::new(DiskStorage::open(work.path()).unwrap()), MountMode::ReadWrite).unwrap();
    let kernel = Kernel::new(KernelConfig { max_depth, ..KernelConfig::new(io, root.clone()) });
    kernel.load(PluginSource::builtin()).await.unwrap();
    // Let the fixture's file events settle before any bash watches the tree.
    tokio::time::sleep(Duration::from_millis(200)).await;
    World { kernel, hook, work, root }
}

async fn world() -> World {
    world_with(|_| Arc::new(AllowAll), 16).await
}

impl World {
    fn events(&self, kind: &str) -> Vec<TraceEvent> {
        self.hook.events().into_iter().filter(|e| e.kind == kind).collect()
    }

    /// The op id of the n-th start of `tool`.
    fn op_of(&self, tool: &str, n: usize) -> String {
        let starts: Vec<_> = self.events("ipo.start").into_iter().filter(|e| e.data["tool"] == tool).collect();
        starts[n].data["op"].as_str().unwrap().to_string()
    }

    fn plugin(&self, dir: &tempfile::TempDir, name: &str, source: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, source).unwrap();
        path
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn builtin_tools_work_on_the_mounted_workspace() {
    let w = world().await;
    let names: Vec<String> = w.kernel.tools().into_iter().map(|t| t.name).collect();
    assert_eq!(names, ["bash", "create", "delete", "edit", "read"]);
    assert_eq!(w.kernel.tools()[0].input_schema["required"], json!(["command"]));

    let read = w.kernel.call("read", json!({ "path": "notes.txt", "offset": 2, "limit": 2 })).await.unwrap();
    assert_eq!(read["content"], "     2\tbeta\n     3\tgamma");
    assert_eq!(read["total_lines"], 4);
    assert_eq!(read["truncated"], true);

    let created = w.kernel.call("create", json!({ "path": "src/lib/new.txt", "content": "fresh\n" })).await.unwrap();
    assert_eq!(created, json!({ "path": format!("{}/src/lib/new.txt", w.root), "bytes": 6, "overwritten": false }));
    let err = w.kernel.call("create", json!({ "path": "notes.txt", "content": "x" })).await.unwrap_err();
    assert!(matches!(&err, KernelError::Tool(m) if m.contains("already exists")), "{err}");

    let err = w.kernel.call("edit", json!({ "path": "notes.txt", "old_string": "beta", "new_string": "BETA" })).await.unwrap_err();
    assert_eq!(err.to_string(), "Error: old_string occurs 2 times in notes.txt; add context or set replace_all");
    let edited = w
        .kernel
        .call("edit", json!({ "path": "notes.txt", "old_string": "beta", "new_string": "$& B", "replace_all": true }))
        .await
        .unwrap();
    assert_eq!(edited["replacements"], 2);
    assert_eq!(std::fs::read_to_string(w.work.path().join("notes.txt")).unwrap(), "alpha\n$& B\ngamma\n$& B\n");
    let err = w.kernel.call("edit", json!({ "path": "notes.txt", "old_string": "nope", "new_string": "x" })).await.unwrap_err();
    assert!(err.to_string().contains("was not found"), "{err}");

    let err = w.kernel.call("delete", json!({ "path": "src" })).await.unwrap_err();
    assert!(err.to_string().contains("set recursive"), "{err}");
    w.kernel.call("delete", json!({ "path": "src", "recursive": true })).await.unwrap();
    assert!(!w.work.path().join("src").exists());

    let bash = w.kernel.call("bash", json!({ "command": "echo made > made.txt; cat made.txt; exit 2" })).await.unwrap();
    assert_eq!(bash["exit_code"], 2);
    assert_eq!(bash["stdout"], "made\n");
    assert_eq!(bash["changes"], json!([{ "op": "write", "path": format!("{}/made.txt", w.root), "dir": false }]));
    assert_eq!(bash["changes_complete"], true);

    let err = w.kernel.call("read", json!({})).await.unwrap_err();
    assert_eq!(err.to_string(), "Error: 'path' is required");
    assert!(matches!(w.kernel.call("nope", json!({})).await, Err(KernelError::UnknownTool(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn every_call_is_an_ipo_operation_and_its_io_carries_the_op() {
    let w = world().await;
    w.kernel.call("create", json!({ "path": "a.txt", "content": "a" })).await.unwrap();
    w.kernel.call("bash", json!({ "command": "echo b > b.txt" })).await.unwrap();
    let _ = w.kernel.call("read", json!({ "path": "missing.txt" })).await.unwrap_err();

    let create = w.op_of("create", 0);
    let phases: Vec<Value> = w.events("ipo.phase").into_iter().filter(|e| e.data["op"] == create).map(|e| e.data["phase"].clone()).collect();
    assert_eq!(phases, [json!("input"), json!("process"), json!("output")]);
    let end = w.events("ipo.end").into_iter().find(|e| e.data["op"] == create).unwrap();
    assert_eq!(end.data["ok"], true);
    assert_eq!(end.data["output"]["bytes"], 1);

    // The write the tool made, through wendy-io, names the operation.
    let write = w.events("io.write").into_iter().find(|e| e.data["path"] == format!("{}/a.txt", w.root)).unwrap();
    assert_eq!(write.data["actor"]["session"], "builtin");
    assert_eq!(write.data["actor"]["label"], "fs.promises.writeFile");
    assert_eq!(write.data["actor"]["op"], create.as_str());

    // So do the process bash started and what it changed.
    let bash = w.op_of("bash", 0);
    let exec = w.events("io.exec").pop().unwrap();
    assert_eq!(exec.data["actor"]["op"], bash.as_str());
    let observed = w.events("io.write").into_iter().find(|e| e.data["observed"] == true).unwrap();
    assert_eq!(observed.data["path"], format!("{}/b.txt", w.root));
    assert_eq!(observed.data["actor"]["runtime"], "exec");
    assert_eq!(observed.data["actor"]["op"], bash.as_str());

    let failed = w.events("ipo.end").into_iter().find(|e| e.data["op"] == w.op_of("read", 0)).unwrap();
    assert_eq!(failed.verdict, Verdict::Error);
    assert!(failed.data["error"].as_str().unwrap().starts_with("Error: ENOENT"), "{}", failed.data);
    assert!(w.hook.events().iter().filter(|e| e.scope == Scope::Ipo).all(|e| e.data["op"].is_string() || e.kind == "plugin.load"));
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_call_and_wake_other_tools() {
    let w = world().await;
    let dir = tempfile::tempdir().unwrap();
    w.plugin(
        &dir,
        "main.ts",
        r#"
import { definePlugin } from "wendy";
export default definePlugin({
  name: "notes",
  tools: {
    count_lines: {
      description: "Count lines of a file using other tools",
      schema: { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
      async process({ path }: { path: string }, ctx: any) {
        const read = await ctx.call("read", { path });
        const wc = await ctx.call("bash", { command: `wc -l < ${path}` });
        const tools = (await ctx.tools()).map((t: any) => t.name);
        return { read: read.total_lines, wc: Number(wc.stdout.trim()), tools };
      },
    },
    kick: {
      async process(_: unknown, ctx: any) {
        return { woke: await ctx.wake("create", { path: "woken.txt", content: "later" }) };
      },
    },
    forever: {
      process: (_: unknown, ctx: any) => ctx.call("forever", {}),
    },
  },
});
"#,
    );
    let spec = w.kernel.load(PluginSource::Path(dir.path().to_path_buf())).await.unwrap();
    assert_eq!(spec.name, "notes");

    let r = w.kernel.call("count_lines", json!({ "path": "notes.txt" })).await.unwrap();
    assert_eq!(r["read"], 4);
    assert_eq!(r["wc"], 4);
    assert_eq!(r["tools"], json!(["bash", "count_lines", "create", "delete", "edit", "forever", "kick", "read"]));
    let outer = w.op_of("count_lines", 0);
    for child in ["read", "bash"] {
        let start = w.events("ipo.start").into_iter().find(|e| e.data["tool"] == child).unwrap();
        assert_eq!(start.data["parent"], outer.as_str(), "{child}");
        assert_eq!(start.data["depth"], 1);
    }

    let r = w.kernel.call("kick", json!({})).await.unwrap();
    let woken = r["woke"].as_str().unwrap().to_string();
    for _ in 0..100 {
        if w.events("ipo.end").iter().any(|e| e.data["op"] == woken.as_str()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(std::fs::read_to_string(w.work.path().join("woken.txt")).unwrap(), "later");
    let start = w.events("ipo.start").into_iter().find(|e| e.data["op"] == woken.as_str()).unwrap();
    assert_eq!(start.data["detached"], true);
    assert_eq!(start.data["parent"], w.op_of("kick", 0).as_str());

    let err = w.kernel.call("forever", json!({})).await.unwrap_err();
    assert!(err.to_string().contains("nested deeper than 16"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_that_collide_or_are_not_plugins_do_not_load() {
    let w = world().await;
    let dir = tempfile::tempdir().unwrap();
    let clash = w.plugin(
        &dir,
        "clash.ts",
        "import { definePlugin } from 'wendy';\nexport default definePlugin({ name: 'mine', tools: { edit: { process: () => 1 } } });\n",
    );
    let err = w.kernel.load(PluginSource::Path(clash)).await.unwrap_err();
    assert_eq!(err.to_string(), "mine: tool `edit` is already provided by builtin");
    let plain = w.plugin(&dir, "plain.ts", "export default { name: 'x' };\n");
    let err = w.kernel.load(PluginSource::Path(plain)).await.unwrap_err();
    assert!(err.to_string().contains("not a plugin"), "{err}");
    let err = w.kernel.load(PluginSource::Path(dir.path().join("missing"))).await.unwrap_err();
    assert!(matches!(err, KernelError::Load(_)));
    assert_eq!(w.kernel.plugins().len(), 1);
}

struct Recording(std::sync::Mutex<Vec<String>>);

#[async_trait::async_trait]
impl Approver for Recording {
    async fn approve(&self, req: &IoRequest) -> Answer {
        self.0.lock().unwrap().push(format!("{} {}", req.op.as_str(), req.path));
        if req.path.ends_with("refused.txt") { Answer::Deny("not this one".into()) } else { Answer::Once }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn approvals_are_operations_under_the_op_that_asked() {
    let asked = Arc::new(Recording(Default::default()));
    let approver = asked.clone();
    let w = world_with(
        move |tracer| {
            let approver = Kernel::approver(tracer, approver);
            Arc::new(Policy::new().default_write(RuleAction::Ask).approver(approver))
        },
        16,
    )
    .await;
    w.kernel.call("create", json!({ "path": "ok.txt", "content": "fine" })).await.unwrap();
    let err = w.kernel.call("create", json!({ "path": "refused.txt", "content": "no" })).await.unwrap_err();
    assert!(err.to_string().contains("EACCES"), "{err}");
    assert!(!w.work.path().join("refused.txt").exists());

    let create = w.op_of("create", 0);
    let approval = w.events("ipo.start").into_iter().find(|e| e.data["kind"] == "approval").unwrap();
    assert_eq!(approval.data["parent"], create.as_str());
    assert_eq!(approval.data["input"]["op"], "write");
    let answers: Vec<Value> = w.events("ipo.end").into_iter().filter(|e| e.data["kind"] == "approval").map(|e| e.data["output"].clone()).collect();
    assert_eq!(answers.last().unwrap(), &json!({ "deny": "not this one" }));
    assert_eq!(asked.0.lock().unwrap().len(), 2);
}
