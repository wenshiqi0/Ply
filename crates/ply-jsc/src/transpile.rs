//! TypeScript / ES modules → a function JSC can evaluate.
//!
//! JavaScriptCore's public C API has no module loader, so ply loads
//! modules itself, the way bun puts its transpiler in front of JSC:
//! 1. oxc strips TypeScript (`.ts`, `.mts`, `.cts`, `.tsx`);
//! 2. top-level `import` / `export` and `import()` / `import.meta` are
//!    rewritten to calls into ply's loader;
//! 3. the result is wrapped as
//!    `(async function (__exports, __require, __import, __meta) { ... })`.
//!
//! Exports are getters installed before the body runs, and every use of an
//! imported name reads the imported module's export at that moment: the
//! bindings are live, so a module in an import cycle (typebox has many)
//! sees what the other one has initialised by the time it uses the name.

use std::collections::HashMap;
use std::path::Path;

use oxc::allocator::Allocator;
use oxc::ast::ast::{
    Declaration, ExportAllDeclaration, ExportDefaultDeclarationKind, ExportFromDeclaration, ExportNamedDeclaration,
    Expression, IdentifierReference, ImportDeclaration, ImportDeclarationSpecifier, ImportExpression, ImportMeta,
    ModuleExportName, ObjectProperty, Statement,
};
use oxc::ast_visit::{Visit, walk};
use oxc::codegen::Codegen;
use oxc::parser::Parser;
use oxc::semantic::{Scoping, SemanticBuilder, SymbolId};
use oxc::span::{GetSpan, SourceType};
use oxc::transformer::{HelperLoaderMode, TransformOptions, Transformer};
use oxc_ecmascript::BoundNames;

/// The wrapper's parameter list; the loader calls the function with these.
/// `__export` and `__exportStar` come from the loader too: a module may
/// declare its own `Object` (typebox does), so the header names no globals.
pub const WRAPPER_PARAMS: &str = "__exports, __require, __import, __meta, __export, __exportStar";

pub fn transpile(source: &str, path: &Path) -> Result<String, String> {
    let js = strip_types(source, path)?;
    // Wrap top-level await before lowering it, so every continuation uses
    // the same scheduler as async functions, including imported JS files.
    let wrapped = rewrite_modules(&js, path)?;
    lower_async(&wrapped, path)
}

/// The CommonJS wrapper's parameters; `__import` serves `import()`.
pub const CJS_PARAMS: &str = "exports, require, module, __filename, __dirname, __import";

/// A CommonJS file → a function JSC can evaluate. It stays a sloppy-mode
/// script; `import()` is rewritten for ply's loader and async functions are
/// lowered like everywhere else, so fibers see their continuations.
pub fn transpile_cjs(source: &str, path: &Path) -> Result<String, String> {
    // Node strips a hashbang line; keep its line so positions stay put.
    let source = match source.strip_prefix("#!") { Some(rest) => format!("//{rest}"), None => source.to_string() };
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, &source, SourceType::cjs()).parse();
    if let Some(e) = parsed.diagnostics.first() {
        return Err(format!("{}: {e}", path.display()));
    }
    let mut edits = Vec::new();
    Dynamic { edits: &mut edits, function_depth: 0, top_level_await: false }.visit_program(&parsed.program);
    let body = apply(&source, &mut edits, path)?;
    lower(&format!("(function ({CJS_PARAMS}) {{{body}\n}})"), path, SourceType::script())
}

/// Node 22's syntax detection for a `.js` file outside a package `type`:
/// `import` / `export` statements or `import.meta` make it an ES module.
pub fn has_module_syntax(source: &str) -> bool {
    let allocator = Allocator::default();
    Parser::new(&allocator, source, SourceType::unambiguous()).parse().program.source_type.is_module()
}

/// Lower native async/await to explicit Promise continuations. JSC's C API
/// cannot attach host identity to native await continuations. The private
/// helper binding is supplied lexically by the loader, never as a global.
pub(crate) fn lower_async(source: &str, path: &Path) -> Result<String, String> {
    lower(source, path, SourceType::mjs())
}

