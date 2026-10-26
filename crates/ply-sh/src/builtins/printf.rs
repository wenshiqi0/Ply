//! `echo` and `printf`.

use crate::exec::state::Fds;
use crate::exec::{Shell, Status};

pub(crate) fn echo(args: &[String], fds: &Fds) -> i32 {
    let mut newline = true;
    let mut escapes = false;
    let mut start = 0;
    for arg in args {
        let Some(flags) = arg.strip_prefix('-') else { break };
        if flags.is_empty() || !flags.chars().all(|c| matches!(c, 'n' | 'e' | 'E')) {
            break;
        }
        for c in flags.chars() {
            match c {
                'n' => newline = false,
                'e' => escapes = true,
                _ => escapes = false,
            }
        }
        start += 1;
    }
    let text = args[start..].join(" ");
    if escapes {
        let (s, stop) = interpret_escapes(&text, true);
        fds.out.write_str(&s);
        if stop {
            return 0;
        }
    } else {
        fds.out.write_str(&text);
    }
    if newline {
        fds.out.write_str("\n");
    }
    0
}

/// Backslash escapes as in `echo -e` / `%b`. Returns (text, stopped by `\c`).
pub(crate) fn interpret_escapes(s: &str, echo_octal: bool) -> (String, bool) {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '\\' || i + 1 >= chars.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let e = chars[i + 1];
        i += 2;
        match e {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'f' => out.push('\x0c'),
            'v' => out.push('\x0b'),
            'e' | 'E' => out.push('\x1b'),
            '\\' => out.push('\\'),
            'c' => return (out, true),
            'x' => {
                let (v, n) = take_digits(&chars[i..], 16, 2);
                if n == 0 {
                    out.push_str("\\x");
                } else {
                    out.push(char::from_u32(v).unwrap_or('?'));
                    i += n;
                }
            }
            'u' | 'U' => {
                let (v, n) = take_digits(&chars[i..], 16, if e == 'u' { 4 } else { 8 });
                if n == 0 {
                    out.push('\\');
                    out.push(e);
                } else {
                    out.push(char::from_u32(v).unwrap_or('?'));
                    i += n;
                }
            }
            '0' if echo_octal => {
                let (v, n) = take_digits(&chars[i..], 8, 3);
                out.push(char::from_u32(v).unwrap_or('?'));
                i += n;
            }
            '0'..='7' if !echo_octal => {
                let (v, n) = take_digits(&chars[i - 1..], 8, 3);
                out.push(char::from_u32(v).unwrap_or('?'));
                i += n - 1;
            }
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    (out, false)
}

fn take_digits(chars: &[char], radix: u32, max: usize) -> (u32, usize) {
    let mut v = 0u32;
    let mut n = 0;
    while n < max && n < chars.len() && chars[n].is_digit(radix) {
        v = v * radix + chars[n].to_digit(radix).unwrap_or(0);
        n += 1;
    }
    (v, n)
}

pub(crate) fn printf(sh: &mut Shell, args: &[String], fds: &Fds) -> Status {
    let (target, args) = match args {
        [flag, var, rest @ ..] if flag == "-v" => (Some(var.clone()), rest),
        _ => (None, args),
    };
    let args = if args.first().map(String::as_str) == Some("--") { &args[1..] } else { args };
    let Some((format, values)) = args.split_first() else {
        sh.report(fds, "printf: usage: printf [-v var] format [arguments]");
        return Ok(2);
    };
    let mut errors = Vec::new();
    let text = format_all(format, values, &mut errors);
    for e in &errors {
        sh.report(fds, &format!("printf: {e}"));
    }
    match target {
        Some(var) => {
            if let Err(e) = sh.st.vars.set_str(&var, text) {
                sh.report(fds, &e);
                return Ok(1);
            }
        }
        None => fds.out.write_str(&text),
    }
    Ok(if errors.is_empty() { 0 } else { 1 })
}

/// Apply `format` to `values`, reusing the format while values remain.
pub(crate) fn format_all(format: &str, values: &[String], errors: &mut Vec<String>) -> String {
    let mut out = String::new();
    let mut next = 0;
    loop {
        let (text, consumed, stop) = format_once(format, &values[next.min(values.len())..], errors);
        out.push_str(&text);
        next += consumed;
        if stop || consumed == 0 || next >= values.len() {
            break;
        }
    }
    out
}

fn format_once(format: &str, values: &[String], errors: &mut Vec<String>) -> (String, usize, bool) {
    let chars: Vec<char> = format.chars().collect();
    let mut out = String::new();
    let mut used = 0;
    let mut i = 0;
    let take = |used: &mut usize| -> Option<String> {
        let v = values.get(*used).cloned();
        *used += 1;
        v
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            // Escapes in the format itself (octal is \NNN here).
            let rest: String = chars[i..].iter().collect();
            let end = escape_len(&chars[i..]);
            let (s, stop) = interpret_escapes(&rest[..rest.char_indices().nth(end).map_or(rest.len(), |(b, _)| b)], false);
            out.push_str(&s);
            if stop {
                return (out, used, true);
            }
            i += end;
            continue;
        }
        if c != '%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        if chars.get(i) == Some(&'%') {
            out.push('%');
            i += 1;
            continue;
        }
        let mut flags = String::new();
        while let Some(&f) = chars.get(i).filter(|f| "-+ #0".contains(**f)) {
            flags.push(f);
            i += 1;
        }
        let mut width = String::new();
        if chars.get(i) == Some(&'*') {
            width = take(&mut used).unwrap_or_default();
            i += 1;
        } else {
            while let Some(&d) = chars.get(i).filter(|d| d.is_ascii_digit()) {
                width.push(d);
                i += 1;
            }
        }
        let mut precision: Option<String> = None;
        if chars.get(i) == Some(&'.') {
            i += 1;
            let mut p = String::new();
            if chars.get(i) == Some(&'*') {
                p = take(&mut used).unwrap_or_default();
                i += 1;
            } else {
                while let Some(&d) = chars.get(i).filter(|d| d.is_ascii_digit()) {
                    p.push(d);
                    i += 1;
                }
            }
            precision = Some(p);
        }
        while chars.get(i).is_some_and(|c| "hlLjzt".contains(*c)) {
            i += 1;
        }
        // TODO(ply-sh): `%(fmt)T` needs a strftime implementation in libc
        // (shared with `date`); until then it fails loudly instead of guessing.
        if chars.get(i) == Some(&'(') {
            errors.push("`%(...)T': time formats are not supported yet".into());
            return (out, used, true);
        }
        let Some(&conv) = chars.get(i) else {
            out.push('%');
            break;
        };
        i += 1;
        let width_n: usize = width.trim_start_matches('-').parse().unwrap_or(0);
        let left = flags.contains('-') || width.starts_with('-');
        let flags = if width.starts_with('-') { format!("{flags}-") } else { flags };
        let precision_n: Option<usize> = precision.as_ref().map(|p| p.parse().unwrap_or(0));
        let arg = take(&mut used);
        let piece = match conv {
            's' => {
                let s = arg.unwrap_or_default();
                match precision_n {
                    Some(p) => s.chars().take(p).collect(),
                    None => s,
                }
            }
            'b' => {
                let (s, stop) = interpret_escapes(&arg.unwrap_or_default(), true);
                if stop {
                    out.push_str(&s);
                    return (out, used, true);
                }
                s
            }
            'c' => arg.and_then(|s| s.chars().next()).map(String::from).unwrap_or_default(),
            'q' => quote(&arg.unwrap_or_default()),
            'd' | 'i' | 'u' => {
                let n = to_int(arg.as_deref(), errors);
                let digits = n.unsigned_abs().to_string();
                let digits = match precision_n {
                    Some(p) if digits.len() < p => format!("{digits:0>p$}"),
                    _ => digits,
                };
                let sign = if n < 0 {
                    "-"
                } else if flags.contains('+') {
                    "+"
                } else if flags.contains(' ') {
                    " "
                } else {
                    ""
                };
                numeric(sign, &digits, &flags, width_n, left, precision_n.is_some())
            }
            'o' | 'x' | 'X' => {
                let n = to_int(arg.as_deref(), errors) as u64;
                let digits = match conv {
                    'o' => format!("{n:o}"),
                    'x' => format!("{n:x}"),
                    _ => format!("{n:X}"),
                };
                let prefix = match (flags.contains('#'), conv, n) {
                    (true, 'o', _) if !digits.starts_with('0') => "0",
                    (true, 'x', 1..) => "0x",
                    (true, 'X', 1..) => "0X",
                    _ => "",
                };
                numeric(prefix, &digits, &flags, width_n, left, precision_n.is_some())
            }
            'f' | 'F' | 'e' | 'E' | 'g' | 'G' => {
                let v = to_float(arg.as_deref(), errors);
                let p = precision_n.unwrap_or(6);
                let body = match conv {
                    'f' | 'F' => format!("{:.*}", p, v.abs()),
                    'e' | 'E' => c_exp(v.abs(), p, conv == 'E'),
                    _ => c_general(v.abs(), p, conv == 'G', flags.contains('#')),
                };
                let sign = if v.is_sign_negative() && v != 0.0 {
                    "-"
                } else if flags.contains('+') {
                    "+"
                } else if flags.contains(' ') {
                    " "
                } else {
                    ""
                };
                numeric(sign, &body, &flags, width_n, left, false)
            }
            other => {
                errors.push(format!("%{other}: invalid format character"));
                return (out, used, true);
            }
        };
        if matches!(conv, 's' | 'b' | 'c' | 'q') {
            out.push_str(&pad(&piece, &flags, &width));
        } else {
            out.push_str(&piece);
        }
    }
    (out, used.min(values.len()), false)
}

