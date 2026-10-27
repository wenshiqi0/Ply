use std::env;
use std::fs;
use std::path::Path;

/// The runtime's own Node modules (RFC 0015) are the TypeScript files under
/// src/node; their names are their paths there without `.ts`.
fn modules(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap().to_str().unwrap();
        if path.is_dir() {
            modules(root, &path, out);
        } else if name.ends_with(".ts") && !name.ends_with(".d.ts") {
            let relative = path.strip_prefix(root).unwrap().to_str().unwrap().trim_end_matches(".ts").replace('\\', "/");
            out.push((relative, path.display().to_string()));
        }
    }
}

fn main() {
    let root = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("src/node");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut found = Vec::new();
    modules(&root, &root, &mut found);
    let mut generated = String::from("const NODE_MODULES: &[(&str, &str)] = &[\n");
    for (name, path) in found {
        generated.push_str(&format!("    ({name:?}, include_str!({path:?})),\n"));
    }
    generated.push_str("];\n");
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("node_modules.rs"), generated).unwrap();
}
