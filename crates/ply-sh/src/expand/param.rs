//! `$name`, `${name...}` and every operator bash supports on them.

use ply_libc::glob::Pattern;

use super::{Ctx, Piece, join_pieces};
use crate::exec::state::{Fds, Value};
use crate::exec::{Ctrl, Shell};
use crate::syntax::{Param, ParamOp, Word, is_name};

/// What a parameter currently holds.
#[derive(Debug, Clone)]
enum Looked {
    Unset,
    Scalar(String),
    /// `$@`, `$*`, `${a[@]}`, `${a[*]}`
    List { items: Vec<String>, star: bool },
}

impl Looked {
    /// Set and (for `colon` forms) non-empty.
    fn is_set(&self, colon: bool) -> bool {
        match self {
            Looked::Unset => false,
            Looked::Scalar(s) => !colon || !s.is_empty(),
            Looked::List { items, .. } => !items.is_empty() && (!colon || items.iter().any(|s| !s.is_empty())),
        }
    }

    fn map(self, f: impl Fn(&str) -> String) -> Looked {
        match self {
            Looked::Unset => Looked::Unset,
            Looked::Scalar(s) => Looked::Scalar(f(&s)),
            Looked::List { items, star } => Looked::List { items: items.iter().map(|s| f(s)).collect(), star },
        }
    }
}

