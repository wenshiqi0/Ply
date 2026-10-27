//! Node's module resolution over a realm's code namespace (RFC 0013).
//!
//! Relative specifiers stay inside the importer's boundary: the plugin root,
//! or the package the importer belongs to. Bare specifiers walk up from the
//! importer's directory through `node_modules`, use the package's `exports`
//! with Node's conditions, and otherwise its `main` and `index` files.

use std::collections::HashMap;
use std::fmt;

use ply_libc::{Fs, path as lpath};
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// How a resolved file is evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Esm,
    Cjs,
    Json,
    /// `.js` outside a package `type`: decided by its syntax, as Node 22 does.
    Detect,
}

/// The `exports` conditions a lookup matches: `import` for ES module
/// imports, `require` for CommonJS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Import,
    Require,
}

pub(crate) struct Failure {
    pub code: &'static str,
    pub message: String,
}

fn fail(code: &'static str, message: impl Into<String>) -> Failure {
    Failure { code, message: message.into() }
}

/// JSON with object keys in document order: a package's `exports`
/// conditions are tried in the order it lists them.
#[derive(Debug, Clone)]
pub(crate) enum Json {
    Null,
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
    Other,
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Json::String(s)) => Some(s),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Json;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("JSON") }
            fn visit_unit<E>(self) -> Result<Json, E> { Ok(Json::Null) }
            fn visit_bool<E>(self, _: bool) -> Result<Json, E> { Ok(Json::Other) }
            fn visit_i64<E>(self, _: i64) -> Result<Json, E> { Ok(Json::Other) }
            fn visit_u64<E>(self, _: u64) -> Result<Json, E> { Ok(Json::Other) }
            fn visit_f64<E>(self, _: f64) -> Result<Json, E> { Ok(Json::Other) }
            fn visit_str<E>(self, s: &str) -> Result<Json, E> { Ok(Json::String(s.to_string())) }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? { items.push(item); }
                Ok(Json::Array(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? { entries.push(entry); }
                Ok(Json::Object(entries))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// Parsed `package.json` files by directory, `None` where there is none.
pub(crate) type Manifests = HashMap<String, Option<Json>>;

pub(crate) struct Resolver<'a> {
    pub code: &'a Fs,
    pub manifests: &'a mut Manifests,
}

impl Resolver<'_> {
    /// Resolve `spec` imported from `importer` (the realm entry when `None`).
    pub async fn resolve(&mut self, spec: &str, importer: Option<&str>, root: &str, kind: Kind) -> Result<(String, Format), Failure> {
        let base = importer.map(lpath::dirname).unwrap_or_else(|| root.to_string());
        let shown = importer.unwrap_or("<entry>");
        if spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') || spec == "." || spec == ".." {
            let boundary = importer.and_then(package_root).unwrap_or_else(|| root.to_string());
            // Decide containment on the path alone, before touching any
            // storage, so a probe outside learns nothing about what exists.
            let target = lpath::resolve(&base, spec);
            let outside = || fail("ERR_MODULE_OUTSIDE_ROOT", format!("'{spec}' resolves outside {boundary}"));
            if !under(&target, &boundary) { return Err(outside()); }
            let file = self.file_or_directory(&target).await
                .ok_or_else(|| fail("ERR_MODULE_NOT_FOUND", format!("Cannot find module '{spec}' imported from {shown}")))?;
            if !under(&file, &boundary) { return Err(outside()); }
            let format = self.format(&file).await;
            return Ok((file, format));
        }
        let (name, subpath) = split_package(spec)
            .ok_or_else(|| fail("ERR_INVALID_MODULE_SPECIFIER", format!("Invalid module specifier '{spec}' imported from {shown}")))?;
        let mut dir = base;
        loop {
            if lpath::basename(&dir) != "node_modules" {
                let package = lpath::join(&lpath::join(&dir, "node_modules"), name);
                if self.code.is_dir(&package).await {
                    let file = self.package(&package, &subpath, kind, spec, shown).await?;
                    let format = self.format(&file).await;
                    return Ok((file, format));
                }
            }
            if dir == "/" || dir.is_empty() { break; }
            dir = lpath::dirname(&dir);
        }
        Err(fail("ERR_MODULE_NOT_FOUND", format!("Cannot find package '{name}' imported from {shown}")))
    }

    async fn package(&mut self, package: &str, subpath: &str, kind: Kind, spec: &str, shown: &str) -> Result<String, Failure> {
        let manifest = self.manifest(package).await;
        let missing = || fail("ERR_MODULE_NOT_FOUND", format!("Cannot find module '{spec}' imported from {shown}"));
        if let Some(exports) = manifest.as_ref().and_then(|m| m.get("exports")).filter(|e| !matches!(e, Json::Null)) {
            let conditions: &[&str] = match kind { Kind::Import => &["node", "import"], Kind::Require => &["node", "require"] };
            let file = exports_resolve(package, subpath, exports, conditions)?;
            return if self.code.is_file(&file).await { Ok(file) } else { Err(missing()) };
        }
        let file = if subpath == "." {
            match manifest.as_ref().and_then(|m| m.str("main")) {
                Some(main) => {
                    let main = lpath::join(package, main);
                    match self.file(&main).await {
                        Some(file) => Some(file),
                        None => match self.index(&main).await { Some(file) => Some(file), None => self.index(package).await },
                    }
                }
                None => self.index(package).await,
            }
        } else {
            self.file_or_directory(&lpath::join(package, &subpath[2..])).await
        };
        file.filter(|file| under(file, package)).ok_or_else(missing)
    }

    async fn file_or_directory(&mut self, path: &str) -> Option<String> {
        if let Some(file) = self.file(path).await { return Some(file); }
        if !self.code.is_dir(path).await { return None; }
        if let Some(main) = self.manifest(path).await.as_ref().and_then(|m| m.str("main")).map(|main| lpath::join(path, main)) {
            if let Some(file) = self.file(&main).await { return Some(file); }
            if let Some(file) = self.index(&main).await { return Some(file); }
        }
        self.index(path).await
    }

    async fn file(&self, path: &str) -> Option<String> {
        if self.code.is_file(path).await { return Some(path.to_string()); }
        for ext in ["ts", "tsx", "mts", "cts", "js", "mjs", "cjs", "json"] {
            let candidate = format!("{path}.{ext}");
            if self.code.is_file(&candidate).await { return Some(candidate); }
        }
        None
    }

    async fn index(&self, dir: &str) -> Option<String> {
        for name in ["index.ts", "index.tsx", "index.js", "index.mjs", "index.cjs", "index.json"] {
            let candidate = lpath::join(dir, name);
            if self.code.is_file(&candidate).await { return Some(candidate); }
        }
        None
    }

    /// Node's package scope: the nearest `package.json` above the file,
    /// stopping at `node_modules`.
    async fn format(&mut self, file: &str) -> Format {
        match lpath::extname(file).as_str() {
            ".mjs" | ".mts" | ".ts" | ".tsx" => return Format::Esm,
            ".cjs" | ".cts" => return Format::Cjs,
            ".json" => return Format::Json,
            _ => {}
        }
        let mut dir = lpath::dirname(file);
        loop {
            if lpath::basename(&dir) == "node_modules" { return Format::Detect; }
            if let Some(manifest) = self.manifest(&dir).await {
                return match manifest.str("type") {
                    Some("module") => Format::Esm,
                    Some("commonjs") => Format::Cjs,
                    _ => Format::Detect,
                };
            }
            if dir == "/" || dir.is_empty() { return Format::Detect; }
            dir = lpath::dirname(&dir);
        }
    }

    async fn manifest(&mut self, dir: &str) -> Option<Json> {
        if let Some(cached) = self.manifests.get(dir) { return cached.clone(); }
        let file = lpath::join(dir, "package.json");
        let parsed = match self.code.is_file(&file).await {
            true => self.code.read(&file).await.ok().and_then(|bytes| serde_json::from_slice::<Json>(&bytes).ok()),
            false => None,
        };
        self.manifests.insert(dir.to_string(), parsed.clone());
        parsed
    }
}

