//! POSIX path algebra (the `path.posix` semantics node exposes, which are
//! also what the shell uses). Pure string functions; no IO.

/// Collapse `.`, `..` and repeated slashes. Keeps absolute vs relative;
/// `..` at the root stays at the root, as the kernel does.
pub fn normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".into(),
        (false, false) => joined,
    }
}

/// Absolute, normalized form of `path` as seen from directory `cwd`.
pub fn resolve(cwd: &str, path: &str) -> String {
    if path.starts_with('/') { normalize(path) } else { normalize(&format!("{cwd}/{path}")) }
}

pub fn join(base: &str, rest: &str) -> String {
    if base.is_empty() { normalize(rest) } else { normalize(&format!("{base}/{rest}")) }
}

/// Last component, ignoring trailing slashes (`basename(1)` semantics).
pub fn basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.starts_with('/') { "/".into() } else { String::new() };
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_string()
}

/// Everything but the last component (`dirname(1)` semantics).
pub fn dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.starts_with('/') { "/".into() } else { ".".into() };
    }
    match trimmed.rfind('/') {
        None => ".".into(),
        Some(i) => {
            let head = trimmed[..i].trim_end_matches('/');
            if head.is_empty() { "/".into() } else { head.to_string() }
        }
    }
}

/// Extension including the dot, node style: `a.tar.gz` → `.gz`, `.bashrc` → ``.
pub fn extname(path: &str) -> String {
    let base = basename(path);
    match base.rfind('.') {
        Some(0) | None => String::new(),
        Some(i) => base[i..].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_like_posix() {
        assert_eq!(normalize("/a//b/./c/../d/"), "/a/b/d");
        assert_eq!(normalize("/../x"), "/x");
        assert_eq!(normalize("a/../../b"), "../b");
        assert_eq!(normalize("./"), ".");
        assert_eq!(resolve("/home/user", "../etc"), "/home/etc");
        assert_eq!(resolve("/home", "/abs"), "/abs");
    }

    #[test]
    fn splits_like_basename_and_dirname() {
        assert_eq!(basename("/a/b/"), "b");
        assert_eq!(basename("/"), "/");
        assert_eq!(dirname("/a/b/"), "/a");
        assert_eq!(dirname("b"), ".");
        assert_eq!(dirname("/b"), "/");
        assert_eq!(extname("x/a.tar.gz"), ".gz");
        assert_eq!(extname(".bashrc"), "");
    }
}