fn escape_len(chars: &[char]) -> usize {
    match chars.get(1) {
        None => 1,
        Some('x') => 2 + chars[2..].iter().take(2).take_while(|c| c.is_ascii_hexdigit()).count(),
        Some('u') => 2 + chars[2..].iter().take(4).take_while(|c| c.is_ascii_hexdigit()).count(),
        Some('U') => 2 + chars[2..].iter().take(8).take_while(|c| c.is_ascii_hexdigit()).count(),
        Some(c) if c.is_digit(8) => 1 + chars[1..].iter().take(3).take_while(|c| c.is_digit(8)).count(),
        Some(_) => 2,
    }
}

fn pad(s: &str, flags: &str, width: &str) -> String {
    let width_n: usize = width.trim_start_matches('-').parse().unwrap_or(0);
    let len = s.chars().count();
    if len >= width_n {
        return s.to_string();
    }
    let fill = " ".repeat(width_n - len);
    if flags.contains('-') || width.starts_with('-') { format!("{s}{fill}") } else { format!("{fill}{s}") }
}

fn numeric(prefix: &str, digits: &str, flags: &str, width: usize, left: bool, has_precision: bool) -> String {
    let len = prefix.len() + digits.chars().count();
    if len >= width {
        return format!("{prefix}{digits}");
    }
    let fill = width - len;
    if left {
        format!("{prefix}{digits}{}", " ".repeat(fill))
    } else if flags.contains('0') && !has_precision {
        format!("{prefix}{}{digits}", "0".repeat(fill))
    } else {
        format!("{}{prefix}{digits}", " ".repeat(fill))
    }
}

