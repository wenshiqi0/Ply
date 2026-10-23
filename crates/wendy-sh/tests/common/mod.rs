#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use wendy_io::{AllowAll, Gate, Io, MountMode};
use wendy_sh::Shell;
use wendy_storage::{DiskStorage, MemoryStorage};
use wendy_trace::{MemoryHook, Tracer};

/// Files every test directory starts with.
pub const FIXTURE: &[(&str, &str)] = &[
    ("a.txt", "alpha\nbeta\ngamma\n"),
    ("b.txt", "one\ntwo\nthree\nfour\n"),
    ("fixture.txt", "x 3\ny 1\nz 2\n"),
    ("sub/c.md", "# title\nbody line\n"),
    ("sub/deep/d.log", "TODO: first\nok\nTODO: second\n"),
    (".hidden", "secret\n"),
];

pub fn fixture_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, content) in FIXTURE {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    dir
}

/// A shell whose cwd and HOME is `dir`, mounted read-write at `/work`,
/// with an in-memory `/tmp`.
pub fn shell_in(dir: &Path, gate: Arc<dyn Gate>) -> (Shell, Io, Arc<MemoryHook>) {
    let tracer = Tracer::new();
    let hook = MemoryHook::new();
    tracer.add_hook(hook.clone());
    let io = Io::new(tracer, gate);
    io.mount("/work", Arc::new(DiskStorage::open(dir).unwrap()), MountMode::ReadWrite).unwrap();
    io.mount("/tmp", Arc::new(MemoryStorage::empty()), MountMode::ReadWrite).unwrap();
    let mut sh = Shell::new(&io);
    sh.set_cwd("/work");
    sh.set_var("HOME", "/work", true);
    (sh, io, hook)
}

pub fn shell() -> (tempfile::TempDir, Shell, Io, Arc<MemoryHook>) {
    let dir = fixture_dir();
    let (sh, io, hook) = shell_in(dir.path(), Arc::new(AllowAll));
    (dir, sh, io, hook)
}

/// Every file under `dir` with its contents (to compare side effects).
pub fn snapshot(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let rel = path.strip_prefix(dir).unwrap().display().to_string();
            if path.is_dir() {
                out.insert(format!("{rel}/"), String::new());
                stack.push(path);
            } else {
                out.insert(rel, String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned());
            }
        }
    }
    out
}
