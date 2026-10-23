//! Shell patterns (`fnmatch`) and pathname expansion.
//!
//! Pattern syntax: `*`, `?`, bracket expressions (`[abc]`, `[a-z]`, `[!x]`,
//! `[^x]`, `[[:alpha:]]`), and `\x` to make any character literal. Callers
//! that build a pattern from partly quoted text escape the quoted characters
//! with [`escape`], which is how a shell keeps `"*"` literal while `*` globs.

use crate::fs::Fs;
use crate::path;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Lit(char),
    Any,
    Star,
    Class { negate: bool, items: Vec<ClassItem> },
}

#[derive(Debug, Clone, PartialEq)]
enum ClassItem {
    Char(char),
    Range(char, char),
    Named(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    toks: Vec<Tok>,
}

/// Escape every pattern metacharacter so `s` matches only itself.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// True if `pattern` has an unescaped `*`, `?` or a complete bracket expression.
pub fn has_meta(pattern: &str) -> bool {
    Pattern::new(pattern).toks.iter().any(|t| !matches!(t, Tok::Lit(_)))
}

/// Remove pattern escapes, giving the literal text.
pub fn unescape(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            } else {
                out.push('\\');
            }
        } else {
            out.push(c);
        }
    }
    out
}

impl Pattern {
    pub fn new(pattern: &str) -> Self {
        let chars: Vec<char> = pattern.chars().collect();
        let mut toks = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '\\' if i + 1 < chars.len() => {
                    toks.push(Tok::Lit(chars[i + 1]));
                    i += 2;
                }
                '*' => {
                    if toks.last() != Some(&Tok::Star) {
                        toks.push(Tok::Star);
                    }
                    i += 1;
                }
                '?' => {
                    toks.push(Tok::Any);
                    i += 1;
                }
                '[' => match parse_class(&chars, i) {
                    Some((tok, next)) => {
                        toks.push(tok);
                        i = next;
                    }
                    None => {
                        toks.push(Tok::Lit('['));
                        i += 1;
                    }
                },
                c => {
                    toks.push(Tok::Lit(c));
                    i += 1;
                }
            }
        }
        Pattern { toks }
    }

    pub fn matches(&self, text: &str) -> bool {
        let text: Vec<char> = text.chars().collect();
        let (mut t, mut p) = (0usize, 0usize);
        let mut backtrack: Option<(usize, usize)> = None;
        while t < text.len() {
            if p < self.toks.len() {
                match &self.toks[p] {
                    Tok::Star => {
                        backtrack = Some((p, t));
                        p += 1;
                        continue;
                    }
                    tok if tok_matches(tok, text[t]) => {
                        t += 1;
                        p += 1;
                        continue;
                    }
                    _ => {}
                }
            }
            match backtrack {
                Some((star_p, star_t)) => {
                    backtrack = Some((star_p, star_t + 1));
                    p = star_p + 1;
                    t = star_t + 1;
                }
                None => return false,
            }
        }
        self.toks[p..].iter().all(|tok| *tok == Tok::Star)
    }

    /// Whether the pattern explicitly starts with a literal dot, which is
    /// required to match hidden names during pathname expansion.
    fn starts_with_dot(&self) -> bool {
        self.toks.first() == Some(&Tok::Lit('.'))
    }
}

fn tok_matches(tok: &Tok, c: char) -> bool {
    match tok {
        Tok::Lit(l) => *l == c,
        Tok::Any => true,
        Tok::Star => true,
        Tok::Class { negate, items } => items.iter().any(|item| class_item_matches(item, c)) != *negate,
    }
}

fn class_item_matches(item: &ClassItem, c: char) -> bool {
    match item {
        ClassItem::Char(x) => *x == c,
        ClassItem::Range(a, b) => *a <= c && c <= *b,
        ClassItem::Named(name) => match name.as_str() {
            "alpha" => c.is_alphabetic(),
            "digit" => c.is_ascii_digit(),
            "alnum" => c.is_alphanumeric(),
            "upper" => c.is_uppercase(),
            "lower" => c.is_lowercase(),
            "space" => c.is_whitespace(),
            "blank" => c == ' ' || c == '\t',
            "punct" => c.is_ascii_punctuation(),
            "xdigit" => c.is_ascii_hexdigit(),
            "cntrl" => c.is_control(),
            "print" => !c.is_control(),
            "graph" => !c.is_control() && c != ' ',
            _ => false,
        },
    }
}