fn to_int(arg: Option<&str>, errors: &mut Vec<String>) -> i64 {
    let Some(raw) = arg else { return 0 };
    let s = raw.trim();
    if s.is_empty() {
        return 0;
    }
    if let Some(c) = s.strip_prefix('\'').or_else(|| s.strip_prefix('"')) {
        return c.chars().next().map_or(0, |c| c as i64);
    }
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let parsed = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16)
    } else if body.len() > 1 && body.starts_with('0') {
        i64::from_str_radix(&body[1..], 8)
    } else {
        body.parse()
    };
    match parsed {
        Ok(v) => if neg { -v } else { v },
        Err(_) => {
            errors.push(format!("{raw}: invalid number"));
            let digits: String = body.chars().take_while(|c| c.is_ascii_digit()).collect();
            let v = digits.parse().unwrap_or(0);
            if neg { -v } else { v }
        }
    }
}

fn to_float(arg: Option<&str>, errors: &mut Vec<String>) -> f64 {
    let Some(raw) = arg else { return 0.0 };
    let s = raw.trim();
    if s.is_empty() {
        return 0.0;
    }
    if let Some(c) = s.strip_prefix('\'').or_else(|| s.strip_prefix('"')) {
        return c.chars().next().map_or(0.0, |c| c as u32 as f64);
    }
    s.parse().unwrap_or_else(|_| {
        errors.push(format!("{raw}: invalid number"));
        0.0
    })
}