/// `pkg`, `@scope/pkg`, either with a subpath: the package name and the
/// subpath as `exports` keys it (`.` or `./sub`).
fn split_package(spec: &str) -> Option<(&str, String)> {
    if spec.is_empty() || spec.starts_with('.') || spec.contains('\\') || spec.contains('%') { return None; }
    let end = if spec.starts_with('@') {
        let slash = spec.find('/')?;
        spec[slash + 1..].find('/').map_or(spec.len(), |i| slash + 1 + i)
    } else {
        spec.find('/').unwrap_or(spec.len())
    };
    let name = &spec[..end];
    if name.ends_with('/') || name == "@" { return None; }
    Some((name, format!(".{}", &spec[end..])))
}

/// The package directory a file under `node_modules` belongs to.
pub(crate) fn package_root(path: &str) -> Option<String> {
    let at = path.rfind("/node_modules/")? + "/node_modules/".len();
    let mut parts = path[at..].split('/');
    let first = parts.next()?;
    let len = if first.starts_with('@') { first.len() + 1 + parts.next()?.len() } else { first.len() };
    Some(path[..at + len].to_string())
}

/// `path` equals `root` or lies below it, compared by whole components.
pub(crate) fn under(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches('/');
    root.is_empty() || path == root || path.starts_with(&format!("{root}/"))
}

