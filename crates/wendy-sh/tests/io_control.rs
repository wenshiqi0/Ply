//! The sandbox requirements, end to end through the shell:
//! only mounts are visible, every IO is traced and attributed, the gate can
//! deny or ask, read-only mounts hold, mounts swap live, traces persist.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use wendy_io::{
    Access, AllowAll, Answer, Approver, IoRequest, MountMode, Policy, Rule, RuleAction,
};
use wendy_sh::Shell;
use wendy_storage::{DiskStorage, SqliteStorage};
use wendy_trace::{JsonlHook, Scope, TraceEvent, Verdict};

fn io_events(events: &[TraceEvent]) -> Vec<&TraceEvent> {
    events.iter().filter(|e| e.scope == Scope::Io).collect()
}

#[tokio::test]
async fn only_mounted_storage_is_visible() {
    let (_dir, mut sh, _io, _hook) = common::shell();
    let out = sh.exec("ls /; cat /etc/passwd; echo $?; ls /Users 2>&1").await;
    assert_eq!(
        out.stdout_str(),
        "tmp\nwork\n1\nls: cannot access '/Users': No such file or directory\n"
    );
}

#[tokio::test]
async fn every_io_is_traced_and_attributed_to_its_command() {
    let (_dir, mut sh, _io, hook) = common::shell();
    sh.exec("cp a.txt copy.txt; grep -c alpha copy.txt > count.txt; echo *.txt > /dev/null").await;
    let events = hook.events();
    let seen: Vec<(String, String, String)> = io_events(&events)
        .iter()
        .map(|e| {
            (
                e.data["actor"]["label"].as_str().unwrap_or_default().to_string(),
                e.data["op"].as_str().unwrap_or_default().to_string(),
                e.data["path"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let has = |label: &str, op: &str, path: &str| seen.iter().any(|(l, o, p)| l == label && o == op && p == path);
    assert!(has("cp", "read", "/work/a.txt"), "{seen:?}");
    assert!(has("cp", "write", "/work/copy.txt"), "{seen:?}");
    assert!(has("grep", "read", "/work/copy.txt"), "{seen:?}");
    assert!(has(">", "write", "/work/count.txt"), "{seen:?}");
    assert!(has("glob", "list", "/work"), "{seen:?}");
    // And each command run is traced with its argv and status.
    let cmds: Vec<&TraceEvent> = events.iter().filter(|e| e.kind == "cmd.run").collect();
    assert_eq!(cmds.len(), 3);
    assert_eq!(cmds[0].data["argv"], serde_json::json!(["cp", "a.txt", "copy.txt"]));
    assert_eq!(cmds[0].data["status"], 0);
}

#[tokio::test]
async fn the_gate_denies_and_the_denial_is_visible_everywhere() {
    let dir = common::fixture_dir();
    let policy = Policy::new()
        .default_write(RuleAction::Allow)
        .rule(Rule::new(Some(Access::Write), "/work/sub", RuleAction::Deny));
    let (mut sh, _io, hook) = common::shell_in(dir.path(), Arc::new(policy));
    let out = sh.exec("echo x > sub/new.txt; echo $?; rm sub/c.md; echo $?; cat sub/c.md | head -1").await;
    assert_eq!(out.stdout_str(), "1\n1\n# title\n");
    assert!(out.stderr_str().contains("sub/new.txt: Permission denied"), "{}", out.stderr_str());
    assert!(out.stderr_str().contains("rm: cannot remove 'sub/c.md': Permission denied"), "{}", out.stderr_str());
    assert!(!dir.path().join("sub/new.txt").exists());

    let events = hook.events();
    let denied: Vec<&TraceEvent> = io_events(&events).into_iter().filter(|e| e.verdict == Verdict::Deny).collect();
    assert_eq!(denied.len(), 2);
    assert_eq!(denied[0].data["actor"]["label"], ">");
    assert!(denied[0].data["reason"].as_str().unwrap().contains("policy denies"));
    assert_eq!(denied[1].data["actor"]["label"], "rm");
}

struct Recorder {
    asked: AtomicUsize,
    labels: std::sync::Mutex<Vec<String>>,
    answer: Answer,
}

#[async_trait::async_trait]
impl Approver for Recorder {
    async fn approve(&self, req: &IoRequest) -> Answer {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.labels.lock().unwrap().push(format!("{} {} {}", req.actor.label, req.op.as_str(), req.path));
        self.answer.clone()
    }
}

#[tokio::test]
async fn writes_wait_for_an_approver_mid_script() {
    let dir = common::fixture_dir();
    let recorder = Arc::new(Recorder { asked: AtomicUsize::new(0), labels: Default::default(), answer: Answer::Always });
    let (mut sh, _io, _hook) = common::shell_in(dir.path(), Arc::new(Policy::new().approver(recorder.clone())));
    let out = sh.exec("for i in 1 2 3; do echo $i >> log.txt; done; cat log.txt").await;
    assert_eq!(out.stdout_str(), "1\n2\n3\n");
    // Asked once for log.txt, then remembered ("always").
    assert_eq!(recorder.asked.load(Ordering::SeqCst), 1);
    assert_eq!(recorder.labels.lock().unwrap()[0], ">> append /work/log.txt");

    let refusing = Arc::new(Recorder { asked: AtomicUsize::new(0), labels: Default::default(), answer: Answer::Deny("user declined".into()) });
    let (mut sh, _io, _hook) = common::shell_in(dir.path(), Arc::new(Policy::new().approver(refusing.clone())));
    let out = sh.exec("sed -i 's/alpha/ALPHA/' a.txt; echo $?; head -1 a.txt").await;
    assert_eq!(out.stdout_str(), "4\nalpha\n");
    assert_eq!(refusing.labels.lock().unwrap()[0], "sed write /work/a.txt");
}

#[tokio::test]
async fn read_only_mounts_hold_against_every_command() {
    let dir = common::fixture_dir();
    let (mut sh, io, _hook) = common::shell_in(dir.path(), Arc::new(AllowAll));
    io.mount("/ro", Arc::new(DiskStorage::open(dir.path()).unwrap()), MountMode::ReadOnly).unwrap();
    let out = sh
        .exec("cd /ro; cat a.txt | head -1; echo x > a.txt; rm b.txt; sed -i d a.txt; mkdir n; touch t; cp a.txt z; echo end")
        .await;
    assert_eq!(out.stdout_str(), "alpha\nend\n");
    assert_eq!(out.stderr_str().matches("Read-only file system").count(), 6, "{}", out.stderr_str());
    assert_eq!(common::snapshot(dir.path()), {
        let fresh = common::fixture_dir();
        common::snapshot(fresh.path())
    });
}

#[tokio::test]
async fn a_mount_switches_storage_under_a_live_session() {
    let dir = common::fixture_dir();
    let db_dir = tempfile::tempdir().unwrap();
    let (mut sh, io, _hook) = common::shell_in(dir.path(), Arc::new(AllowAll));
    let first = sh.exec("cat a.txt | head -1; echo from-disk > note").await;
    assert_eq!(first.stdout_str(), "alpha\n");
    assert!(dir.path().join("note").exists());

    io.swap("/work", Arc::new(SqliteStorage::open(db_dir.path().join("w.db")).unwrap()), MountMode::ReadWrite).unwrap();
    let second = sh.exec("ls; echo from-db > note; cat note").await;
    assert_eq!(second.stdout_str(), "from-db\n");
    assert_eq!(std::fs::read_to_string(dir.path().join("note")).unwrap(), "from-disk\n");

    // A brand new shell over the same database sees what the first one wrote.
    let (mut other, io2, _hook2) = common::shell_in(dir.path(), Arc::new(AllowAll));
    io2.swap("/work", Arc::new(SqliteStorage::open(db_dir.path().join("w.db")).unwrap()), MountMode::ReadWrite).unwrap();
    assert_eq!(other.exec("cat note").await.stdout_str(), "from-db\n");
}

#[tokio::test]
async fn traces_persist_to_disk_through_a_hook() {
    let dir = common::fixture_dir();
    let trace_dir = tempfile::tempdir().unwrap();
    let (mut sh, io, _hook) = common::shell_in(dir.path(), Arc::new(AllowAll));
    io.tracer().add_hook(JsonlHook::open(trace_dir.path().join("trace.jsonl")).unwrap());
    sh.exec("echo hi > greeting.txt").await;

    let text = std::fs::read_to_string(trace_dir.path().join("trace.jsonl")).unwrap();
    let kinds: Vec<String> = text
        .lines()
        .map(|l| serde_json::from_str::<TraceEvent>(l).unwrap().kind)
        .collect();
    assert_eq!(kinds, ["exec.start", "io.write", "cmd.run", "io.append", "exec.end"]);
}

#[tokio::test]
async fn sessions_are_isolated_but_share_mounts() {
    let (dir, mut a, io, _hook) = common::shell();
    let mut b = Shell::new(&io);
    b.set_cwd("/work");
    a.exec("x=only-a; cd sub; echo shared > /work/shared.txt").await;
    let out = b.exec("echo [$x]; pwd; cat shared.txt").await;
    assert_eq!(out.stdout_str(), "[]\n/work\nshared\n");
    assert!(dir.path().join("shared.txt").exists());
    assert_ne!(a.session(), b.session());
}