fn c_exp(v: f64, p: usize, upper: bool) -> String {
    let s = format!("{:.*e}", p, v);
    let (mantissa, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let e = if upper { 'E' } else { 'e' };
    format!("{mantissa}{e}{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
}

fn c_general(v: f64, p: usize, upper: bool, keep_zeros: bool) -> String {
    let p = p.max(1);
    if v == 0.0 {
        return "0".into();
    }
    let exp = v.abs().log10().floor() as i32;
    let s = if exp < -4 || exp >= p as i32 {
        let s = c_exp(v, p - 1, upper);
        if keep_zeros {
            return s;
        }
        let e = if upper { 'E' } else { 'e' };
        let (m, rest) = s.split_once(e).unwrap_or((&s, ""));
        let m = if m.contains('.') { m.trim_end_matches('0').trim_end_matches('.') } else { m };
        return format!("{m}{e}{rest}");
    } else {
        format!("{:.*}", (p as i32 - 1 - exp).max(0) as usize, v)
    };
    if keep_zeros || !s.contains('.') { s } else { s.trim_end_matches('0').trim_end_matches('.').to_string() }
}

/// `%q`: quote so the shell would read the value back.
fn quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "_-./:,@%+=".contains(c)) {
        return s.to_string();
    }
    if s.chars().any(|c| c.is_control()) {
        let mut out = String::from("$'");
        for c in s.chars() {
            match c {
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                '\'' => out.push_str("\\'"),
                '\\' => out.push_str("\\\\"),
                c if c.is_control() => out.push_str(&format!("\\{:03o}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('\'');
        return out;
    }
    let mut out = String::new();
    for c in s.chars() {
        if !(c.is_ascii_alphanumeric() || "_-./:,@%+=".contains(c)) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(fmt: &str, args: &[&str]) -> String {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        format_all(fmt, &args, &mut Vec::new())
    }

    #[test]
    fn formats_like_printf() {
        assert_eq!(f("%s-%s\\n", &["a", "b", "c"]), "a-b\nc-\n");
        assert_eq!(f("%5s|%-5s|", &["ab", "cd"]), "   ab|cd   |");
        assert_eq!(f("%05d %x %X %o %#x", &["42", "255", "255", "8", "255"]), "00042 ff FF 10 0xff");
        assert_eq!(f("%.2f %e", &["3.14159", "12345.678"]), "3.14 1.234568e+04");
        assert_eq!(f("%g %g", &["0.0001", "100000"]), "0.0001 100000");
        assert_eq!(f("%d", &["'A"]), "65");
        assert_eq!(f("%b", &["a\\tb"]), "a\tb");
        assert_eq!(f("%q", &["a b"]), "a\\ b");
        assert_eq!(f("%%", &[]), "%");
        assert_eq!(f("%s", &[]), "");
        assert_eq!(f("\\101\\x42", &[]), "AB");
        assert_eq!(f("%.3s", &["abcdef"]), "abc");
        assert_eq!(f("%+d % d", &["5", "5"]), "+5  5");
    }
}
