use std::io::ErrorKind;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use wendy_io::{
    Access, Actor, AllowAll, Answer, Approver, Io, IoRequest, MountMode, Policy, Rule, RuleAction,
    Runtime,
};
use wendy_storage::{DiskStorage, MemoryStorage, SqliteStorage};
use wendy_trace::{MemoryHook, Tracer, Verdict};

fn layer(gate: Arc<dyn wendy_io::Gate>) -> (Io, Arc<MemoryHook>) {
    let tracer = Tracer::new();
    let hook = MemoryHook::new();
    tracer.add_hook(hook.clone());
    (Io::new(tracer, gate), hook)
}

fn plugin(io: &Io, label: &str) -> Io {
    io.as_actor(Actor::new(Runtime::Js, "plugin-1", label))
}

#[tokio::test]
async fn only_mounted_storage_exists() {
    let (io, _) = layer(Arc::new(AllowAll));
    io.mount("/workspace", Arc::new(MemoryStorage::empty()), MountMode::ReadWrite).unwrap();
    io.mount("/data/db", Arc::new(MemoryStorage::empty()), MountMode::ReadOnly).unwrap();

    let root: Vec<String> = io.list("/").await.unwrap().into_iter().map(|i| i.name).collect();
    assert_eq!(root, ["data", "workspace"]);
    let data: Vec<String> = io.list("/data").await.unwrap().into_iter().map(|i| i.name).collect();
    assert_eq!(data, ["db"]);

    assert_eq!(io.read("/etc/passwd").await.unwrap_err().kind(), ErrorKind::NotFound);
    // Synthetic directories cannot be written into.
    assert_eq!(io.write("/data/x", b"x").await.unwrap_err().kind(), ErrorKind::PermissionDenied);
    assert_eq!(io.mkdir("/tmp", false).await.unwrap_err().kind(), ErrorKind::PermissionDenied);
}

#[tokio::test]
async fn read_only_mounts_refuse_writes_and_the_refusal_is_traced() {
    let host = tempfile::tempdir().unwrap();
    std::fs::write(host.path().join("a.txt"), "from host").unwrap();
    let (io, hook) = layer(Arc::new(AllowAll));
    io.mount("/src", Arc::new(DiskStorage::open(host.path()).unwrap()), MountMode::ReadOnly).unwrap();

    let reader = plugin(&io, "fs.readFileSync");
    assert_eq!(reader.read("/src/a.txt").await.unwrap(), b"from host");
    let err = plugin(&io, "fs.writeFileSync").write("/src/a.txt", b"hacked").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ReadOnlyFilesystem);
    assert_eq!(std::fs::read_to_string(host.path().join("a.txt")).unwrap(), "from host");

    let io_events: Vec<_> = hook.events().into_iter().filter(|e| e.kind.starts_with("io.")).collect();
    assert_eq!(io_events.len(), 2);
    assert_eq!(io_events[0].verdict, Verdict::Allow);
    assert_eq!(io_events[0].data["actor"]["label"], "fs.readFileSync");
    assert_eq!(io_events[1].verdict, Verdict::Deny);
    assert_eq!(io_events[1].data["reason"], "read-only mount");
    assert_eq!(io_events[1].data["actor"]["label"], "fs.writeFileSync");
}

struct CountingApprover {
    asked: AtomicUsize,
    answer: Answer,
}

#[async_trait::async_trait]
impl Approver for CountingApprover {
    async fn approve(&self, _req: &IoRequest) -> Answer {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.answer.clone()
    }
}

#[tokio::test]
async fn writes_are_asked_and_always_is_remembered() {
    let approver = Arc::new(CountingApprover { asked: AtomicUsize::new(0), answer: Answer::Always });
    let (io, _) = layer(Arc::new(Policy::new().approver(approver.clone())));
    io.mount("/", Arc::new(MemoryStorage::empty()), MountMode::ReadWrite).unwrap();

    io.write("/a", b"1").await.unwrap();
    io.write("/a", b"2").await.unwrap();
    io.read("/a").await.unwrap(); // reads default to allow, never asked
    assert_eq!(approver.asked.load(Ordering::SeqCst), 1);
    io.write("/b", b"x").await.unwrap(); // a different path is asked again
    assert_eq!(approver.asked.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn denials_from_rules_and_approvers_reach_the_caller() {
    let approver = Arc::new(CountingApprover { asked: AtomicUsize::new(0), answer: Answer::Deny("user said no".into()) });
    let policy = Policy::new()
        .rule(Rule::new(Some(Access::Write), "/scratch", RuleAction::Allow))
        .rule(Rule::new(None, "/secrets", RuleAction::Deny))
        .approver(approver.clone());
    let (io, _) = layer(Arc::new(policy));
    io.mount("/", Arc::new(MemoryStorage::empty()), MountMode::ReadWrite).unwrap();
    io.mkdir("/scratch", false).await.unwrap(); // the rule covers /scratch itself
    assert_eq!(approver.asked.load(Ordering::SeqCst), 0);

    io.write("/scratch/ok", b"x").await.unwrap();
    let err = io.read("/secrets/key").await.unwrap_err();
    assert!(err.to_string().contains("policy denies"), "{err}");
    let err = io.write("/elsewhere", b"x").await.unwrap_err();
    assert!(err.to_string().contains("user said no"), "{err}");
}

#[tokio::test]
async fn a_mount_can_switch_from_disk_to_a_database_without_the_caller_changing() {
    let host = tempfile::tempdir().unwrap();
    let db = host.path().join("store.db");
    std::fs::create_dir(host.path().join("dir")).unwrap();
    let (io, hook) = layer(Arc::new(AllowAll));

    io.mount("/data", Arc::new(DiskStorage::open(host.path().join("dir")).unwrap()), MountMode::ReadWrite).unwrap();
    io.write("/data/note", b"on disk").await.unwrap();
    assert!(host.path().join("dir/note").exists());

    io.swap("/data", Arc::new(SqliteStorage::open(&db).unwrap()), MountMode::ReadWrite).unwrap();
    assert_eq!(io.read("/data/note").await.unwrap_err().kind(), ErrorKind::NotFound);
    io.write("/data/note", b"in sqlite").await.unwrap();
    assert_eq!(io.read("/data/note").await.unwrap(), b"in sqlite");
    assert_eq!(std::fs::read_to_string(host.path().join("dir/note")).unwrap(), "on disk");

    assert_eq!(io.mounts()[0].storage.kind, "sqlite");
    assert!(hook.events().iter().any(|e| e.kind == "mount.swap"));
}

#[tokio::test]
async fn renames_stay_within_one_mount() {
    let (io, _) = layer(Arc::new(AllowAll));
    io.mount("/a", Arc::new(MemoryStorage::empty()), MountMode::ReadWrite).unwrap();
    io.mount("/b", Arc::new(MemoryStorage::empty()), MountMode::ReadWrite).unwrap();
    io.write("/a/f", b"x").await.unwrap();
    io.rename("/a/f", "/a/g").await.unwrap();
    assert_eq!(io.rename("/a/g", "/b/g").await.unwrap_err().kind(), ErrorKind::CrossesDevices);
    assert_eq!(io.remove("/a", true).await.unwrap_err().kind(), ErrorKind::PermissionDenied);
}