impl Shell {
    pub(crate) async fn expand_param(&mut self, p: &Param, ctx: Ctx, fds: &Fds) -> Result<Vec<Piece>, Ctrl> {
        let index = match &p.index {
            Some(w) => Some(self.expand_string(w, fds).await?),
            None => None,
        };

        // ${!name} indirection, ${!arr[@]} keys.
        let (name, index) = if p.indirect {
            if matches!(index.as_deref(), Some("@") | Some("*")) {
                let keys = self.st.vars.value(&p.name).map(Value::keys).unwrap_or_default();
                let star = index.as_deref() == Some("*");
                return Ok(self.pieces(Looked::List { items: keys, star }, ctx));
            }
            let target = match self.lookup(&p.name, index.as_deref(), fds)? {
                Looked::Scalar(s) if !s.is_empty() => s,
                _ => {
                    self.report(fds, &format!("{}: invalid indirect expansion", p.name));
                    return Err(Ctrl::Abort);
                }
            };
            match target.split_once('[') {
                Some((n, rest)) if rest.ends_with(']') && is_name(n) => {
                    (n.to_string(), Some(rest[..rest.len() - 1].to_string()))
                }
                _ if is_name(&target) || target.chars().all(|c| c.is_ascii_digit()) || matches!(target.as_str(), "@" | "*" | "#" | "?" | "$" | "!" | "-") => {
                    (target, None)
                }
                _ => {
                    self.report(fds, &format!("{target}: invalid variable name"));
                    return Err(Ctrl::Abort);
                }
            }
        } else {
            (p.name.clone(), index)
        };

        let value = self.lookup(&name, index.as_deref(), fds)?;
        let display = match &index {
            Some(i) => format!("{name}[{i}]"),
            None => name.clone(),
        };

        let checks_unset = !matches!(
            p.op,
            ParamOp::Default { .. } | ParamOp::Assign { .. } | ParamOp::Error { .. } | ParamOp::Alt { .. }
        );
        if checks_unset && matches!(value, Looked::Unset) && self.st.opts.nounset && !matches!(name.as_str(), "@" | "*") {
            self.report(fds, &format!("{display}: unbound variable"));
            return Err(Ctrl::Fatal);
        }

        let word_ctx = Ctx { quoted: ctx.quoted, in_param: !ctx.quoted, word_start: true };
        let result = match &p.op {
            ParamOp::None => value,
            ParamOp::Length => match value {
                Looked::List { items, .. } => Looked::Scalar(items.len().to_string()),
                Looked::Scalar(s) => Looked::Scalar(s.chars().count().to_string()),
                Looked::Unset => Looked::Scalar("0".into()),
            },
            ParamOp::Default { colon, word } => {
                if value.is_set(*colon) {
                    value
                } else {
                    return self.expand_parts(&word.0, word_ctx, fds).await;
                }
            }
            ParamOp::Alt { colon, word } => {
                if value.is_set(*colon) {
                    return self.expand_parts(&word.0, word_ctx, fds).await;
                } else {
                    Looked::Scalar(String::new())
                }
            }
            ParamOp::Assign { colon, word } => {
                if value.is_set(*colon) {
                    value
                } else {
                    let text = self.expand_string(word, fds).await?;
                    if !is_name(&name) {
                        self.report(fds, &format!("${display}: cannot assign in this way"));
                        return Err(Ctrl::Abort);
                    }
                    let assigned = match index {
                        Some(i) => self.assign_element(&name, &i, text.clone(), fds),
                        None => self.st.vars.set_str(&name, text.clone()),
                    };
                    if let Err(e) = assigned {
                        self.report(fds, &e);
                        return Err(Ctrl::Abort);
                    }
                    Looked::Scalar(text)
                }
            }
            ParamOp::Error { colon, word } => {
                if value.is_set(*colon) {
                    value
                } else {
                    let msg = self.expand_string(word, fds).await?;
                    let msg = if msg.is_empty() { "parameter null or not set".to_string() } else { msg };
                    self.report(fds, &format!("{display}: {msg}"));
                    return Err(Ctrl::Fatal);
                }
            }
            ParamOp::TrimPrefix { longest, pattern } => {
                let pat = Pattern::new(&self.expand_pattern(pattern, fds).await?);
                value.map(|s| trim_prefix(s, &pat, *longest))
            }
            ParamOp::TrimSuffix { longest, pattern } => {
                let pat = Pattern::new(&self.expand_pattern(pattern, fds).await?);
                value.map(|s| trim_suffix(s, &pat, *longest))
            }
            ParamOp::Replace { all, anchor, pattern, with } => {
                let pat_text = self.expand_pattern(pattern, fds).await?;
                let with = self.expand_string(with, fds).await?;
                if pat_text.is_empty() {
                    value
                } else {
                    let pat = Pattern::new(&pat_text);
                    value.map(|s| replace(s, &pat, &with, *all, *anchor))
                }
            }
            ParamOp::Substring { offset, length } => {
                let offset = self.arith_word(offset, fds).await?;
                let length = match length {
                    Some(w) => Some(self.arith_word(w, fds).await?),
                    None => None,
                };
                match value {
                    Looked::List { items, star } => {
                        // ${@:n} counts $0 as element 0.
                        let items = if matches!(name.as_str(), "@" | "*") {
                            std::iter::once(self.st.arg0.clone()).chain(items).collect()
                        } else {
                            items
                        };
                        Looked::List { items: slice(&items, offset, length), star }
                    }
                    Looked::Scalar(s) => {
                        let chars: Vec<char> = s.chars().collect();
                        Looked::Scalar(slice(&chars, offset, length).into_iter().collect())
                    }
                    Looked::Unset => Looked::Unset,
                }
            }
            ParamOp::Case { upper, all } => value.map(|s| change_case(s, *upper, *all)),
        };
        Ok(self.pieces(result, ctx))
    }

    fn pieces(&self, value: Looked, ctx: Ctx) -> Vec<Piece> {
        match value {
            Looked::Unset => Vec::new(),
            Looked::Scalar(s) => vec![Piece::text(s, ctx.quoted, !ctx.quoted)],
            Looked::List { items, star } => {
                if ctx.quoted && star {
                    let sep = self.ifs().chars().next().map(String::from).unwrap_or_default();
                    return vec![Piece::text(items.join(&sep), true, false)];
                }
                let mut out = Vec::new();
                for (i, item) in items.into_iter().enumerate() {
                    if i > 0 {
                        out.push(Piece::Break);
                    }
                    out.push(Piece::text(item, ctx.quoted, !ctx.quoted));
                }
                out
            }
        }
    }

