use std::env;
use std::fs;
use std::path::Path;

fn modules(root: &Path, directory: &Path, files: &mut Vec<(String, String)>) {
    let mut entries: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            modules(root, &path, files);
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("ts" | "tsx" | "mts" | "js" | "mjs")
        ) {
            files.push((
                path.strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/"),
                path.to_str().unwrap().to_string(),
            ));
        }
    }
}

/// Development mode reads the plugins' npm dependencies from the
/// repository's node_modules at run time (RFC 0013): refuse to build
/// without them.
fn check_dependencies(workspace: &Path) {
    println!("cargo:rerun-if-changed={}", workspace.join("package.json").display());
    println!("cargo:rerun-if-changed={}", workspace.join("node_modules/.package-lock.json").display());
    let manifest: serde_json::Value = serde_json::from_str(&fs::read_to_string(workspace.join("package.json")).unwrap()).unwrap();
    let missing: Vec<&String> = manifest["dependencies"]
        .as_object()
        .into_iter()
        .flat_map(|dependencies| dependencies.keys())
        .filter(|name| !workspace.join("node_modules").join(name).join("package.json").is_file())
        .collect();
    if !missing.is_empty() {
        panic!("npm dependencies are not installed: {missing:?}; run `npm ci --ignore-scripts` in {}", workspace.display());
    }
}

fn main() {
    let workspace = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    check_dependencies(&workspace);
    let root = workspace.join("plugins");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut plugins: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    plugins.sort();
    let mut generated = String::from("const BUILTIN_PLUGINS: &[(&str, &[(&str, &str)])] = &[\n");
    for directory in plugins {
        if !directory.join("main.ts").is_file() {
            continue;
        }
        let mut files = Vec::new();
        modules(&directory, &directory, &mut files);
        generated.push_str(&format!(
            "({:?}, &[\n",
            directory.file_name().unwrap().to_str().unwrap()
        ));
        for (name, path) in files {
            generated.push_str(&format!("({name:?}, include_str!({path:?})),\n"));
        }
        generated.push_str("]),\n");
    }
    generated.push_str("];\n");
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("plugins.rs"),
        generated,
    )
    .unwrap();
}
