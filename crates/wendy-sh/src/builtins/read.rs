//! `read`, `mapfile` / `readarray`, `getopts`.

use std::collections::BTreeMap;

use crate::exec::state::{Fds, Value};
use crate::exec::{Shell, Status};
use crate::syntax::is_name;

pub(crate) fn read(sh: &mut Shell, args: &[String], fds: &Fds) -> Status {
    let mut raw = false;
    let mut array: Option<String> = None;
    let mut delim = b'\n';
    let mut nchars: Option<usize> = None;
    let mut names = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if !names.is_empty() || !arg.starts_with('-') || arg == "-" {
            names.push(arg.clone());
            i += 1;
            continue;
        }
        let mut chars = arg[1..].chars();
        while let Some(c) = chars.next() {
            let mut value = || -> Option<String> {
                let rest: String = chars.by_ref().collect();
                if !rest.is_empty() {
                    return Some(rest);
                }
                i += 1;
                args.get(i).cloned()
            };
            match c {
                'r' => raw = true,
                's' | 'e' => {}
                'a' => array = value(),
                'd' => delim = value().and_then(|d| d.bytes().next()).unwrap_or(0),
                'n' | 'N' => nchars = value().and_then(|n| n.parse().ok()),
                'p' | 't' | 'u' | 'i' => {
                    value();
                }
                other => {
                    sh.report(fds, &format!("read: -{other}: invalid option"));
                    return Ok(2);
                }
            }
        }
        i += 1;
    }

    // Read one logical line (backslash-newline joins lines unless -r).
    let mut line = Vec::new();
    let mut hit_eof = false;
    loop {
        let chunk = match nchars {
            Some(n) => sh_read_n(fds, n - line.len().min(n)),
            None => fds.stdin.read_until(delim),
        };
        let Some(mut chunk) = chunk else {
            hit_eof = true;
            break;
        };
        let terminated = nchars.is_none() && chunk.last() == Some(&delim);
        if terminated {
            chunk.pop();
        } else if nchars.is_none() {
            hit_eof = true;
        }
        if !raw && terminated && chunk.last() == Some(&b'\\') && delim == b'\n' {
            chunk.pop();
            line.extend(chunk);
            continue;
        }
        line.extend(chunk);
        break;
    }
    let no_data = hit_eof && line.is_empty();
    let text = String::from_utf8_lossy(&line).into_owned();
    let text = if raw { text } else { unescape(&text) };

    let ifs = sh.ifs();
    if let Some(array) = array {
        let fields = split(&text, &ifs, usize::MAX);
        let map: BTreeMap<i64, String> = fields.into_iter().enumerate().map(|(i, f)| (i as i64, f)).collect();
        if let Err(e) = sh.st.vars.set(&array, Value::Indexed(map)) {
            sh.report(fds, &e);
            return Ok(1);
        }
    } else if names.is_empty() {
        let _ = sh.st.vars.set_str("REPLY", text);
    } else {
        for name in &names {
            if !is_name(name) {
                sh.report(fds, &format!("read: `{name}': not a valid identifier"));
                return Ok(1);
            }
        }
        let fields = split(&text, &ifs, names.len());
        for (i, name) in names.iter().enumerate() {
            let value = fields.get(i).cloned().unwrap_or_default();
            if let Err(e) = sh.st.vars.set_str(name, value) {
                sh.report(fds, &e);
                return Ok(1);
            }
        }
    }
    Ok(if hit_eof && (no_data || nchars.is_none()) { 1 } else { 0 })
}

fn sh_read_n(fds: &Fds, n: usize) -> Option<Vec<u8>> {
    if n == 0 { Some(Vec::new()) } else { fds.stdin.read_n(n) }
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Split for `read`: at most `max` fields, the last one keeps the rest of
/// the line (minus trailing IFS whitespace).
fn split(text: &str, ifs: &str, max: usize) -> Vec<String> {
    let is_ws = |c: char| ifs.contains(c) && c.is_whitespace();
    let is_sep = |c: char| ifs.contains(c);
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() && is_ws(chars[i]) {
        i += 1;
    }
    let mut fields = Vec::new();
    while i < chars.len() {
        if fields.len() + 1 == max {
            let mut rest: String = chars[i..].iter().collect();
            while rest.ends_with(is_ws) {
                rest.pop();
            }
            fields.push(rest);
            return fields;
        }
        let start = i;
        while i < chars.len() && !is_sep(chars[i]) {
            i += 1;
        }
        fields.push(chars[start..i].iter().collect());
        // Skip one separator (plus surrounding IFS whitespace).
        while i < chars.len() && is_ws(chars[i]) {
            i += 1;
        }
        if i < chars.len() && is_sep(chars[i]) && !is_ws(chars[i]) {
            i += 1;
            while i < chars.len() && is_ws(chars[i]) {
                i += 1;
            }
        }
    }
    fields
}

pub(crate) fn mapfile(sh: &mut Shell, cmd: &str, args: &[String], fds: &Fds) -> Status {
    let mut strip = false;
    let mut count: Option<usize> = None;
    let mut skip = 0usize;
    let mut delim = b'\n';
    let mut name = "MAPFILE".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-t" => strip = true,
            "-n" => {
                i += 1;
                count = args.get(i).and_then(|n| n.parse().ok()).filter(|n| *n > 0);
            }
            "-s" => {
                i += 1;
                skip = args.get(i).and_then(|n| n.parse().ok()).unwrap_or(0);
            }
            "-d" => {
                i += 1;
                delim = args.get(i).and_then(|d| d.bytes().next()).unwrap_or(0);
            }
            "-u" | "-C" | "-c" | "-O" => i += 1,
            other if other.starts_with('-') => {
                sh.report(fds, &format!("{cmd}: {other}: invalid option"));
                return Ok(2);
            }
            other => name = other.to_string(),
        }
        i += 1;
    }
    let mut map = BTreeMap::new();
    let mut index = 0i64;
    let mut seen = 0usize;
    while let Some(mut line) = fds.stdin.read_until(delim) {
        seen += 1;
        if seen <= skip {
            continue;
        }
        if strip && line.last() == Some(&delim) {
            line.pop();
        }
        map.insert(index, String::from_utf8_lossy(&line).into_owned());
        index += 1;
        if count.is_some_and(|c| index as usize >= c) {
            break;
        }
    }
    if let Err(e) = sh.st.vars.set(&name, Value::Indexed(map)) {
        sh.report(fds, &e);
        return Ok(1);
    }
    Ok(0)
}

