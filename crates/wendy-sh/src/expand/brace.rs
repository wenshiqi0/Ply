//! Brace expansion: `a{b,c}d` → `abd acd`, `{1..5}`, `{a..e..2}`, `{01..10}`.
//!
//! Runs first, on the parsed word, and only looks at unquoted literal text,
//! so `"{a,b}"` and `${x}` are untouched (as in bash).

use crate::syntax::{Part, Word};

pub fn expand(word: &Word) -> Vec<Word> {
    for (index, part) in word.0.iter().enumerate() {
        let Part::Lit(text) = part else { continue };
        let Some((open, close, alternatives)) = find_group(text) else { continue };
        let mut out = Vec::new();
        for alt in alternatives {
            let mut parts: Vec<Part> = word.0[..index].to_vec();
            parts.push(Part::Lit(format!("{}{alt}{}", &text[..open], &text[close + 1..])));
            parts.extend(word.0[index + 1..].iter().cloned());
            out.extend(expand(&Word(merge(parts))));
        }
        return out;
    }
    vec![word.clone()]
}

fn merge(parts: Vec<Part>) -> Vec<Part> {
    let mut out: Vec<Part> = Vec::new();
    for part in parts {
        match (out.last_mut(), part) {
            (Some(Part::Lit(a)), Part::Lit(b)) => a.push_str(&b),
            (_, part) => out.push(part),
        }
    }
    out
}

/// First expandable `{...}` in `text`: (open, close, alternatives).
fn find_group(text: &str) -> Option<(usize, usize, Vec<String>)> {
    let bytes = text.as_bytes();
    let mut start = 0;
    while let Some(rel) = text[start..].find('{') {
        let open = start + rel;
        // Find the matching close brace, tracking nesting.
        let mut depth = 0;
        let mut close = None;
        for (i, b) in bytes.iter().enumerate().skip(open) {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close?;
        let inner = &text[open + 1..close];
        if let Some(alts) = comma_list(inner).or_else(|| sequence(inner)) {
            return Some((open, close, alts));
        }
        start = open + 1;
    }
    None
}

fn comma_list(inner: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut current = String::new();
    for c in inner.chars() {
        match c {
            '{' => {
                depth += 1;
                current.push(c);
            }
            '}' => {
                depth -= 1;
                current.push(c);
            }
            ',' if depth == 0 => parts.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    parts.push(current);
    if parts.len() < 2 { None } else { Some(parts) }
}

fn sequence(inner: &str) -> Option<Vec<String>> {
    let pieces: Vec<&str> = inner.split("..").collect();
    if !(2..=3).contains(&pieces.len()) {
        return None;
    }
    let step: i64 = match pieces.get(2) {
        Some(s) => s.parse().ok()?,
        None => 1,
    };
    let step = step.unsigned_abs().max(1) as i64;
    if let (Ok(a), Ok(b)) = (pieces[0].parse::<i64>(), pieces[1].parse::<i64>()) {
        let width = [pieces[0], pieces[1]]
            .iter()
            .filter(|p| p.trim_start_matches('-').starts_with('0') && p.trim_start_matches('-').len() > 1)
            .map(|p| p.len())
            .max()
            .unwrap_or(0);
        let mut out = Vec::new();
        let mut v = a;
        loop {
            out.push(if width > 0 { pad(v, width) } else { v.to_string() });
            if v == b || out.len() > 100_000 {
                break;
            }
            v = if a < b { v + step } else { v - step };
            if (a < b && v > b) || (a > b && v < b) {
                break;
            }
        }
        return Some(out);
    }
    let (a, b) = (single_char(pieces[0])?, single_char(pieces[1])?);
    let (a, b) = (a as i64, b as i64);
    let mut out = Vec::new();
    let mut v = a;
    loop {
        out.push(char::from_u32(v as u32)?.to_string());
        if v == b {
            break;
        }
        v = if a < b { v + step } else { v - step };
        if (a < b && v > b) || (a > b && v < b) {
            break;
        }
    }
    Some(out)
}

fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let c = chars.next()?;
    (chars.next().is_none() && c.is_ascii_alphabetic()).then_some(c)
}

fn pad(v: i64, width: usize) -> String {
    if v < 0 { format!("-{:0>w$}", -v, w = width - 1) } else { format!("{v:0>width$}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(src: &str) -> Vec<String> {
        expand(&Word::lit(src))
            .into_iter()
            .map(|w| match w.0.as_slice() {
                [Part::Lit(s)] => s.clone(),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn expands_lists_sequences_and_nesting() {
        assert_eq!(texts("a{b,c}d"), ["abd", "acd"]);
        assert_eq!(texts("{1..3}"), ["1", "2", "3"]);
        assert_eq!(texts("{3..1}"), ["3", "2", "1"]);
        assert_eq!(texts("{01..03}"), ["01", "02", "03"]);
        assert_eq!(texts("{a..e..2}"), ["a", "c", "e"]);
        assert_eq!(texts("{a,b{1,2}}"), ["a", "b1", "b2"]);
        assert_eq!(texts("x{a,b}{1,2}"), ["xa1", "xa2", "xb1", "xb2"]);
        assert_eq!(texts("{a}"), ["{a}"]);
        assert_eq!(texts("{}"), ["{}"]);
        assert_eq!(texts("{a,}"), ["a", ""]);
    }
}
