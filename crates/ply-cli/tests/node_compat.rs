//! Node compatibility cases (RFC 0014): every tests/node/<category>/<case>.mjs
//! runs with `ply run` and must print what the pinned Node printed when its
//! `.expected` was recorded, or its `.ply.expected` where Ply deliberately
//! differs. scripts/node-compat.mjs records and re-checks the Node side.

use std::path::{Path, PathBuf};
use std::process::Command;

fn suite() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/node")
}

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() { copy(&entry.path(), &target) } else { std::fs::copy(entry.path(), target).unwrap(); }
    }
}

/// The case's stdout from a fresh copy of its category, with that copy's
/// path as `<dir>`, normalised like the Node side.
fn run(category: &Path, name: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    copy(category, dir.path());
    let output = Command::new(env!("CARGO_BIN_EXE_ply"))
        .current_dir(dir.path())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .args(["run", "--no-default-plugins", name])
        .output()
        .unwrap();
    let real = std::fs::canonicalize(dir.path()).unwrap();
    let mut out = String::from_utf8_lossy(&output.stdout)
        .replace(&real.display().to_string(), "<dir>")
        .replace(&dir.path().display().to_string(), "<dir>");
    match output.status.code() {
        Some(0) => {}
        Some(code) => out.push_str(&format!("[exit {code}]\n")),
        None => out.push_str("[killed]\n"),
    }
    if !output.status.success() && out.trim().is_empty() {
        out.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    out
}

#[test]
fn ply_matches_the_pinned_node_on_every_case() {
    let mut categories: Vec<PathBuf> = std::fs::read_dir(suite()).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_dir()).collect();
    categories.sort();
    let (mut count, mut failures) = (0, Vec::new());
    for category in &categories {
        let mut cases: Vec<String> = std::fs::read_dir(category).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".mjs"))
            .collect();
        cases.sort();
        for name in cases {
            count += 1;
            let stem = name.trim_end_matches(".mjs");
            let ply = category.join(format!("{stem}.ply.expected"));
            let expected = std::fs::read_to_string(if ply.exists() { ply } else { category.join(format!("{stem}.expected")) })
                .unwrap_or_else(|_| panic!("{}/{name}: no expectation; run npm run compat:record", category.display()));
            let actual = run(category, &name);
            if actual != expected {
                let label = format!("{}/{name}", category.file_name().unwrap().to_string_lossy());
                failures.push(format!("{label}\n--- expected\n{expected}--- ply\n{actual}"));
            }
        }
    }
    assert!(count > 0, "no cases under {}", suite().display());
    assert!(failures.is_empty(), "{} of {count} cases differ from Node:\n\n{}", failures.len(), failures.join("\n"));
}
