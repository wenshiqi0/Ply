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

fn main() {
    let root = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../plugins");
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