/// Node's PACKAGE_EXPORTS_RESOLVE, with `default` always matching.
fn exports_resolve(package: &str, subpath: &str, exports: &Json, conditions: &[&str]) -> Result<String, Failure> {
    let not_exported = || fail("ERR_PACKAGE_PATH_NOT_EXPORTED",
        format!("Package subpath '{subpath}' is not defined by \"exports\" in {package}/package.json"));
    let sugar = [(".".to_string(), exports.clone())];
    let map: &[(String, Json)] = match exports {
        Json::Object(entries) if entries.first().is_some_and(|(key, _)| key.starts_with('.')) => entries,
        _ => &sugar,
    };
    if !subpath.contains('*')
        && let Some((_, target)) = map.iter().find(|(key, _)| key == subpath)
    {
        return target_resolve(package, target, None, conditions)?.ok_or_else(not_exported);
    }
    let mut best: Option<(&str, &Json, &str)> = None;
    for (key, target) in map {
        let Some(star) = key.find('*') else { continue };
        let (base, trailer) = (&key[..star], &key[star + 1..]);
        if trailer.contains('*') || !subpath.starts_with(base) || subpath.len() < key.len() || !subpath.ends_with(trailer) { continue; }
        // Longer base wins, then the longer key (PATTERN_KEY_COMPARE).
        if best.is_none_or(|(other, _, _)| {
            let other_base = other.find('*').map_or(other.len(), |i| i + 1);
            star + 1 > other_base || (star + 1 == other_base && key.len() > other.len())
        }) {
            best = Some((key, target, &subpath[star..subpath.len() - trailer.len()]));
        }
    }
    let (_, target, matched) = best.ok_or_else(not_exported)?;
    target_resolve(package, target, Some(matched), conditions)?.ok_or_else(not_exported)
}

