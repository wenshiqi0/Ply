//! The sandbox requirements, end to end through the shell:
//! only mounts are visible, read-only mounts hold, mounts swap live, and
//! independent shell sessions share storage without sharing variables.

mod common;

use std::sync::Arc;
use ply_io::MountMode;
use ply_sh::Shell;
use ply_storage::{DiskStorage, SqliteStorage};

#[tokio::test]
async fn only_mounted_storage_is_visible() {
    let (_dir, mut sh, _io) = common::shell();
    let out = sh.exec("ls /; cat /etc/passwd; echo $?; ls /Users 2>&1").await;
    assert_eq!(
        out.stdout_str(),
        "tmp\nwork\n1\nls: cannot access '/Users': No such file or directory\n"
    );
}



#[tokio::test]
async fn read_only_mounts_hold_against_every_command() {
    let dir = common::fixture_dir();
    let (mut sh, io) = common::shell_in(dir.path());
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
    let (mut sh, io) = common::shell_in(dir.path());
    let first = sh.exec("cat a.txt | head -1; echo from-disk > note").await;
    assert_eq!(first.stdout_str(), "alpha\n");
    assert!(dir.path().join("note").exists());

    io.swap("/work", Arc::new(SqliteStorage::open(db_dir.path().join("w.db")).unwrap()), MountMode::ReadWrite).unwrap();
    let second = sh.exec("ls; echo from-db > note; cat note").await;
    assert_eq!(second.stdout_str(), "from-db\n");
    assert_eq!(std::fs::read_to_string(dir.path().join("note")).unwrap(), "from-disk\n");

    // A brand new shell over the same database sees what the first one wrote.
    let (mut other, io2) = common::shell_in(dir.path());
    io2.swap("/work", Arc::new(SqliteStorage::open(db_dir.path().join("w.db")).unwrap()), MountMode::ReadWrite).unwrap();
    assert_eq!(other.exec("cat note").await.stdout_str(), "from-db\n");
}


#[tokio::test]
async fn sessions_are_isolated_but_share_mounts() {
    let (dir, mut a, io) = common::shell();
    let mut b = Shell::new(&io);
    b.set_cwd("/work");
    a.exec("x=only-a; cd sub; echo shared > /work/shared.txt").await;
    let out = b.exec("echo [$x]; pwd; cat shared.txt").await;
    assert_eq!(out.stdout_str(), "[]\n/work\nshared\n");
    assert!(dir.path().join("shared.txt").exists());
    assert_ne!(a.session(), b.session());
}
