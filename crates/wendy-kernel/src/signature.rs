//! Reading what a plugin is from its source: the JSDoc on `handle` is its
//! description, `handle`'s parameter and return types are its input and
//! output.
//!
//! Only the structural part of TypeScript a signature usually uses is
//! read: primitives, literals, arrays, object types and interfaces declared
//! in the same module, `Record<string, T>`, unions, `Promise<T>` on the
//! return type. Anything else (generics, imported types, conditional or
//! mapped types) reads as [`Type::Unknown`]; the plugin still loads.

use std::collections::HashMap;
use std::path::Path;

use oxc::allocator::Allocator;
use oxc::ast::Comment;
use oxc::ast::ast::{
    Declaration, Expression, FormalParameters, Statement, TSLiteral, TSSignature, TSType, TSTypeAnnotation, TSTypeName,
};
use oxc::parser::Parser;
use oxc::span::{GetSpan, SourceType};
use serde_json::json;
use wendy_core::{Field, Type};

/// What the entry module of a plugin says about itself.
#[derive(Debug)]
pub struct Signature {
    pub description: String,
    pub input: Type,
    pub output: Type,
}

pub fn read(source: &str, path: &Path) -> Result<Signature, String> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if let Some(e) = parsed.diagnostics.first() {
        return Err(format!("{}: {e}", path.display()));
    }
    let program = parsed.program;

    // Types declared in the module, by name, so references can be expanded.
    let mut declared: HashMap<&str, Declared> = HashMap::new();
    let mut handle = None;
    for statement in &program.body {
        let (declaration, start) = match statement {
            Statement::ExportDeclaration(export) => (&export.declaration, export.span.start),
            Statement::TSInterfaceDeclaration(_) | Statement::TSTypeAliasDeclaration(_) => {
                match statement.as_declaration() {
                    Some(d) => (d, statement.span().start),
                    None => continue,
                }
            }
            _ => continue,
        };
        match declaration {
            Declaration::TSInterfaceDeclaration(i) => {
                declared.insert(i.id.name.as_str(), Declared::Members(&i.body.body));
            }
            Declaration::TSTypeAliasDeclaration(t) => {
                declared.insert(t.id.name.as_str(), Declared::Alias(&t.type_annotation));
            }
            Declaration::FunctionDeclaration(f) if f.id.as_ref().is_some_and(|id| id.name == "handle") => {
                handle = Some((start, &*f.params, f.return_type.as_deref()));
            }
            Declaration::VariableDeclaration(v) => {
                for d in &v.declarations {
                    if d.id.get_identifier_name().is_some_and(|n| n == "handle") {
                        match &d.init {
                            Some(Expression::ArrowFunctionExpression(a)) => {
                                handle = Some((start, &*a.params, a.return_type.as_deref()));
                            }
                            Some(Expression::FunctionExpression(f)) => {
                                handle = Some((start, &*f.params, f.return_type.as_deref()));
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let Some((start, params, returns)) = handle else {
        return Err(format!("{}: does not export a `handle` function", path.display()));
    };

    let reader = Reader { source, comments: &program.comments, declared };
    let input = match parameter(params) {
        Some(Some(annotation)) => reader.ty(&annotation.type_annotation, &mut Vec::new()),
        Some(None) => Type::Unknown,
        None => Type::Undefined,
    };
    let output = match returns {
        Some(annotation) => reader.ty(promised(&annotation.type_annotation), &mut Vec::new()),
        None => Type::Unknown,
    };
    Ok(Signature { description: reader.doc(start).unwrap_or_default(), input, output })
}

/// `None`: no parameter; `Some(None)`: one without a type.
fn parameter<'p, 'a>(params: &'p FormalParameters<'a>) -> Option<Option<&'p TSTypeAnnotation<'a>>> {
    params.items.first().map(|p| p.type_annotation.as_deref())
}

/// `Promise<T>` → `T`: a handle's output is what it resolves to.
fn promised<'p, 'a>(ty: &'p TSType<'a>) -> &'p TSType<'a> {
    if let TSType::TSTypeReference(r) = ty
        && matches!(&r.type_name, TSTypeName::IdentifierReference(id) if id.name == "Promise")
        && let Some(args) = &r.type_arguments
        && let Some(inner) = args.params.first()
    {
        return inner;
    }
    ty
}

enum Declared<'p, 'a> {
    Members(&'p oxc::allocator::Vec<'a, TSSignature<'a>>),
    Alias(&'p TSType<'a>),
}

struct Reader<'p, 'a> {
    source: &'p str,
    comments: &'p [Comment],
    declared: HashMap<&'p str, Declared<'p, 'a>>,
}

impl<'p, 'a> Reader<'p, 'a> {
    /// `visiting` holds the declared names being expanded, so recursive
    /// types end in `Unknown` instead of looping.
    fn ty(&self, ty: &TSType<'a>, visiting: &mut Vec<String>) -> Type {
        match ty {
            TSType::TSStringKeyword(_) => Type::String,
            TSType::TSNumberKeyword(_) | TSType::TSBigIntKeyword(_) => Type::Number,
            TSType::TSBooleanKeyword(_) => Type::Boolean,
            TSType::TSNullKeyword(_) => Type::Null,
            TSType::TSUndefinedKeyword(_) | TSType::TSVoidKeyword(_) => Type::Undefined,
            TSType::TSLiteralType(l) => match &l.literal {
                TSLiteral::StringLiteral(s) => Type::Literal { value: json!(s.value.as_str()) },
                TSLiteral::NumericLiteral(n) => Type::Literal { value: json!(n.value) },
                TSLiteral::BooleanLiteral(b) => Type::Literal { value: json!(b.value) },
                _ => Type::Unknown,
            },
            TSType::TSArrayType(a) => Type::Array { items: Box::new(self.ty(&a.element_type, visiting)) },
            TSType::TSParenthesizedType(p) => self.ty(&p.type_annotation, visiting),
            TSType::TSTypeLiteral(l) => self.members(&l.members, visiting),
            TSType::TSUnionType(u) => {
                let mut of = Vec::new();
                for member in &u.types {
                    match self.ty(member, visiting) {
                        Type::Union { of: inner } => of.extend(inner),
                        other => of.push(other),
                    }
                }
                Type::Union { of }
            }
            TSType::TSTypeReference(r) => {
                let TSTypeName::IdentifierReference(id) = &r.type_name else { return Type::Unknown };
                let name = id.name.as_str();
                let args: Vec<&TSType<'a>> = r.type_arguments.as_ref().map(|a| a.params.iter().collect()).unwrap_or_default();
                match (name, args.as_slice()) {
                    ("Array" | "ReadonlyArray", [item]) => Type::Array { items: Box::new(self.ty(item, visiting)) },
                    ("Record", [_, value]) => Type::Map { values: Box::new(self.ty(value, visiting)) },
                    _ if visiting.iter().any(|v| v == name) => Type::Unknown,
                    _ => match self.declared.get(name) {
                        Some(declared) => {
                            visiting.push(name.to_string());
                            let ty = match declared {
                                Declared::Members(members) => self.members(members, visiting),
                                Declared::Alias(alias) => self.ty(alias, visiting),
                            };
                            visiting.pop();
                            ty
                        }
                        None => Type::Unknown,
                    },
                }
            }
            _ => Type::Unknown,
        }
    }

    /// An object type's members; only index signatures make it a map.
    fn members(&self, members: &oxc::allocator::Vec<'a, TSSignature<'a>>, visiting: &mut Vec<String>) -> Type {
        let mut fields = Vec::new();
        let mut index = None;
        for member in members {
            match member {
                TSSignature::TSPropertySignature(p) => {
                    let Some(name) = p.key.static_name() else { continue };
                    let ty = p.type_annotation.as_ref().map_or(Type::Unknown, |a| self.ty(&a.type_annotation, visiting));
                    fields.push(Field { name: name.display().to_string(), optional: p.optional, ty, description: self.doc(p.span.start) });
                }
                TSSignature::TSIndexSignature(i) => index = Some(self.ty(&i.type_annotation.type_annotation, visiting)),
                _ => {}
            }
        }
        match (index, fields.is_empty()) {
            (Some(values), true) => Type::Map { values: Box::new(values) },
            _ => Type::Object { fields },
        }
    }

    /// The text of the JSDoc comment attached to the node starting at
    /// `start`, without its `*` gutters and `@tag` lines.
    fn doc(&self, start: u32) -> Option<String> {
        let comment = self.comments.iter().rev().find(|c| c.is_jsdoc() && c.attached_to == start)?;
        let span = comment.content_span();
        let raw = &self.source[span.start as usize..span.end as usize];
        let mut lines = Vec::new();
        for line in raw.lines() {
            let line = line.trim().trim_start_matches('*').trim();
            if line.starts_with('@') {
                break;
            }
            lines.push(line);
        }
        let text = lines.join("\n").trim().to_string();
        (!text.is_empty()).then_some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_ts(source: &str) -> Signature {
        read(source, Path::new("main.ts")).unwrap()
    }

    fn field(name: &str, optional: bool, ty: Type, description: Option<&str>) -> Field {
        Field { name: name.into(), optional, ty, description: description.map(str::to_string) }
    }

    #[test]
    fn reads_the_handle_signature_and_its_docs() {
        let sig = read_ts(
            r#"
interface Edit {
  /** File to edit. */
  path: string;
  mode?: "a" | "b";
  tags: string[];
}
type Out = { count: number; extra: Record<string, boolean> | null };

/**
 * Replace text in a file.
 *
 * Second paragraph.
 * @param input what to replace
 */
export async function handle(input: Edit): Promise<Out> {
  return { count: 1, extra: null };
}
"#,
        );
        assert_eq!(sig.description, "Replace text in a file.\n\nSecond paragraph.");
        assert_eq!(
            sig.input,
            Type::Object {
                fields: vec![
                    field("path", false, Type::String, Some("File to edit.")),
                    field(
                        "mode",
                        true,
                        Type::Union { of: vec![Type::Literal { value: json!("a") }, Type::Literal { value: json!("b") }] },
                        None
                    ),
                    field("tags", false, Type::Array { items: Box::new(Type::String) }, None),
                ]
            }
        );
        assert_eq!(
            sig.output,
            Type::Object {
                fields: vec![
                    field("count", false, Type::Number, None),
                    field(
                        "extra",
                        false,
                        Type::Union { of: vec![Type::Map { values: Box::new(Type::Boolean) }, Type::Null] },
                        None
                    ),
                ]
            }
        );
    }

    #[test]
    fn reads_arrow_handles_untyped_and_unknown_parts() {
        let sig = read_ts("type Tree = { children: Tree[] };\nexport const handle = (input: Tree, extra: number) => input;\n");
        assert_eq!(sig.description, "");
        let Type::Object { fields } = &sig.input else { panic!("{:?}", sig.input) };
        assert_eq!(fields[0].ty, Type::Array { items: Box::new(Type::Unknown) });
        assert_eq!(sig.output, Type::Unknown);

        let sig = read("export function handle() { return 1; }\n", Path::new("main.js")).unwrap();
        assert_eq!((sig.input, sig.output), (Type::Undefined, Type::Unknown));
        let sig = read_ts("import type { X } from './x';\nexport function handle(input: X): Map<string, X> { return new Map(); }\n");
        assert_eq!((sig.input, sig.output), (Type::Unknown, Type::Unknown));

        assert!(read("export const x = 1;\n", Path::new("main.ts")).unwrap_err().contains("does not export a `handle`"));
        assert!(read("export const = ;\n", Path::new("main.ts")).is_err());
    }
}
