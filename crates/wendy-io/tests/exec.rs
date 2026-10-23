//! Real processes through the IO layer: confined writes, observed changes,
//! gate, network, lifetime.

#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use wendy_io::{
    Access, Actor, AllowAll, Change, ExecPolicy, ExecSpec, Gate, Io, MountMode, OpKind, Policy, Rule, RuleAction,
    Runtime,
};
use wendy_storage::{DiskStorage, MemoryStorage};
use wendy_trace::{MemoryHook, Scope, Tracer, Verdict};

fn layer(gate: Arc<dyn Gate>) -> (Io, Arc<MemoryHook>) {
    let tracer = Tracer::new();
    let hook = MemoryHook::new();
    tracer.add_hook(hook.clone());
    let io = Io::new(tracer, gate);
    (io.as_actor(Actor { op: Some("op-7".into()), ..Actor::new(Runtime::Js, "tools", "wendy.exec") }), hook)
}

fn bash(cwd: &str, script: &str) -> ExecSpec {
    ExecSpec { argv: vec!["bash".into(), "-c".into(), script.into()], cwd: cwd.into(), ..ExecSpec::default() }
}

fn change(op: OpKind, path: &str, dir: bool) -> Change {
    Change { op, path: path.into(), dir }
}

#[tokio::test(flavor = "multi_thread")]
async fn processes_write_only_to_read_write_disk_mounts_and_every_change_is_traced() {
    let work = tempfile::tempdir().unwrap();
    let reference = tempfile::tempdir().unwrap();
    // Not under the per-user temp dir, which processes may write to.
    let outside = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    std::fs::write(work.path().join("old.txt"), "old\n").unwrap();
    std::fs::write(work.path().join("gone.txt"), "bye\n").unwrap();
    // Let the fixture's own file events settle before watching starts.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let (io, hook) = layer(Arc::new(AllowAll));
    io.mount("/work", Arc::new(DiskStorage::open(work.path()).unwrap()), MountMode::ReadWrite).unwrap();
    io.mount("/ref", Arc::new(DiskStorage::open(reference.path()).unwrap()), MountMode::ReadOnly).unwrap();
    let script = format!(
        "echo hi > new.txt; mkdir -p d/e; echo x > d/e/f; echo more >> old.txt; rm gone.txt
         {{ echo no > '{out}/x'; }} 2>/dev/null || echo denied-outside
         {{ echo no > '{reference}/x'; }} 2>/dev/null || echo denied-read-only
         t=$(mktemp) && echo scratch > \"$t\" && echo temp-ok
         echo to-stderr >&2; exit 3",
        out = outside.path().display(),
        reference = reference.path().display(),
    );
    let r = io.exec(bash("/work", &script)).await.unwrap();

    assert_eq!(r.code, Some(3));
    assert_eq!(String::from_utf8_lossy(&r.stdout), "denied-outside\ndenied-read-only\ntemp-ok\n");
    assert_eq!(String::from_utf8_lossy(&r.stderr), "to-stderr\n");
    assert!(!outside.path().join("x").exists() && !reference.path().join("x").exists());
    assert_eq!(std::fs::read_to_string(work.path().join("old.txt")).unwrap(), "old\nmore\n");
    assert!(r.changes_complete);
    let mut changes = r.changes.clone();
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(
        changes,
        [
            change(OpKind::Mkdir, "/work/d", true),
            change(OpKind::Mkdir, "/work/d/e", true),
            change(OpKind::Write, "/work/d/e/f", false),
            change(OpKind::Remove, "/work/gone.txt", false),
            change(OpKind::Write, "/work/new.txt", false),
            change(OpKind::Write, "/work/old.txt", false),
        ]
    );

    let events = hook.events();
    let spawn = events.iter().find(|e| e.kind == "exec.spawn").unwrap();
    assert_eq!(spawn.data["net"], "deny");
    assert_eq!(spawn.data["writable"], serde_json::json!(["/work"]));
    let observed: Vec<_> = events.iter().filter(|e| e.data["observed"] == true).collect();
    assert_eq!(observed.len(), 6);
    for e in &observed {
        assert_eq!(e.scope, Scope::Io);
        assert_eq!(e.data["actor"]["runtime"], "exec");
        assert_eq!(e.data["actor"]["label"], "bash");
        assert_eq!(e.data["actor"]["session"], "tools");
        assert_eq!(e.data["actor"]["op"], "op-7");
    }
    let done = events.iter().find(|e| e.kind == "io.exec").unwrap();
    assert_eq!(done.verdict, Verdict::Allow);
    assert_eq!(done.data["argv"][0], "bash");
    assert_eq!(done.data["detail"]["code"], 3);
    assert_eq!(done.data["detail"]["changes"], 6);
    // The exec summary comes after what the process did.
    assert!(observed.iter().all(|e| e.seq < done.seq) && spawn.seq < observed[0].seq);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_gate_decides_before_anything_runs() {
    let work = tempfile::tempdir().unwrap();
    std::fs::create_dir(work.path().join("locked")).unwrap();
    let policy = Policy::new().default_write(RuleAction::Allow).rule(Rule::new(
        Some(Access::Write),
        "/work/locked",
        RuleAction::Deny,
    ));
    let (io, hook) = layer(Arc::new(policy));
    io.mount("/work", Arc::new(DiskStorage::open(work.path()).unwrap()), MountMode::ReadWrite).unwrap();
    io.mount("/mem", Arc::new(MemoryStorage::empty()), MountMode::ReadWrite).unwrap();

    let err = io.exec(bash("/work/locked", "touch pwned")).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(!work.path().join("locked/pwned").exists());
    assert!(!hook.events().iter().any(|e| e.kind == "exec.spawn"));
    let denial = hook.events().into_iter().find(|e| e.kind == "io.exec").unwrap();
    assert_eq!(denial.verdict, Verdict::Deny);
    assert_eq!(denial.data["argv"][2], "touch pwned");

    // Storage that is not a host directory does not exist for a real process.
    let err = io.exec(bash("/mem", "true")).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
}

#[tokio::test(flavor = "multi_thread")]
async fn network_is_closed_unless_the_policy_opens_it() {
    let work = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            drop(stream);
        }
    });
    let (io, _) = layer(Arc::new(AllowAll));
    io.mount("/work", Arc::new(DiskStorage::open(work.path()).unwrap()), MountMode::ReadWrite).unwrap();
    let probe = format!("(exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null && echo connected || echo refused");

    let r = io.exec(bash("/work", &probe)).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&r.stdout), "refused\n");
    io.set_exec_policy(ExecPolicy { allow_net: true, ..ExecPolicy::default() });
    let r = io.exec(bash("/work", &probe)).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&r.stdout), "connected\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_outlives_the_call_and_timeouts_kill_the_whole_group() {
    let work = tempfile::tempdir().unwrap();
    let (io, _) = layer(Arc::new(AllowAll));
    io.mount("/work", Arc::new(DiskStorage::open(work.path()).unwrap()), MountMode::ReadWrite).unwrap();

    let started = Instant::now();
    let spec = ExecSpec { timeout: Some(Duration::from_millis(300)), ..bash("/work", "sleep 30 & sleep 30") };
    let r = io.exec(spec).await.unwrap();
    assert!(r.timed_out && r.signal == Some(9), "{r:?}");
    assert!(started.elapsed() < Duration::from_secs(5));

    // A background child holding stdout open does not keep the call alive.
    let started = Instant::now();
    let r = io.exec(bash("/work", "(sleep 30; echo late > late.txt) & echo main-done")).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&r.stdout), "main-done\n");
    assert!(started.elapsed() < Duration::from_secs(5));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!work.path().join("late.txt").exists());

    // stdin and env reach the process; host secrets do not.
    let spec = ExecSpec {
        stdin: Some(b"from stdin".to_vec()),
        env: BTreeMap::from([("GREETING".into(), "hello".into())]),
        ..bash("/work", "read line; echo \"$GREETING $line ${WENDY_TEST_SECRET:-no-secret}\"")
    };
    // SAFETY: tests in this file do not read this variable concurrently.
    unsafe { std::env::set_var("WENDY_TEST_SECRET", "leaked") };
    let r = io.exec(spec).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&r.stdout), "hello from stdin no-secret\n");
}