fn lower(source: &str, path: &Path, source_type: SourceType) -> Result<String, String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if let Some(error) = parsed.diagnostics.first() { return Err(format!("{}: {error}", path.display())); }
    let mut program = parsed.program;
    let scoping = SemanticBuilder::new().build(&program).semantic.into_scoping();
    let mut options = TransformOptions::default();
    options.env.es2017.async_to_generator = true;
    options.env.es2018.async_generator_functions = true;
    options.helper_loader.mode = HelperLoaderMode::External;
    let result = Transformer::new(&allocator, path, &options).build_with_scoping(scoping, &mut program);
    if let Some(error) = result.diagnostics.first() { return Err(format!("{}: {error}", path.display())); }
    Ok(Codegen::new().build(&program).code)
}

fn strip_types(source: &str, path: &Path) -> Result<String, String> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).map_err(|e| e.to_string())?;
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if let Some(e) = parsed.diagnostics.first() {
        return Err(format!("{}: {e}", path.display()));
    }
    let mut program = parsed.program;
    let scoping = SemanticBuilder::new().with_enum_eval(true).build(&program).semantic.into_scoping();
    // Node's TS semantics: only `type` imports disappear, so modules imported
    // for their side effects still load.
    let mut options = TransformOptions::default();
    options.typescript.only_remove_type_imports = true;
    // Imported helper bindings cannot collide with a plugin's own names.
    options.env.es2017.async_to_generator = true;
    options.env.es2018.async_generator_functions = true;
    options.helper_loader.module_name = "ply:async".into();
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

fn apply(js: &str, edits: &mut [Edit], path: &Path) -> Result<String, String> {
    edits.sort_by_key(|e| e.start);
    let mut body = String::with_capacity(js.len() + 256);
    let mut cursor = 0usize;
    for edit in edits.iter() {
        let (start, end) = (edit.start as usize, edit.end as usize);
        if start < cursor {
            return Err(format!("{}: overlapping module rewrite at byte {start}", path.display()));
        }
        body.push_str(&js[cursor..start]);
        body.push_str(&edit.text);
        cursor = end;
    }
    body.push_str(&js[cursor..]);
    Ok(body)
}

/// Turns each use of an imported binding into a read of the imported
/// module's export.
struct Live<'s, 'e> {
    scoping: &'s Scoping,
    imports: &'s HashMap<SymbolId, String>,
    edits: &'e mut Vec<Edit>,
}

impl Live<'_, '_> {
    fn read(&self, id: &IdentifierReference<'_>) -> Option<&String> {
        let symbol = self.scoping.get_reference(id.reference_id()).symbol_id()?;
        self.imports.get(&symbol)
    }
}