pub(crate) fn getopts(sh: &mut Shell, args: &[String], fds: &Fds) -> i32 {
    let [optstring, name, rest @ ..] = args else {
        sh.report(fds, "getopts: usage: getopts optstring name [arg ...]");
        return 2;
    };
    let silent = optstring.starts_with(':');
    let spec = optstring.trim_start_matches(':');
    let argv: Vec<String> = if rest.is_empty() { sh.st.positional.clone() } else { rest.to_vec() };
    let optind: usize = sh.st.vars.value("OPTIND").and_then(Value::scalar).and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    if sh.st.getopts_pos == 0 {
        sh.st.getopts_pos = 1;
    }
    let set = |sh: &mut Shell, k: &str, v: &str| {
        let _ = sh.st.vars.set_str(k, v);
    };
    let Some(arg) = argv.get(optind - 1) else {
        set(sh, name, "?");
        return 1;
    };
    if arg == "--" {
        set(sh, "OPTIND", &(optind + 1).to_string());
        set(sh, name, "?");
        sh.st.getopts_pos = 1;
        return 1;
    }
    if !arg.starts_with('-') || arg == "-" {
        set(sh, name, "?");
        return 1;
    }
    let chars: Vec<char> = arg.chars().collect();
    let pos = sh.st.getopts_pos.min(chars.len() - 1).max(1);
    let opt = chars[pos];
    let advance = |sh: &mut Shell, next_pos: usize, consumed_args: usize| {
        if next_pos >= chars.len() || consumed_args > 0 {
            set(sh, "OPTIND", &(optind + 1 + consumed_args.saturating_sub(1)).to_string());
            sh.st.getopts_pos = 1;
        } else {
            sh.st.getopts_pos = next_pos;
        }
    };
    match spec.find(opt) {
        Some(idx) if opt != ':' => {
            let needs_arg = spec[idx + opt.len_utf8()..].starts_with(':');
            set(sh, name, &opt.to_string());
            if !needs_arg {
                let _ = sh.st.vars.unset("OPTARG");
                advance(sh, pos + 1, 0);
                return 0;
            }
            if pos + 1 < chars.len() {
                set(sh, "OPTARG", &chars[pos + 1..].iter().collect::<String>());
                advance(sh, chars.len(), 1);
            } else if let Some(value) = argv.get(optind) {
                set(sh, "OPTARG", &value.clone());
                advance(sh, chars.len(), 2);
            } else {
                if silent {
                    set(sh, name, ":");
                    set(sh, "OPTARG", &opt.to_string());
                } else {
                    sh.report(fds, &format!("option requires an argument -- {opt}"));
                    set(sh, name, "?");
                    let _ = sh.st.vars.unset("OPTARG");
                }
                advance(sh, chars.len(), 1);
            }
            0
        }
        _ => {
            if silent {
                set(sh, "OPTARG", &opt.to_string());
            } else {
                sh.report(fds, &format!("illegal option -- {opt}"));
                let _ = sh.st.vars.unset("OPTARG");
            }
            set(sh, name, "?");
            advance(sh, pos + 1, 0);
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn splits_like_read() {
        assert_eq!(split("  a  b  c  ", " \t\n", 2), ["a", "b  c"]);
        assert_eq!(split("a:b:c", ":", 3), ["a", "b", "c"]);
        assert_eq!(split("a::c", ":", 3), ["a", "", "c"]);
        assert_eq!(split("one", " ", 3), ["one"]);
        assert_eq!(split("x y z", " ", usize::MAX), ["x", "y", "z"]);
    }
}