    /// Current value of a parameter (no operators applied).
    fn lookup(&mut self, name: &str, index: Option<&str>, fds: &Fds) -> Result<Looked, Ctrl> {
        let st = &self.st;
        let scalar = |s: String| Ok(Looked::Scalar(s));
        match name {
            "@" | "*" => return Ok(Looked::List { items: st.positional.clone(), star: name == "*" }),
            "#" => return scalar(st.positional.len().to_string()),
            "?" => return scalar(st.last_status.to_string()),
            "$" => return scalar("1".into()),
            "!" => return scalar("2".into()),
            "-" => return scalar(st.opts.flags()),
            "0" => return scalar(st.arg0.clone()),
            _ => {}
        }
        if let Ok(n) = name.parse::<usize>() {
            return Ok(match st.positional.get(n.wrapping_sub(1)) {
                Some(v) if n > 0 => Looked::Scalar(v.clone()),
                _ => Looked::Unset,
            });
        }
        match name {
            "RANDOM" => {
                self.st.random = self.st.random.wrapping_mul(1103515245).wrapping_add(12345);
                return scalar(((self.st.random >> 16) & 0x7fff).to_string());
            }
            "LINENO" => return scalar(self.st.line.to_string()),
            "PPID" | "UID" | "EUID" => return scalar(if name == "PPID" { "0" } else { "1000" }.into()),
            "SECONDS" => return scalar("0".into()),
            _ => {}
        }
        let Some(value) = self.st.vars.value(name).cloned() else { return Ok(Looked::Unset) };
        Ok(match index {
            None => value.scalar().map_or(Looked::Unset, Looked::Scalar),
            Some("@") | Some("*") => Looked::List { items: value.elements(), star: index == Some("*") },
            Some(key) => match value {
                Value::Assoc(m) => m.get(key).cloned().map_or(Looked::Unset, Looked::Scalar),
                Value::Indexed(m) => {
                    let i = self.arith(key, fds)?;
                    let i = if i < 0 { m.keys().next_back().map_or(i, |max| max + 1 + i) } else { i };
                    m.get(&i).cloned().map_or(Looked::Unset, Looked::Scalar)
                }
                Value::Str(s) => {
                    if self.arith(key, fds)? == 0 { Looked::Scalar(s) } else { Looked::Unset }
                }
            },
        })
    }

    /// `name[key]=value` for indexed and associative arrays.
    pub(crate) fn assign_element(&mut self, name: &str, key: &str, value: String, fds: &Fds) -> Result<(), String> {
        match self.st.vars.value(name).cloned() {
            Some(Value::Assoc(mut m)) => {
                m.insert(key.to_string(), value);
                self.st.vars.set(name, Value::Assoc(m))
            }
            other => {
                let i = self.arith(key, fds).map_err(|_| format!("{name}[{key}]: bad array subscript"))?;
                let mut m = match other {
                    Some(Value::Indexed(m)) => m,
                    Some(Value::Str(s)) => std::iter::once((0, s)).collect(),
                    _ => Default::default(),
                };
                let i = if i < 0 { m.keys().next_back().map_or(i, |max| max + 1 + i) } else { i };
                if i < 0 {
                    return Err(format!("{name}[{key}]: bad array subscript"));
                }
                m.insert(i, value);
                self.st.vars.set(name, Value::Indexed(m))
            }
        }
    }

    /// Words joined with spaces, for places that want one string.
    #[allow(dead_code)]
    pub(crate) async fn expand_joined(&mut self, word: &Word, fds: &Fds) -> Result<String, Ctrl> {
        let pieces = self.expand_parts(&word.0, Ctx { quoted: true, in_param: false, word_start: true }, fds).await?;
        Ok(join_pieces(&pieces))
    }
}

fn char_boundaries(s: &str) -> Vec<usize> {
    s.char_indices().map(|(i, _)| i).chain(std::iter::once(s.len())).collect()
}

fn trim_prefix(s: &str, pat: &Pattern, longest: bool) -> String {
    let bounds = char_boundaries(s);
    let order: Box<dyn Iterator<Item = &usize>> = if longest { Box::new(bounds.iter().rev()) } else { Box::new(bounds.iter()) };
    for &end in order {
        if pat.matches(&s[..end]) {
            return s[end..].to_string();
        }
    }
    s.to_string()
}