impl<'a> Visit<'a> for Live<'_, '_> {
    // The module rewrite replaces these statements whole.
    fn visit_import_declaration(&mut self, _: &ImportDeclaration<'a>) {}
    fn visit_export_named_declaration(&mut self, _: &ExportNamedDeclaration<'a>) {}
    fn visit_export_from_declaration(&mut self, _: &ExportFromDeclaration<'a>) {}
    fn visit_export_all_declaration(&mut self, _: &ExportAllDeclaration<'a>) {}

    fn visit_object_property(&mut self, it: &ObjectProperty<'a>) {
        // `{ name }` → `{ name: __im1["name"] }`
        if it.shorthand && let Expression::Identifier(id) = &it.value && let Some(read) = self.read(id) {
            self.edits.push(Edit { start: it.span.start, end: it.span.end, text: format!("{}: {read}", id.name) });
            return;
        }
        walk::walk_object_property(self, it);
    }

    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        if let Some(read) = self.read(it) {
            self.edits.push(Edit { start: it.span.start, end: it.span.end, text: read.clone() });
        }
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
    let scoping = SemanticBuilder::new().build(&program).semantic.into_scoping();
    let mut edits: Vec<Edit> = Vec::new();
    // Getter definitions, installed before the module body runs.
    let mut getters: Vec<String> = Vec::new();
    let mut n = 0usize;
    let mut fresh = |prefix: &str| {
        n += 1;
        format!("__{prefix}{n}")
    };

    // Imports first (they hoist): each binding becomes a read of the
    // imported module, by symbol for references and by name for exports.
    let mut imports: HashMap<SymbolId, String> = HashMap::new();
    let mut imported: HashMap<String, String> = HashMap::new();
    for stmt in &program.body {
        let Statement::ImportDeclaration(decl) = stmt else { continue };
        let source = js_string(&decl.source.value.display().to_string());
        let replacement = match decl.specifiers.as_ref().filter(|specs| !specs.is_empty()) {
            None => format!("__require({source});"),
            Some(specs) => {
                let module = fresh("im");
                for spec in specs {
                    let read = match spec {
                        ImportDeclarationSpecifier::ImportDefaultSpecifier(_) => format!("{module}.default"),
                        ImportDeclarationSpecifier::ImportNamespaceSpecifier(_) => module.clone(),
                        ImportDeclarationSpecifier::ImportSpecifier(s) => format!("{module}[{}]", js_string(&export_name(&s.imported))),
                    };
                    imports.insert(spec.symbol_id(), read.clone());
                    imported.insert(spec.name().to_string(), read);
                }
                format!("const {module} = __require({source});")
            }
        };
        edits.push(Edit { start: decl.span.start, end: decl.span.end, text: replacement });
    }

    for stmt in &program.body {
        match stmt {
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
                    // Two edits around the expression, so uses of imports
                    // inside it are rewritten too.
                    let span = other.span();
                    let value = fresh("default");
                    getters.push(format!("__export(\"default\", () => {value});"));
                    edits.push(Edit { start: decl.span.start, end: span.start, text: format!("const {value} = (") });
                    edits.push(Edit { start: span.end, end: decl.span.end, text: ");".into() });
                }
            },
            Statement::ExportNamedDeclaration(decl) => {
                for spec in &decl.specifiers {
                    let local = export_name(&spec.local);
                    getters.push(format!(
                        "__export({}, () => {});",
                        js_string(&export_name(&spec.exported)),
                        imported.get(&local).unwrap_or(&local)
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
    Live { scoping: &scoping, imports: &imports, edits: &mut edits }.visit_program(&program);
    let mut dynamic = Dynamic { edits: &mut edits, function_depth: 0, top_level_await: false };
    dynamic.visit_program(&program);
    let is_async = dynamic.top_level_await;
    let body = apply(js, &mut edits, path)?;

    // Header and getters share line 1 so line numbers in plain JS stay put.
    // Only modules with top-level `await` get an async wrapper, so a module
    // that throws while being imported throws into its importer (as in node).
    Ok(format!(
        "({}function ({WRAPPER_PARAMS}) {{\"use strict\"; {}{body}\n}})",
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
            "import fs, { readFileSync as rf, type Stats } from 'node:fs';\nimport * as path from 'node:path';\nimport './side';\nconst n: number = 1;\ninterface X { a: string }\nexport const x: number = n + 1;\nexport const y = () => rf(path.join('a'), fs);\nconst o = { rf };\nexport { rf as read };\n",
            "m.ts",
        );
        // Imported bindings are live: each use reads the module's export.
        for expected in ["const __im1 = __require(\"node:fs\");", "__im1[\"readFileSync\"](__im2.join(\"a\"), __im1.default)",
            "rf: __im1[\"readFileSync\"]", "__export(\"read\", () => __im1[\"readFileSync\"]);"] { assert!(out.contains(expected), "{out}"); }
        assert!(!out.contains("const fs") && !out.contains("const path"), "{out}");
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
        assert!(expr.contains("const __default1 = { ok: true };"), "{expr}");
    }

    #[test]
    fn rewrites_dynamic_import_and_import_meta() {
        let out = t("const m = await import('./x.js'); console.log(import.meta.url);", "m.js");
        assert!(out.contains("yield __import(\"./x.js\")"), "{out}");
        assert!(out.contains("__meta.url"), "{out}");
        assert!(out.contains("babelHelpers.asyncToGenerator"), "{out}");
        let sync = t("async function f() { await g(); } const h = async () => await 1;", "m.js");
        assert!(sync.starts_with("(function"), "{sync}");
    }

    #[test]
    fn reports_syntax_errors_with_the_file() {
        let err = transpile("const = ;", Path::new("bad.ts")).unwrap_err();
        assert!(err.starts_with("bad.ts:"), "{err}");
    }

    #[test]
    fn evaluates_enum_members_before_typescript_lowering() {
        let out = t("enum ContextType { Rules = 'rules', Extensions = 'extensions' } export const type = ContextType.Extensions;", "context.ts");
        assert!(out.contains("rules") && out.contains("extensions"), "{out}");
        assert!(!out.contains("enum ContextType"), "{out}");
        let numeric = t("enum Code { First = 7, Next } export const value = Code.Next;", "numeric.ts");
        assert!(numeric.contains('8'), "{numeric}");
    }
}
