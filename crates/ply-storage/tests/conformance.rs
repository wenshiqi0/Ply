//! One behavior contract, run against every backend: a mount can switch
//! between disk, memory and sqlite without the shell seeing a difference.

use std::io::ErrorKind;
use std::path::Path;

use ply_storage::{DiskStorage, EntryKind, MemoryStorage, SqliteStorage, Storage};

async fn contract(s: &dyn Storage) {
    let p = Path::new;

    // Root exists and starts empty.
    assert_eq!(s.stat(p("/")).await.unwrap().kind, EntryKind::Dir);
    assert!(s.list(p("/")).await.unwrap().is_empty());

    // Files need an existing parent.
    assert_eq!(s.write(p("/a/b.txt"), b"x").await.unwrap_err().kind(), ErrorKind::NotFound);
    s.mkdir(p("/a/deep/er"), true).await.unwrap();
    s.mkdir(p("/a/deep/er"), true).await.unwrap(); // recursive mkdir is idempotent
    assert_eq!(s.mkdir(p("/a"), false).await.unwrap_err().kind(), ErrorKind::AlreadyExists);

    s.write(p("/a/b.txt"), b"hello").await.unwrap();
    s.append(p("/a/b.txt"), b" world").await.unwrap();
    s.append(p("/a/new.log"), b"1").await.unwrap(); // append creates
    assert_eq!(s.read(p("/a/b.txt")).await.unwrap(), b"hello world");
    assert_eq!(s.stat(p("/a/b.txt")).await.unwrap().size, 11);

    let names: Vec<String> = s.list(p("/a")).await.unwrap().into_iter().map(|i| i.name).collect();
    assert_eq!(names, ["b.txt", "deep", "new.log"]);

    // Directories are not files.
    assert_eq!(s.read(p("/a")).await.unwrap_err().kind(), ErrorKind::IsADirectory);

    // Rename moves whole subtrees.
    s.write(p("/a/deep/er/leaf"), b"leaf").await.unwrap();
    s.rename(p("/a/deep"), p("/moved")).await.unwrap();
    assert_eq!(s.read(p("/moved/er/leaf")).await.unwrap(), b"leaf");
    assert_eq!(s.stat(p("/a/deep")).await.unwrap_err().kind(), ErrorKind::NotFound);

    // Non-recursive removal refuses non-empty directories.
    assert_eq!(s.remove(p("/moved"), false).await.unwrap_err().kind(), ErrorKind::DirectoryNotEmpty);
    s.remove(p("/moved"), true).await.unwrap();
    assert_eq!(s.stat(p("/moved/er/leaf")).await.unwrap_err().kind(), ErrorKind::NotFound);

    // Nothing may climb out of the root or delete it.
    assert_eq!(s.read(p("/../etc/passwd")).await.unwrap_err().kind(), ErrorKind::PermissionDenied);
    assert_eq!(s.remove(p("/"), true).await.unwrap_err().kind(), ErrorKind::PermissionDenied);
}

#[tokio::test]
async fn memory_backend_meets_the_contract() {
    contract(&MemoryStorage::empty()).await;
}

#[tokio::test]
async fn sqlite_backend_meets_the_contract() {
    let dir = tempfile::tempdir().unwrap();
    contract(&SqliteStorage::open(dir.path().join("mount.db")).unwrap()).await;
}

#[tokio::test]
async fn sqlite_backend_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("mount.db");
    SqliteStorage::open(&db).unwrap().write(Path::new("/note"), b"kept").await.unwrap();
    let reopened = SqliteStorage::open(&db).unwrap();
    assert_eq!(reopened.read(Path::new("/note")).await.unwrap(), b"kept");
}

#[tokio::test]
async fn disk_backend_meets_the_contract() {
    let dir = tempfile::tempdir().unwrap();
    contract(&DiskStorage::open(dir.path()).unwrap()).await;
}

#[cfg(unix)]
#[tokio::test]
async fn disk_backend_refuses_symlinks_that_leave_the_root() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"host secret").unwrap();
    let root = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("nope"), root.path().join("dangling")).unwrap();

    let s = DiskStorage::open(root.path()).unwrap();
    let err = s.read(Path::new("/escape/secret")).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    let err = s.write(Path::new("/escape/new"), b"x").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    let err = s.write(Path::new("/dangling"), b"x").await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    assert!(!outside.path().join("nope").exists());
    // Escaping links do not even show up in listings.
    assert!(s.list(Path::new("/")).await.unwrap().is_empty());
}
