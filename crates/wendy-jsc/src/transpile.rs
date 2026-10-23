//! TypeScript / ES modules → a function JSC can evaluate.
//!
//! JavaScriptCore's public C API has no module loader, so wendy loads
//! modules itself, the way bun puts its transpiler in front of JSC:
//! 1. oxc strips TypeScript (`.ts`, `.mts`, `.cts`, `.tsx`);
//! 2. top-level `import` / `export` and `import()` / `import.meta` are
//!    rewritten to calls into wendy's loader;
//! 3. the result is wrapped as
//!    `(async function (__exports, __require, __import, __meta) { ... })`.
//!
//! Exports are getters installed before the body runs, so circular imports
//! and later reassignments are seen by importers. Imported names are copied
//! at import time (not live bindings), which is what CommonJS interop gives.

use std::path::Path;

use oxc::allocator::Allocator;
use oxc::ast::ast::{
    Declaration, ExportDefaultDeclarationKind, ImportDeclarationSpecifier, ImportExpression,
    ImportMeta, ModuleExportName, Statement,
};
use oxc::ast_visit::{Visit, walk};
use oxc::codegen::Codegen;
use oxc::parser::Parser;
use oxc::semantic::SemanticBuilder;
use oxc::span::{GetSpan, SourceType};
use oxc::transformer::{TransformOptions, Transformer};
use oxc_ecmascript::BoundNames;

/// The wrapper's parameter list; the loader calls the function with these.
pub const WRAPPER_PARAMS: &str = "__exports, __require, __import, __meta";

pub fn transpile(source: &str, path: &Path) -> Result<String, String> {
    let js = strip_types(source, path)?;
    rewrite_modules(&js, path)
}

fn strip_types(source: &str, path: &Path) -> Result<String, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !matches!(ext, "ts" | "mts" | "cts" | "tsx") {
        return Ok(source.to_string());
    }
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).map_err(|e| e.to_string())?;
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if let Some(e) = parsed.diagnostics.first() {
        return Err(format!("{}: {e}", path.display()));
    }
    let mut program = parsed.program;
    let scoping = SemanticBuilder::new().build(&program).semantic.into_scoping();
    // Node's TS semantics: only `type` imports disappear, so modules imported
    // for their side effects still load.
    let mut options = TransformOptions::default();
    options.typescript.only_remove_type_imports = true;
    let result = Transformer::new(&allocator, path, &options).build_with_scoping(scoping, &mut program);
    if let Some(e) = result.diagnostics.first() {
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(Codegen::new().build(&program).code)
}

struct Edit {
    start: u32,
    end: u32,
    text: String,
}

/// Finds `import(...)` and `import.meta` anywhere in the module, and
/// whether the module uses top-level `await`.
struct Dynamic<'e> {
    edits: &'e mut Vec<Edit>,
    function_depth: usize,
    top_level_await: bool,
}

impl<'a> Visit<'a> for Dynamic<'_> {
    fn visit_function(&mut self, it: &oxc::ast::ast::Function<'a>, flags: oxc::syntax::scope::ScopeFlags) {
        self.function_depth += 1;
        walk::walk_function(self, it, flags);
        self.function_depth -= 1;
    }

    fn visit_arrow_function_expression(&mut self, it: &oxc::ast::ast::ArrowFunctionExpression<'a>) {
        self.function_depth += 1;
        walk::walk_arrow_function_expression(self, it);
        self.function_depth -= 1;
    }

    fn visit_await_expression(&mut self, it: &oxc::ast::ast::AwaitExpression<'a>) {
        if self.function_depth == 0 {
            self.top_level_await = true;
        }
        walk::walk_await_expression(self, it);
    }

    fn visit_for_of_statement(&mut self, it: &oxc::ast::ast::ForOfStatement<'a>) {
        if it.r#await && self.function_depth == 0 {
            self.top_level_await = true;
        }
        walk::walk_for_of_statement(self, it);
    }

    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        // `import(` → `__import(`
        self.edits.push(Edit { start: it.span.start, end: it.span.start + 6, text: "__import".into() });
        walk::walk_import_expression(self, it);
    }

    fn visit_import_meta(&mut self, it: &ImportMeta) {
        self.edits.push(Edit { start: it.span.start, end: it.span.end, text: "__meta".into() });
    }
}

fn export_name(name: &ModuleExportName<'_>) -> String {
    name.name().to_string()
}

fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn rewrite_modules(js: &str, path: &Path) -> Result<String, String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, js, SourceType::mjs()).parse();
    if let Some(e) = parsed.diagnostics.first() {
        return Err(format!("{}: {e}", path.display()));
    }
    let program = parsed.program;
    let mut edits: Vec<Edit> = Vec::new();
    // Getter definitions, installed before the module body runs.
    let mut getters: Vec<String> = Vec::new();
    let mut n = 0usize;
    let mut fresh = |prefix: &str| {
        n += 1;
        format!("__{prefix}{n}")
    };
    let text = |start: u32, end: u32| js[start as usize..end as usize].to_string();

    for stmt in &program.body {
        match stmt {
            Statement::ImportDeclaration(decl) => {
                let source = js_string(&decl.source.value.display().to_string());
                let replacement = match &decl.specifiers {
                    None => format!("__require({source});"),
                    Some(specs) if specs.is_empty() => format!("__require({source});"),
                    Some(specs) => {
                        let module = fresh("im");
                        let mut out = format!("const {module} = __require({source});");
                        let mut named = Vec::new();
                        for spec in specs {
                            match spec {
                                ImportDeclarationSpecifier::ImportDefaultSpecifier(d) => {
                                    out.push_str(&format!(" const {} = {module}.default;", d.local.name));
                                }
                                ImportDeclarationSpecifier::ImportNamespaceSpecifier(ns) => {
                                    out.push_str(&format!(" const {} = {module};", ns.local.name));
                                }
                                ImportDeclarationSpecifier::ImportSpecifier(s) => {
                                    named.push(format!("{}: {}", js_string(&export_name(&s.imported)), s.local.name));
                                }
                            }
                        }
                        if !named.is_empty() {
                            out.push_str(&format!(" const {{ {} }} = {module};", named.join(", ")));
                        }
                        out
                    }
                };
                edits.push(Edit { start: decl.span.start, end: decl.span.end, text: replacement });
            }
            Statement::ExportDeclaration(decl) => {
                let declaration: &Declaration<'_> = &decl.declaration;
                declaration.bound_names(&mut |id| {
                    getters.push(format!("__export({}, () => {});", js_string(id.name.as_str()), id.name));
                });
                // Drop the `export ` keyword, keep the declaration.
                edits.push(Edit { start: decl.span.start, end: declaration.span().start, text: String::new() });
            }
            Statement::ExportDefaultDeclaration(decl) => match &decl.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(f) if f.id.is_some() => {
                    let name = f.id.as_ref().map(|i| i.name.to_string()).unwrap_or_default();
                    getters.push(format!("__export(\"default\", () => {name});"));
                    edits.push(Edit { start: decl.span.start, end: f.span.start, text: String::new() });
                }
                ExportDefaultDeclarationKind::ClassDeclaration(c) if c.id.is_some() => {
                    let name = c.id.as_ref().map(|i| i.name.to_string()).unwrap_or_default();
                    getters.push(format!("__export(\"default\", () => {name});"));
                    edits.push(Edit { start: decl.span.start, end: c.span.start, text: String::new() });
                }
                ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => {
                    edits.push(Edit { start: decl.span.start, end: decl.span.end, text: String::new() });
                }
                other => {
                    let span = other.span();
                    let value = fresh("default");
                    getters.push(format!("__export(\"default\", () => {value});"));
                    edits.push(Edit {
                        start: decl.span.start,
                        end: decl.span.end,
                        text: format!("const {value} = ({});", text(span.start, span.end)),
                    });
                }
            },
            Statement::ExportNamedDeclaration(decl) => {
                for spec in &decl.specifiers {
                    getters.push(format!(
                        "__export({}, () => {});",
                        js_string(&export_name(&spec.exported)),
                        export_name(&spec.local)
                    ));
                }
                edits.push(Edit { start: decl.span.start, end: decl.span.end, text: String::new() });
            }
            Statement::ExportFromDeclaration(decl) => {
                let module = fresh("re");
                for spec in &decl.specifiers {
                    getters.push(format!(
                        "__export({}, () => {module}[{}]);",
                        js_string(&export_name(&spec.exported)),
                        js_string(&export_name(&spec.local))
                    ));
                }
                edits.push(Edit {
                    start: decl.span.start,
                    end: decl.span.end,
                    text: format!("const {module} = __require({});", js_string(&decl.source.value.display().to_string())),
                });
            }
            Statement::ExportAllDeclaration(decl) => {
                let source = js_string(&decl.source.value.display().to_string());
                let replacement = match &decl.exported {
                    Some(name) => {
                        let module = fresh("re");
                        getters.push(format!("__export({}, () => {module});", js_string(&export_name(name))));
                        format!("const {module} = __require({source});")
                    }
                    None => format!("__exportStar(__require({source}));"),
                };
                edits.push(Edit { start: decl.span.start, end: decl.span.end, text: replacement });
            }
            _ => {}
        }
    }
    let mut dynamic = Dynamic { edits: &mut edits, function_depth: 0, top_level_await: false };
    dynamic.visit_program(&program);
    let is_async = dynamic.top_level_await;

    edits.sort_by_key(|e| e.start);
    let mut body = String::with_capacity(js.len() + 256);
    let mut cursor = 0usize;
    for edit in &edits {
        let (start, end) = (edit.start as usize, edit.end as usize);
        if start < cursor {
            return Err(format!("{}: overlapping module rewrite at byte {start}", path.display()));
        }
        body.push_str(&js[cursor..start]);
        body.push_str(&edit.text);
        cursor = end;
    }
    body.push_str(&js[cursor..]);

    // Header and getters share line 1 so line numbers in plain JS stay put.
    // Only modules with top-level `await` get an async wrapper, so a module
    // that throws while being imported throws into its importer (as in node).
    Ok(format!(
        "({}function ({WRAPPER_PARAMS}) {{\"use strict\"; \
         const __export = (n, g) => Object.defineProperty(__exports, n, {{ enumerable: true, configurable: true, get: g }}); \
         const __exportStar = (m) => {{ for (const k of Object.keys(m)) if (k !== \"default\" && !Object.prototype.hasOwnProperty.call(__exports, k)) __export(k, () => m[k]); }}; \
         {}{body}\n}})",
        if is_async { "async " } else { "" },
        getters.join(" ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(src: &str, file: &str) -> String {
        transpile(src, Path::new(file)).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn strips_types_and_rewrites_imports() {
        let out = t(
            "import fs, { readFileSync as rf, type Stats } from 'node:fs';\nimport * as path from 'node:path';\nimport './side';\nconst n: number = 1;\ninterface X { a: string }\nexport const x: number = n + 1;\n",
            "m.ts",
        );
        assert!(out.contains("const __im1 = __require(\"node:fs\"); const fs = __im1.default; const { \"readFileSync\": rf } = __im1;"), "{out}");
        assert!(out.contains("const path = __im2;"), "{out}");
        assert!(out.contains("__require(\"./side\");"), "{out}");
        assert!(out.contains("__export(\"x\", () => x);"), "{out}");
        assert!(!out.contains("interface"), "{out}");
        assert!(!out.contains(": number"), "{out}");
        assert!(!out.lines().any(|l| l.trim_start().starts_with("export ")), "{out}");
    }

    #[test]
    fn rewrites_every_export_form() {
        let out = t(
            "const a = 1, b = 2;\nexport { a, b as bee };\nexport default function main() {}\nexport { c } from './c';\nexport * from './d';\nexport * as e from './e';\nexport class K {}\nexport let [p, q] = [1, 2];\n",
            "m.js",
        );
        for needle in [
            "__export(\"a\", () => a);",
            "__export(\"bee\", () => b);",
            "__export(\"default\", () => main);",
            "__export(\"c\", () => __re1[\"c\"]);",
            "__exportStar(__require(\"./d\"));",
            "__export(\"e\", () => __re2);",
            "__export(\"K\", () => K);",
            "__export(\"p\", () => p);",
            "__export(\"q\", () => q);",
            "function main() {}",
        ] {
            assert!(out.contains(needle), "missing {needle} in:\n{out}");
        }
        let expr = t("export default { ok: true };", "m.js");
        assert!(expr.contains("const __default1 = ({ ok: true });"), "{expr}");
    }

    #[test]
    fn rewrites_dynamic_import_and_import_meta() {
        let out = t("const m = await import('./x.js'); console.log(import.meta.url);", "m.js");
        assert!(out.contains("await __import('./x.js')"), "{out}");
        assert!(out.contains("__meta.url"), "{out}");
        assert!(out.starts_with("(async function"), "{out}");
        let sync = t("async function f() { await g(); } const h = async () => await 1;", "m.js");
        assert!(sync.starts_with("(function"), "{sync}");
    }

    #[test]
    fn reports_syntax_errors_with_the_file() {
        let err = transpile("const = ;", Path::new("bad.ts")).unwrap_err();
        assert!(err.starts_with("bad.ts:"), "{err}");
    }
}
