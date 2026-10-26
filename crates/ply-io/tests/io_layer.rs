//! The IO layer: only mounts exist, mounts can change under callers, and
//! real processes run on disk mounts.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ply_io::{DiskStorage, ExecSpec, Io, MemoryStorage, MountMode, SqliteStorage};

#[tokio::test]
async fn only_mounted_storage_exists() {
    let io = Io::new();
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
async fn read_only_mounts_refuse_writes() {
    let host = tempfile::tempdir().unwrap();
    std::fs::write(host.path().join("a.txt"), "from host").unwrap();
    let io = Io::new();
    io.mount("/src", Arc::new(DiskStorage::open(host.path()).unwrap()), MountMode::ReadOnly).unwrap();

    assert_eq!(io.read("/src/a.txt").await.unwrap(), b"from host");
    let err = io.write("/src/a.txt", b"changed").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ReadOnlyFilesystem);
    assert_eq!(io.remove("/src/a.txt", false).await.unwrap_err().kind(), ErrorKind::ReadOnlyFilesystem);
    assert_eq!(std::fs::read_to_string(host.path().join("a.txt")).unwrap(), "from host");
}

#[tokio::test]
async fn a_mount_can_switch_from_disk_to_a_database_without_the_caller_changing() {
    let host = tempfile::tempdir().unwrap();
    let db = host.path().join("store.db");
    std::fs::create_dir(host.path().join("dir")).unwrap();
    let io = Io::new();

    io.mount("/data", Arc::new(DiskStorage::open(host.path().join("dir")).unwrap()), MountMode::ReadWrite).unwrap();
    io.write("/data/note", b"on disk").await.unwrap();
    assert!(host.path().join("dir/note").exists());

    io.swap("/data", Arc::new(SqliteStorage::open(&db).unwrap()), MountMode::ReadWrite).unwrap();
    assert_eq!(io.read("/data/note").await.unwrap_err().kind(), ErrorKind::NotFound);
    io.write("/data/note", b"in sqlite").await.unwrap();
    assert_eq!(io.read("/data/note").await.unwrap(), b"in sqlite");
    assert_eq!(std::fs::read_to_string(host.path().join("dir/note")).unwrap(), "on disk");
    assert_eq!(io.mounts()[0].storage.kind, "sqlite");
}

#[tokio::test]
async fn renames_stay_within_one_mount() {
    let io = Io::new();
    io.mount("/a", Arc::new(MemoryStorage::empty()), MountMode::ReadWrite).unwrap();
    io.mount("/b", Arc::new(MemoryStorage::empty()), MountMode::ReadWrite).unwrap();
    io.write("/a/f", b"x").await.unwrap();
    io.rename("/a/f", "/a/g").await.unwrap();
    assert_eq!(io.rename("/a/g", "/b/g").await.unwrap_err().kind(), ErrorKind::CrossesDevices);
    assert_eq!(io.remove("/a", true).await.unwrap_err().kind(), ErrorKind::PermissionDenied);
}

fn bash(cwd: &str, script: &str) -> ExecSpec {
    ExecSpec { argv: vec!["bash".into(), "-c".into(), script.into()], cwd: cwd.into(), ..ExecSpec::default() }
}

#[tokio::test(flavor = "multi_thread")]
async fn processes_run_in_disk_mounts_and_end_with_the_call() {
    let work = tempfile::tempdir().unwrap();
    let io = Io::new();
    io.mount("/work", Arc::new(DiskStorage::open(work.path()).unwrap()), MountMode::ReadWrite).unwrap();
    io.mount("/mem", Arc::new(MemoryStorage::empty()), MountMode::ReadWrite).unwrap();

    let r = io.exec(bash("/work", "echo hi > made.txt; cat made.txt; echo oops >&2; exit 3")).await.unwrap();
    assert_eq!((r.code, r.stdout.as_slice(), r.stderr.as_slice()), (Some(3), &b"hi\n"[..], &b"oops\n"[..]));
    assert!(work.path().join("made.txt").exists());

    // stdin and extra environment reach the process.
    let spec = ExecSpec {
        stdin: Some(b"from stdin".to_vec()),
        env: BTreeMap::from([("GREETING".into(), "hello".into())]),
        ..bash("/work", "read line; echo \"$GREETING $line\"")
    };
    assert_eq!(io.exec(spec).await.unwrap().stdout, b"hello from stdin\n");

    // Storage that is not a host directory does not exist for a process.
    assert_eq!(io.exec(bash("/mem", "true")).await.unwrap_err().kind(), ErrorKind::Unsupported);
    assert_eq!(io.exec(bash("/nowhere", "true")).await.unwrap_err().kind(), ErrorKind::PermissionDenied);

    // Timeouts kill the whole group; leftovers die with the main process.
    let started = Instant::now();
    let r = io.exec(ExecSpec { timeout: Some(Duration::from_millis(300)), ..bash("/work", "sleep 30 & sleep 30") }).await.unwrap();
    assert!(r.timed_out && r.signal == Some(9), "{r:?}");
    let r = io.exec(bash("/work", "(sleep 30; echo late > late.txt) & echo main-done")).await.unwrap();
    assert_eq!(r.stdout, b"main-done\n");
    assert!(started.elapsed() < Duration::from_secs(5));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!work.path().join("late.txt").exists());
}