fn target_resolve(package: &str, target: &Json, pattern: Option<&str>, conditions: &[&str]) -> Result<Option<String>, Failure> {
    let invalid = |target: &str| fail("ERR_INVALID_PACKAGE_TARGET", format!("Invalid \"exports\" target \"{target}\" in {package}/package.json"));
    match target {
        Json::String(target) => {
            if !target.starts_with("./") || invalid_segments(&target[2..]) { return Err(invalid(target)); }
            let resolved = match pattern {
                Some(matched) if invalid_segments(matched) => return Err(invalid(target)),
                Some(matched) => target.replace('*', matched),
                None => target.clone(),
            };
            Ok(Some(lpath::join(package, &resolved[2..])))
        }
        Json::Array(items) => {
            let mut last = Ok(None);
            for item in items {
                match target_resolve(package, item, pattern, conditions) {
                    Ok(Some(file)) => return Ok(Some(file)),
                    other => last = other,
                }
            }
            last
        }
        Json::Object(entries) => {
            for (key, value) in entries {
                if (key == "default" || conditions.contains(&key.as_str()))
                    && let Some(file) = target_resolve(package, value, pattern, conditions)?
                {
                    return Ok(Some(file));
                }
            }
            Ok(None)
        }
        Json::Null => Ok(None),
        Json::Other => Err(invalid("<non-string>")),
    }
}

/// `""`, `.`, `..` or `node_modules` as a path segment.
fn invalid_segments(path: &str) -> bool {
    path.split(['/', '\\']).any(|segment| matches!(segment, "" | "." | "..") || segment.eq_ignore_ascii_case("node_modules"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(s: &str) -> Json { serde_json::from_str(s).unwrap() }

    #[test]
    fn package_names_and_subpaths() {
        assert_eq!(split_package("pkg"), Some(("pkg", ".".into())));
        assert_eq!(split_package("@s/pkg/a/b.js"), Some(("@s/pkg", "./a/b.js".into())));
        assert_eq!(split_package("@s"), None);
        assert_eq!(package_root("/r/node_modules/@s/p/dist/x.js").as_deref(), Some("/r/node_modules/@s/p"));
        assert_eq!(package_root("/r/node_modules/a/node_modules/b/x.js").as_deref(), Some("/r/node_modules/a/node_modules/b"));
        assert_eq!(package_root("/r/src/x.js"), None);
    }

    #[test]
    fn exports_follow_condition_order_and_patterns() {
        let exports = json(r#"{ ".": { "types": "./t.d.ts", "require": "./c.cjs", "import": "./m.mjs" },
            "./providers/*": { "import": "./dist/providers/*.js" }, "./providers/internal/*": null,
            "./data/*.json": "./data/*.json" }"#);
        let resolve = |sub: &str, c: &[&str]| exports_resolve("/p", sub, &exports, c).map_err(|f| f.code);
        assert_eq!(resolve(".", &["node", "import"]).ok().as_deref(), Some("/p/m.mjs"));
        assert_eq!(resolve(".", &["node", "require"]).ok().as_deref(), Some("/p/c.cjs"));
        assert_eq!(resolve("./providers/deepseek", &["node", "import"]).ok().as_deref(), Some("/p/dist/providers/deepseek.js"));
        assert_eq!(resolve("./providers/internal/x", &["node", "import"]).err(), Some("ERR_PACKAGE_PATH_NOT_EXPORTED"));
        assert_eq!(resolve("./data/a.json", &["node"]).ok().as_deref(), Some("/p/data/a.json"));
        assert_eq!(resolve("./missing", &["node"]).err(), Some("ERR_PACKAGE_PATH_NOT_EXPORTED"));
        assert_eq!(resolve("./providers/../../x", &["node", "import"]).err(), Some("ERR_INVALID_PACKAGE_TARGET"));
        let sugar = json(r#"{ "node": "./n.js", "default": "./d.js" }"#);
        assert_eq!(exports_resolve("/p", ".", &sugar, &["node"]).ok().as_deref(), Some("/p/n.js"));
        assert_eq!(exports_resolve("/p", ".", &json(r#""../x.js""#), &[]).err().map(|f| f.code), Some("ERR_INVALID_PACKAGE_TARGET"));
    }
}