/// Parse a bracket expression starting at `chars[start] == '['`.
fn parse_class(chars: &[char], start: usize) -> Option<(Tok, usize)> {
    let mut i = start + 1;
    let negate = matches!(chars.get(i), Some('!') | Some('^'));
    if negate {
        i += 1;
    }
    let mut items = Vec::new();
    let mut first = true;
    while i < chars.len() {
        let c = chars[i];
        if c == ']' && !first {
            return Some((Tok::Class { negate, items }, i + 1));
        }
        first = false;
        if c == '[' && chars.get(i + 1) == Some(&':') {
            let rest: String = chars[i + 2..].iter().collect();
            if let Some(end) = rest.find(":]") {
                items.push(ClassItem::Named(rest[..end].to_string()));
                i += 2 + end + 2;
                continue;
            }
        }
        let c = if c == '\\' && i + 1 < chars.len() {
            i += 1;
            chars[i]
        } else {
            c
        };
        if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|n| *n != ']') {
            items.push(ClassItem::Range(c, chars[i + 2]));
            i += 3;
        } else {
            items.push(ClassItem::Char(c));
            i += 1;
        }
    }
    None
}

/// Pathname expansion: every existing path matching `pattern`, sorted.
/// Relative patterns are resolved against `fs.cwd()` and returned relative.
pub async fn expand(fs: &Fs, pattern: &str) -> Vec<String> {
    expand_with(fs, pattern, false).await
}

/// [`expand`], optionally letting wildcards match hidden names (`dotglob`).
pub async fn expand_with(fs: &Fs, pattern: &str, dotglob: bool) -> Vec<String> {
    let absolute = pattern.starts_with('/');
    let segments: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    // Each candidate: (display form, absolute path).
    let mut candidates: Vec<(String, String)> =
        vec![if absolute { (String::new(), "/".into()) } else { (String::new(), fs.cwd().to_string()) }];

    for (index, segment) in segments.iter().enumerate() {
        let last = index + 1 == segments.len();
        let mut next = Vec::new();
        for (shown, abs) in &candidates {
            let join_shown = |name: &str| {
                if shown.is_empty() && !absolute { name.to_string() } else { format!("{shown}/{name}") }
            };
            if !has_meta(segment) {
                let name = unescape(segment);
                let candidate = path::join(abs, &name);
                if last || fs.is_dir(&candidate).await {
                    next.push((join_shown(&name), candidate));
                }
                continue;
            }
            let pat = Pattern::new(segment);
            let Ok(items) = fs.read_dir(abs).await else { continue };
            for item in items {
                if item.name.starts_with('.') && !pat.starts_with_dot() && !dotglob {
                    continue;
                }
                if !pat.matches(&item.name) {
                    continue;
                }
                if !last && item.entry.kind != wendy_io::EntryKind::Dir {
                    continue;
                }
                next.push((join_shown(&item.name), path::join(abs, &item.name)));
            }
        }
        candidates = next;
        if candidates.is_empty() {
            return Vec::new();
        }
    }

    let mut out = Vec::new();
    for (shown, abs) in candidates {
        if fs.exists(&abs).await {
            out.push(if shown.is_empty() { "/".into() } else { shown });
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, t: &str) -> bool {
        Pattern::new(p).matches(t)
    }

    #[test]
    fn matches_shell_patterns() {
        assert!(m("*.rs", "main.rs"));
        assert!(!m("*.rs", "main.rsx"));
        assert!(m("a?c", "abc"));
        assert!(m("[a-c]x", "bx"));
        assert!(!m("[!a-c]x", "bx"));
        assert!(m("[[:digit:]]*", "9lives"));
        assert!(m("*", ""));
        assert!(m("a*b*c", "aXXbYYc"));
        assert!(!m("a*b*c", "aXXbYY"));
        assert!(m(r"\*", "*"));
        assert!(!m(r"\*", "x"));
        assert!(m("[]]", "]"));
        assert!(m("[", "["));
    }

    #[test]
    fn escape_round_trips_and_detects_meta() {
        assert!(has_meta("*.txt"));
        assert!(!has_meta(&escape("*.txt")));
        assert!(Pattern::new(&escape("a*b?")).matches("a*b?"));
        assert_eq!(unescape(r"a\*b"), "a*b");
    }
}