fn trim_suffix(s: &str, pat: &Pattern, longest: bool) -> String {
    let bounds = char_boundaries(s);
    let order: Box<dyn Iterator<Item = &usize>> = if longest { Box::new(bounds.iter()) } else { Box::new(bounds.iter().rev()) };
    for &start in order {
        if pat.matches(&s[start..]) {
            return s[..start].to_string();
        }
    }
    s.to_string()
}

/// Longest match starting at byte `start`, as an end offset.
fn longest_match_at(s: &str, start: usize, pat: &Pattern, bounds: &[usize]) -> Option<usize> {
    bounds.iter().rev().copied().filter(|end| *end >= start).find(|end| pat.matches(&s[start..*end]))
}

fn replace(s: &str, pat: &Pattern, with: &str, all: bool, anchor: Option<char>) -> String {
    let bounds = char_boundaries(s);
    match anchor {
        Some('#') => match longest_match_at(s, 0, pat, &bounds) {
            Some(end) => format!("{with}{}", &s[end..]),
            None => s.to_string(),
        },
        Some('%') => {
            for &start in &bounds {
                if pat.matches(&s[start..]) {
                    return format!("{}{with}", &s[..start]);
                }
            }
            s.to_string()
        }
        _ => {
            let mut out = String::new();
            let mut i = 0;
            let mut replaced = false;
            while i < bounds.len() {
                let start = bounds[i];
                if start == s.len() {
                    break;
                }
                if !(replaced && !all)
                    && let Some(end) = longest_match_at(s, start, pat, &bounds).filter(|e| *e > start)
                {
                    out.push_str(with);
                    replaced = true;
                    i = bounds.iter().position(|b| *b == end).unwrap_or(bounds.len());
                    continue;
                }
                let next = bounds[i + 1];
                out.push_str(&s[start..next]);
                i += 1;
            }
            out
        }
    }
}

fn slice<T: Clone>(items: &[T], offset: i64, length: Option<i64>) -> Vec<T> {
    let len = items.len() as i64;
    let start = if offset < 0 { (len + offset).max(0) } else { offset.min(len) };
    let end = match length {
        None => len,
        Some(l) if l < 0 => (len + l).max(start),
        Some(l) => (start + l).min(len),
    };
    if start >= end { Vec::new() } else { items[start as usize..end as usize].to_vec() }
}

fn change_case(s: &str, upper: bool, all: bool) -> String {
    let convert = |c: char| -> String { if upper { c.to_uppercase().collect() } else { c.to_lowercase().collect() } };
    if all {
        s.chars().map(convert).collect()
    } else {
        let mut chars = s.chars();
        match chars.next() {
            Some(first) => convert(first) + chars.as_str(),
            None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_replaces_like_bash() {
        let p = |s: &str| Pattern::new(s);
        assert_eq!(trim_prefix("a/b/c", &p("*/"), false), "b/c");
        assert_eq!(trim_prefix("a/b/c", &p("*/"), true), "c");
        assert_eq!(trim_suffix("f.tar.gz", &p(".*"), false), "f.tar");
        assert_eq!(trim_suffix("f.tar.gz", &p(".*"), true), "f");
        assert_eq!(replace("aaa", &p("a"), "b", false, None), "baa");
        assert_eq!(replace("aaa", &p("a"), "b", true, None), "bbb");
        assert_eq!(replace("hello", &p("l*"), "L", false, None), "heL");
        assert_eq!(replace("abc", &p("a"), "X", false, Some('#')), "Xbc");
        assert_eq!(replace("abc", &p("c"), "X", false, Some('%')), "abX");
        assert_eq!(replace("abc", &p("b"), "X", false, Some('#')), "abc");
        assert_eq!(slice(&[1, 2, 3, 4], 1, Some(2)), [2, 3]);
        assert_eq!(slice(&[1, 2, 3, 4], -1, None), [4]);
        assert_eq!(slice(&[1, 2, 3, 4], 1, Some(-1)), [2, 3]);
        assert_eq!(change_case("hello", true, false), "Hello");
        assert_eq!(change_case("HeLLo", false, true), "hello");
    }
}
