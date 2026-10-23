//! POSIX regular expressions (BRE / ERE, with the GNU extensions people
//! rely on) translated to Rust `regex` syntax for grep and sed.
//!
//! Back-references (`\1` inside a pattern) have no equivalent in the
//! `regex` crate; they are reported as unsupported rather than mismatched.

pub(crate) fn translate(pat: &str, extended: bool) -> Result<String, String> {
    let chars: Vec<char> = pat.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    // Whether the previous token can take a repetition operator.
    let mut can_repeat = false;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if i + 1 < chars.len() => {
                let n = chars[i + 1];
                i += 2;
                match n {
                    '(' if !extended => {
                        out.push('(');
                        can_repeat = false;
                        continue;
                    }
                    ')' if !extended => {
                        out.push(')');
                    }
                    '{' if !extended => {
                        out.push('{');
                        while i < chars.len() && !(chars[i] == '\\' && chars.get(i + 1) == Some(&'}')) {
                            out.push(chars[i]);
                            i += 1;
                        }
                        out.push('}');
                        i += 2;
                    }
                    '|' if !extended => {
                        out.push('|');
                        can_repeat = false;
                        continue;
                    }
                    '+' | '?' if !extended => out.push(n),
                    '<' | '>' => {
                        out.push_str("\\b");
                        can_repeat = false;
                        continue;
                    }
                    '1'..='9' => return Err("back-references are not supported".into()),
                    'w' | 'W' | 's' | 'S' | 'b' | 'B' => {
                        out.push('\\');
                        out.push(n);
                    }
                    'n' => out.push_str("\\n"),
                    't' => out.push_str("\\t"),
                    other => out.push_str(&regex::escape(&other.to_string())),
                }
                can_repeat = true;
            }
            '[' => {
                let (class, next) = bracket(&chars, i)?;
                out.push_str(&class);
                i = next;
                can_repeat = true;
            }
            '*' => {
                if can_repeat {
                    out.push('*');
                } else {
                    out.push_str("\\*");
                    can_repeat = true;
                }
                i += 1;
            }
            '^' => {
                let anchor = out.is_empty() || out.ends_with('(') || out.ends_with('|');
                out.push_str(if anchor { "^" } else { "\\^" });
                can_repeat = !anchor;
                i += 1;
            }
            '$' => {
                let rest_closes = i + 1 == chars.len()
                    || (extended && matches!(chars.get(i + 1), Some(')') | Some('|')))
                    || (!extended && chars.get(i + 1) == Some(&'\\') && matches!(chars.get(i + 2), Some(')') | Some('|')));
                out.push_str(if rest_closes { "$" } else { "\\$" });
                i += 1;
                can_repeat = !rest_closes;
            }
            '(' | ')' | '|' if extended => {
                out.push(c);
                can_repeat = c == ')';
                i += 1;
            }
            '+' | '?' if extended => {
                if can_repeat {
                    out.push(c);
                } else {
                    out.push('\\');
                    out.push(c);
                    can_repeat = true;
                }
                i += 1;
            }
            '{' if extended => {
                let close = chars[i..].iter().position(|c| *c == '}');
                match close {
                    Some(len) if can_repeat && chars[i + 1..i + len].iter().all(|c| c.is_ascii_digit() || *c == ',') => {
                        out.extend(&chars[i..=i + len]);
                        i += len + 1;
                    }
                    _ => {
                        out.push_str("\\{");
                        i += 1;
                    }
                }
            }
            '.' => {
                out.push('.');
                i += 1;
                can_repeat = true;
            }
            other => {
                out.push_str(&regex::escape(&other.to_string()));
                i += 1;
                can_repeat = true;
            }
        }
    }
    Ok(out)
}

/// A POSIX bracket expression starting at `chars[start] == '['`.
fn bracket(chars: &[char], start: usize) -> Result<(String, usize), String> {
    let mut i = start + 1;
    let mut out = String::from("[");
    if chars.get(i) == Some(&'^') {
        out.push('^');
        i += 1;
    }
    let mut first = true;
    while i < chars.len() {
        let c = chars[i];
        if c == ']' && !first {
            out.push(']');
            return Ok((out, i + 1));
        }
        first = false;
        if c == '[' && matches!(chars.get(i + 1), Some(':') | Some('.') | Some('=')) {
            let kind = chars[i + 1];
            let rest: String = chars[i + 2..].iter().collect();
            let end_marker = format!("{kind}]");
            if let Some(end) = rest.find(&end_marker) {
                let name = &rest[..end];
                if kind == ':' {
                    out.push_str(&format!("[:{name}:]"));
                } else {
                    out.push_str(&regex::escape(name));
                }
                i += 2 + end + 2;
                continue;
            }
        }
        match c {
            '\\' | '[' | ']' | '&' | '~' | '^' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
        i += 1;
    }
    Err("unterminated [".into())
}

pub(crate) fn build(pat: &str, extended: bool, fixed: bool, ignore_case: bool) -> Result<regex::Regex, String> {
    let body = if fixed { regex::escape(pat) } else { translate(pat, extended)? };
    regex::RegexBuilder::new(&body).case_insensitive(ignore_case).build().map_err(|e| {
        let first = e.to_string().lines().last().unwrap_or("invalid regular expression").trim().to_string();
        first.trim_start_matches("error: ").to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pat: &str, ext: bool, text: &str) -> bool {
        build(pat, ext, false, false).unwrap().is_match(text)
    }

    #[test]
    fn bre_and_ere_semantics() {
        assert!(m("a\\(b\\)*c", false, "abbc"));
        assert!(m("a(b)c", false, "a(b)c"));
        assert!(m("a(b)+c", true, "abbc"));
        assert!(m("x\\{2\\}", false, "xx"));
        assert!(!m("^x\\{2\\}$", false, "x"));
        assert!(m("a+", false, "a+"));
        assert!(m("a\\+", false, "aaa"));
        assert!(m("*a", false, "*a"));
        assert!(m("[[:digit:]]\\+", false, "x12"));
        assert!(m("[]x]", false, "]"));
        assert!(m("a.c", false, "abc"));
        assert!(m("\\<foo\\>", false, "a foo b"));
        assert!(m("a^b", false, "a^b"));
        assert!(m("foo|bar", true, "bar"));
        assert!(m("foo\\|bar", false, "bar"));
        assert!(build("\\(a\\)\\1", false, false, false).is_err());
    }
}
