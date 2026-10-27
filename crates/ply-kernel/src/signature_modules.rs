//! Resolve relative type imports through the same read-only code namespace,
//! and collect which plugins a plugin's modules import by name.
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use oxc::allocator::Allocator;
use oxc::ast::ast::{ImportDeclarationSpecifier, ImportOrExportKind, ModuleDeclaration, Statement};
use oxc::parser::Parser;
use oxc::span::SourceType;

use crate::{Io, Type, signature};

struct Import {
    spec: String,
    local: String,
    exported: String,
}
struct Module {
    source: String,
    imports: Vec<(String, String, String)>,
}

fn imports(source: &str, path: &Path) -> Result<Vec<Import>, String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(
        &allocator,
        source,
        SourceType::from_path(path).map_err(|error| error.to_string())?,
    )
    .parse();
    if let Some(error) = parsed.diagnostics.first() {
        return Err(format!("{}: {error}", path.display()));
    }
    let mut result = Vec::new();
    for statement in &parsed.program.body {
        let Statement::ImportDeclaration(declaration) = statement else {
            continue;
        };
        let Some(spec) = declaration.source.value.as_str() else {
            continue;
        };
        if !spec.starts_with("./") && !spec.starts_with("../") {
            continue;
        }
        for specifier in declaration.specifiers.iter().flatten() {
            if let ImportDeclarationSpecifier::ImportSpecifier(named) = specifier {
                if declaration.import_kind == ImportOrExportKind::Type
                    || named.import_kind == ImportOrExportKind::Type
                {
                    result.push(Import {
                        spec: spec.into(),
                        local: named.local.name.to_string(),
                        exported: named.imported.name().to_string(),
                    });
                }
            }
        }
    }
    Ok(result)
}

async fn resolve(io: &Io, from: &str, spec: &str, root: &str) -> Result<String, String> {
    let joined = Path::new(from).parent().unwrap().join(spec);
    let mut target = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::ParentDir => {
                target.pop();
            }
            Component::CurDir => {}
            other => target.push(other.as_os_str()),
        }
    }
    if !target.starts_with(root) {
        return Err(format!("import {spec} resolves outside plugin root {root}"));
    }
    let target = target.to_string_lossy();
    let mut candidates = vec![target.to_string()];
    candidates
        .extend(["ts", "tsx", "mts", "js", "mjs"].map(|extension| format!("{target}.{extension}")));
    candidates.extend(["index.ts", "index.js"].map(|entry| format!("{target}/{entry}")));
    for candidate in candidates {
        if io.read(&candidate).await.is_ok() {
            return Ok(candidate);
        }
    }
    Err(format!("cannot resolve import {spec} from {from}"))
}

fn imported(
    path: &str,
    modules: &HashMap<String, Module>,
    visiting: &mut HashSet<String>,
) -> Result<HashMap<String, Type>, String> {
    let module = &modules[path];
    let mut result = HashMap::new();
    if !visiting.insert(path.into()) {
        return Ok(result);
    }
    for (local, exported, dependency) in &module.imports {
        let nested = imported(dependency, modules, visiting)?;
        let types = signature::types(&modules[dependency].source, Path::new(dependency), nested)?;
        result.insert(
            local.clone(),
            types.get(exported).cloned().unwrap_or(Type::Unknown),
        );
    }
    visiting.remove(path);
    Ok(result)
}

pub async fn read(io: &Io, entry: &str, root: &str) -> Result<signature::Signature, String> {
    let mut pending = vec![entry.to_string()];
    let mut modules = HashMap::new();
    while let Some(path) = pending.pop() {
        if modules.contains_key(&path) {
            continue;
        }
        if modules.len() >= 64 {
            return Err("plugin type imports exceed 64 modules".into());
        }
        let source = String::from_utf8(
            io.read(&path)
                .await
                .map_err(|error| format!("{path}: {error}"))?,
        )
        .map_err(|error| format!("{path}: {error}"))?;
        let mut dependencies = Vec::new();
        for import in imports(&source, Path::new(&path))? {
            let dependency = resolve(io, &path, &import.spec, root).await?;
            pending.push(dependency.clone());
            dependencies.push((import.local, import.exported, dependency));
        }
        modules.insert(
            path,
            Module {
                source,
                imports: dependencies,
            },
        );
    }
    let imported = imported(entry, &modules, &mut HashSet::new())?;
    signature::with_imports(&modules[entry].source, Path::new(entry), imported)
}

/// What one module imports: relative modules, and plugins named in
/// `import { a } from "ply:plugins"` (type-only imports excluded).
fn module_links(source: &str, path: &Path) -> Result<(Vec<String>, Vec<String>), String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::from_path(path).map_err(|error| error.to_string())?).parse();
    if let Some(error) = parsed.diagnostics.first() {
        return Err(format!("{}: {error}", path.display()));
    }
    let (mut relative, mut plugins) = (Vec::new(), Vec::new());
    for statement in &parsed.program.body {
        let Some(declaration) = statement.as_module_declaration() else { continue };
        let source = match declaration {
            ModuleDeclaration::ImportDeclaration(import) => {
                if import.source.value == "ply:plugins" && import.import_kind != ImportOrExportKind::Type {
                    for specifier in import.specifiers.iter().flatten() {
                        if let ImportDeclarationSpecifier::ImportSpecifier(named) = specifier
                            && named.import_kind != ImportOrExportKind::Type
                        {
                            plugins.push(named.imported.name().to_string());
                        }
                    }
                }
                Some(&import.source)
            }
            ModuleDeclaration::ExportFromDeclaration(export) => Some(&export.source),
            ModuleDeclaration::ExportAllDeclaration(export) => Some(&export.source),
            _ => None,
        };
        if let Some(spec) = source.and_then(|source| source.value.as_str())
            && (spec.starts_with("./") || spec.starts_with("../"))
        {
            relative.push(spec.to_string());
        }
    }
    Ok((relative, plugins))
}

/// The plugins a plugin imports by name anywhere in its own modules; its
/// `onLoad` runs after theirs.
pub async fn plugin_dependencies(io: &Io, entry: &str, root: &str) -> Result<Vec<String>, String> {
    let mut pending = vec![entry.to_string()];
    let mut seen = HashSet::new();
    let mut plugins = BTreeSet::new();
    while let Some(path) = pending.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        if seen.len() > 64 {
            return Err("plugin imports exceed 64 modules".into());
        }
        let source = String::from_utf8(io.read(&path).await.map_err(|error| format!("{path}: {error}"))?)
            .map_err(|error| format!("{path}: {error}"))?;
        let (relative, named) = module_links(&source, Path::new(&path))?;
        plugins.extend(named);
        for spec in relative {
            pending.push(resolve(io, &path, &spec, root).await?);
        }
    }
    Ok(plugins.into_iter().collect())
}
