//! Text utilities: head tail wc sort uniq cut tr tee rev tac nl paste seq.

use std::cmp::Ordering;

use super::{UtilCtx, lines, parse_args};

macro_rules! args_or_exit {
    ($ctx:expr, $args:expr, $known:expr, $vals:expr, $long:expr) => {
        match parse_args($args, $known, $vals, $long) {
            Ok(a) => a,
            Err(e) => {
                $ctx.error(&e);
                return 2;
            }
        }
    };
}

/// `head -5` / `tail -5` → `-n 5`; `tail +5` → `-n +5`.
fn legacy_count(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if i == 0 && a.len() > 1 && (a.starts_with('-') || a.starts_with('+')) && a[1..].chars().all(|c| c.is_ascii_digit()) {
            out.push("-n".into());
            out.push(if a.starts_with('+') { a.clone() } else { a[1..].to_string() });
        } else {
            out.push(a.clone());
        }
    }
    out
}

async fn inputs(ctx: &UtilCtx<'_>, operands: &[String]) -> Vec<(String, Result<Vec<u8>, wendy_libc::SysError>)> {
    let operands = if operands.is_empty() { vec!["-".to_string()] } else { operands.to_vec() };
    let mut out = Vec::new();
    for op in operands {
        let data = ctx.read_input(&op).await;
        out.push((op, data));
    }
    out
}

pub(crate) async fn head(ctx: UtilCtx<'_>) -> i32 {
    let args = legacy_count(ctx.args);
    let a = args_or_exit!(ctx, &args, "qv", "nc", &[("lines=", 'n'), ("bytes=", 'c'), ("quiet", 'q'), ("verbose", 'v')]);
    let files = inputs(&ctx, &a.operands).await;
    let headers = (files.len() > 1 || a.has('v')) && !a.has('q');
    let mut status = 0;
    for (i, (name, data)) in files.into_iter().enumerate() {
        let data = match data {
            Ok(d) => d,
            Err(e) => {
                ctx.error(&format!("cannot open '{name}' for reading: {e}"));
                status = 1;
                continue;
            }
        };
        if headers {
            ctx.out(&format!("{}==> {} <==\n", if i > 0 { "\n" } else { "" }, if name == "-" { "standard input" } else { &name }));
        }
        if let Some(c) = a.value('c') {
            let n: i64 = c.parse().unwrap_or(0);
            let end = if n < 0 { data.len().saturating_sub((-n) as usize) } else { (n as usize).min(data.len()) };
            ctx.out_bytes(&data[..end]);
            continue;
        }
        let n: i64 = a.value('n').and_then(|n| n.parse().ok()).unwrap_or(10);
        let pieces: Vec<&[u8]> = data.split_inclusive(|b| *b == b'\n').collect();
        let take = if n < 0 { pieces.len().saturating_sub((-n) as usize) } else { (n as usize).min(pieces.len()) };
        for p in &pieces[..take] {
            ctx.out_bytes(p);
        }
    }
    status
}

pub(crate) async fn tail(ctx: UtilCtx<'_>) -> i32 {
    let args = legacy_count(ctx.args);
    let a = args_or_exit!(ctx, &args, "qvfF", "nc", &[("lines=", 'n'), ("bytes=", 'c'), ("quiet", 'q'), ("follow", 'f')]);
    let files = inputs(&ctx, &a.operands).await;
    let headers = (files.len() > 1 || a.has('v')) && !a.has('q');
    let mut status = 0;
    for (i, (name, data)) in files.into_iter().enumerate() {
        let data = match data {
            Ok(d) => d,
            Err(e) => {
                ctx.error(&format!("cannot open '{name}' for reading: {e}"));
                status = 1;
                continue;
            }
        };
        if headers {
            ctx.out(&format!("{}==> {} <==\n", if i > 0 { "\n" } else { "" }, if name == "-" { "standard input" } else { &name }));
        }
        if let Some(c) = a.value('c') {
            let start = match c.strip_prefix('+') {
                Some(from) => from.parse::<usize>().unwrap_or(1).saturating_sub(1).min(data.len()),
                None => data.len().saturating_sub(c.trim_start_matches('-').parse().unwrap_or(0)),
            };
            ctx.out_bytes(&data[start..]);
            continue;
        }
        let spec = a.value('n').unwrap_or("10");
        let pieces: Vec<&[u8]> = data.split_inclusive(|b| *b == b'\n').collect();
        let start = match spec.strip_prefix('+') {
            Some(from) => from.parse::<usize>().unwrap_or(1).saturating_sub(1).min(pieces.len()),
            None => pieces.len().saturating_sub(spec.trim_start_matches('-').parse().unwrap_or(10)),
        };
        for p in &pieces[start..] {
            ctx.out_bytes(p);
        }
    }
    status
}

pub(crate) async fn wc(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, ctx.args, "lwcmL", "", &[("lines", 'l'), ("words", 'w'), ("bytes", 'c'), ("chars", 'm'), ("max-line-length", 'L')]);
    let explicit = a.has('l') || a.has('w') || a.has('c') || a.has('m') || a.has('L');
    let (show_l, show_w, show_c) = if explicit { (a.has('l'), a.has('w'), a.has('c')) } else { (true, true, true) };
    let (show_m, show_max) = (a.has('m'), a.has('L'));
    let from_stdin = a.operands.is_empty();
    let files = inputs(&ctx, &a.operands).await;
    let mut rows = Vec::new();
    let mut totals = [0u64; 5];
    let mut status = 0;
    let mut regular_bytes = 0u64;
    let mut any_stdin = false;
    for (name, data) in files {
        let data = match data {
            Ok(d) => d,
            Err(e) => {
                ctx.error(&format!("{name}: {e}"));
                status = 1;
                continue;
            }
        };
        if name == "-" {
            any_stdin = true;
        } else {
            regular_bytes += data.len() as u64;
        }
        let text = String::from_utf8_lossy(&data);
        let counts = [
            data.iter().filter(|b| **b == b'\n').count() as u64,
            text.split_whitespace().count() as u64,
            text.chars().count() as u64,
            data.len() as u64,
            text.lines().map(|l| l.chars().count() as u64).max().unwrap_or(0),
        ];
        for (t, c) in totals.iter_mut().zip(counts) {
            *t += c;
        }
        rows.push((if name == "-" && from_stdin { None } else { Some(name) }, counts));
    }
    let selected = |c: &[u64; 5]| -> Vec<u64> {
        let mut v = Vec::new();
        if show_l {
            v.push(c[0]);
        }
        if show_w {
            v.push(c[1]);
        }
        if show_m {
            v.push(c[2]);
        }
        if show_c {
            v.push(c[3]);
        }
        if show_max {
            v.push(c[4]);
        }
        v
    };
    let columns = selected(&totals).len();
    // GNU: one number for one input needs no padding; otherwise the width
    // fits the total byte count, and is at least 7 when stdin is involved.
    let width = if columns == 1 && rows.len() == 1 {
        1
    } else {
        let w = regular_bytes.max(totals[3]).to_string().len();
        if any_stdin { w.max(7) } else { w }
    };
    let render = |vals: Vec<u64>, name: Option<&str>| -> String {
        let nums: Vec<String> = vals.iter().map(|v| format!("{v:>width$}")).collect();
        match name {
            Some(n) => format!("{} {n}\n", nums.join(" ")),
            None => format!("{}\n", nums.join(" ")),
        }
    };
    let many = rows.len() > 1;
    for (name, counts) in rows {
        ctx.out(&render(selected(&counts), name.as_deref()));
    }
    if many {
        ctx.out(&render(selected(&totals), Some("total")));
    }
    status
}

#[derive(Clone)]
struct SortKey {
    start_field: usize,
    start_char: usize,
    end_field: Option<usize>,
    numeric: bool,
    reverse: bool,
    fold: bool,
    human: bool,
}

fn parse_key(spec: &str) -> Option<SortKey> {
    let (start, end) = match spec.split_once(',') {
        Some((s, e)) => (s, Some(e)),
        None => (spec, None),
    };
    let mut key = SortKey { start_field: 1, start_char: 0, end_field: None, numeric: false, reverse: false, fold: false, human: false };
    let split_opts = |s: &str| -> (String, String) {
        let digits: String = s.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let opts: String = s[digits.len()..].to_string();
        (digits, opts)
    };
    let (sd, so) = split_opts(start);
    let (field, ch) = match sd.split_once('.') {
        Some((f, c)) => (f, c.parse().unwrap_or(1)),
        None => (sd.as_str(), 1),
    };
    key.start_field = field.parse().ok()?;
    key.start_char = ch.max(1) - 1;
    let mut opts = so;
    if let Some(e) = end {
        let (ed, eo) = split_opts(e);
        key.end_field = ed.split('.').next().and_then(|f| f.parse().ok());
        opts.push_str(&eo);
    }
    for c in opts.chars() {
        match c {
            'n' => key.numeric = true,
            'r' => key.reverse = true,
            'f' => key.fold = true,
            'h' => key.human = true,
            'b' | 'g' | 'd' | 'i' | 'M' | 'V' => {}
            _ => return None,
        }
    }
    Some(key)
}

fn fields_of(line: &str, sep: Option<char>) -> Vec<(usize, usize)> {
    // Byte ranges of fields; with no separator, a field is leading blanks + non-blanks.
    let mut out = Vec::new();
    match sep {
        Some(sep) => {
            let mut start = 0;
            for (i, c) in line.char_indices() {
                if c == sep {
                    out.push((start, i));
                    start = i + c.len_utf8();
                }
            }
            out.push((start, line.len()));
        }
        None => {
            let bytes = line.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                let start = i;
                while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                    i += 1;
                }
                while i < bytes.len() && bytes[i] != b' ' && bytes[i] != b'\t' {
                    i += 1;
                }
                out.push((start, i));
            }
        }
    }
    out
}

fn key_text<'a>(line: &'a str, key: &SortKey, sep: Option<char>) -> &'a str {
    let fields = fields_of(line, sep);
    let Some(&(fs, fe)) = fields.get(key.start_field - 1) else { return "" };
    let start = (fs + key.start_char).min(fe);
    let end = match key.end_field {
        Some(e) => fields.get(e - 1).map_or(line.len(), |f| f.1),
        None => line.len(),
    };
    if start >= end { "" } else { &line[start..end] }
}

fn numeric_prefix(s: &str) -> f64 {
    let t = s.trim_start();
    let end = t
        .char_indices()
        .take_while(|(i, c)| c.is_ascii_digit() || *c == '.' || (*i == 0 && (*c == '-' || *c == '+')))
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0);
    t[..end].parse().unwrap_or(0.0)
}

fn human_value(s: &str) -> f64 {
    let t = s.trim();
    let n = numeric_prefix(t);
    let suffix = t.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '-' || c == '+').chars().next();
    let mult = match suffix.map(|c| c.to_ascii_uppercase()) {
        Some('K') => 1e3,
        Some('M') => 1e6,
        Some('G') => 1e9,
        Some('T') => 1e12,
        Some('P') => 1e15,
        _ => 1.0,
    };
    n * mult
}

fn compare(a: &str, b: &str, numeric: bool, human: bool, fold: bool) -> Ordering {
    if human {
        return human_value(a).partial_cmp(&human_value(b)).unwrap_or(Ordering::Equal);
    }
    if numeric {
        return numeric_prefix(a).partial_cmp(&numeric_prefix(b)).unwrap_or(Ordering::Equal);
    }
    if fold {
        return a.to_uppercase().cmp(&b.to_uppercase());
    }
    a.cmp(b)
}

pub(crate) async fn sort(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(
        ctx,
        ctx.args,
        "rnufbshcVdgiMR",
        "ktTo",
        &[("reverse", 'r'), ("numeric-sort", 'n'), ("unique", 'u'), ("ignore-case", 'f'), ("key=", 'k'), ("field-separator=", 't'), ("human-numeric-sort", 'h'), ("stable", 's'), ("output=", 'o')]
    );
    let sep = a.value('t').and_then(|s| s.chars().next());
    let mut keys = Vec::new();
    for spec in a.values('k') {
        match parse_key(spec) {
            Some(k) => keys.push(k),
            None => {
                ctx.error(&format!("invalid key specification '{spec}'"));
                return 2;
            }
        }
    }
    let mut all = Vec::new();
    for (name, data) in inputs(&ctx, &a.operands).await {
        match data {
            Ok(d) => all.extend(lines(&d).0),
            Err(e) => {
                ctx.error(&format!("cannot read: {name}: {e}"));
                return 2;
            }
        }
    }
    let (numeric, human, fold, reverse) = (a.has('n'), a.has('h'), a.has('f'), a.has('r'));
    let cmp = |x: &String, y: &String| -> Ordering {
        for key in &keys {
            let (kx, ky) = (key_text(x, key, sep), key_text(y, key, sep));
            let any_type = key.numeric || key.human || key.fold;
            let ord = if any_type {
                compare(kx, ky, key.numeric, key.human, key.fold)
            } else {
                compare(kx, ky, numeric, human, fold)
            };
            let ord = if key.reverse { ord.reverse() } else { ord };
            if ord != Ordering::Equal {
                return if reverse && !key.reverse { ord.reverse() } else { ord };
            }
        }
        let ord = if keys.is_empty() { compare(x, y, numeric, human, fold) } else { Ordering::Equal };
        let ord = if ord == Ordering::Equal && !a.has('s') && !a.has('u') { x.cmp(y) } else { ord };
        if reverse { ord.reverse() } else { ord }
    };
    all.sort_by(cmp);
    if a.has('u') {
        all.dedup_by(|y, x| cmp(x, y) == Ordering::Equal);
    }
    if a.has('c') {
        return if all.windows(2).all(|w| cmp(&w[0], &w[1]) != Ordering::Greater) { 0 } else { 1 };
    }
    let mut out = String::new();
    for line in &all {
        out.push_str(line);
        out.push('\n');
    }
    match a.value('o') {
        Some(path) => match ctx.fs.write(path, out.as_bytes()).await {
            Ok(()) => 0,
            Err(e) => {
                ctx.error(&format!("cannot create {path}: {e}"));
                2
            }
        },
        None => {
            ctx.out(&out);
            0
        }
    }
}

pub(crate) async fn uniq(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, ctx.args, "cduiz", "fs", &[("count", 'c'), ("repeated", 'd'), ("unique", 'u'), ("ignore-case", 'i'), ("skip-fields=", 'f'), ("skip-chars=", 's')]);
    let input = a.operands.first().cloned().unwrap_or_else(|| "-".into());
    let data = match ctx.read_input(&input).await {
        Ok(d) => d,
        Err(e) => {
            ctx.error(&format!("{input}: {e}"));
            return 1;
        }
    };
    let skip_fields: usize = a.value('f').and_then(|v| v.parse().ok()).unwrap_or(0);
    let skip_chars: usize = a.value('s').and_then(|v| v.parse().ok()).unwrap_or(0);
    let key = |line: &str| -> String {
        let mut rest = line;
        for _ in 0..skip_fields {
            rest = rest.trim_start_matches([' ', '\t']);
            rest = rest.trim_start_matches(|c: char| c != ' ' && c != '\t');
        }
        let k: String = rest.chars().skip(skip_chars).collect();
        if a.has('i') { k.to_lowercase() } else { k }
    };
    let (all, _) = lines(&data);
    let mut groups: Vec<(String, usize)> = Vec::new();
    for line in all {
        match groups.last_mut() {
            Some((prev, n)) if key(prev) == key(&line) => *n += 1,
            _ => groups.push((line, 1)),
        }
    }
    let mut out = String::new();
    for (line, n) in groups {
        if (a.has('d') && n < 2) || (a.has('u') && n > 1) {
            continue;
        }
        if a.has('c') {
            out.push_str(&format!("{n:>7} {line}\n"));
        } else {
            out.push_str(&format!("{line}\n"));
        }
    }
    match a.operands.get(1) {
        Some(path) => match ctx.fs.write(path, out.as_bytes()).await {
            Ok(()) => 0,
            Err(e) => {
                ctx.error(&format!("{path}: {e}"));
                1
            }
        },
        None => {
            ctx.out(&out);
            0
        }
    }
}

/// `1,3-5,7-` style list → membership test (1-based).
fn parse_list(spec: &str) -> Option<Vec<(usize, usize)>> {
    let mut ranges = Vec::new();
    for part in spec.split(',') {
        let (lo, hi) = match part.split_once('-') {
            Some(("", hi)) => (1, hi.parse().ok()?),
            Some((lo, "")) => (lo.parse().ok()?, usize::MAX),
            Some((lo, hi)) => (lo.parse().ok()?, hi.parse().ok()?),
            None => {
                let n = part.parse().ok()?;
                (n, n)
            }
        };
        if lo == 0 {
            return None;
        }
        ranges.push((lo, hi));
    }
    Some(ranges)
}

pub(crate) async fn cut(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, ctx.args, "sn", "dfcb", &[("delimiter=", 'd'), ("fields=", 'f'), ("characters=", 'c'), ("bytes=", 'b'), ("only-delimited", 's'), ("complement", '\0'), ("output-delimiter=", 'O')]);
    let (list, mode) = match (a.value('f'), a.value('c').or(a.value('b'))) {
        (Some(f), _) => (f, 'f'),
        (None, Some(c)) => (c, 'c'),
        _ => {
            ctx.error("you must specify a list of bytes, characters, or fields");
            return 1;
        }
    };
    let Some(ranges) = parse_list(list) else {
        ctx.error(&format!("invalid field value '{list}'"));
        return 1;
    };
    let wanted = |i: usize| ranges.iter().any(|(lo, hi)| i >= *lo && i <= *hi);
    let delim = a.value('d').and_then(|d| d.chars().next()).unwrap_or('\t');
    let mut status = 0;
    for (name, data) in inputs(&ctx, &a.operands).await {
        let data = match data {
            Ok(d) => d,
            Err(e) => {
                ctx.error(&format!("{name}: {e}"));
                status = 1;
                continue;
            }
        };
        let mut out = String::new();
        for line in lines(&data).0 {
            if mode == 'c' {
                let picked: String = line.chars().enumerate().filter(|(i, _)| wanted(i + 1)).map(|(_, c)| c).collect();
                out.push_str(&picked);
                out.push('\n');
                continue;
            }
            if !line.contains(delim) {
                if !a.has('s') {
                    out.push_str(&line);
                    out.push('\n');
                }
                continue;
            }
            let picked: Vec<&str> = line.split(delim).enumerate().filter(|(i, _)| wanted(i + 1)).map(|(_, f)| f).collect();
            out.push_str(&picked.join(&delim.to_string()));
            out.push('\n');
        }
        ctx.out(&out);
    }
    status
}

/// Expand a `tr` set: ranges, classes, escapes.
fn tr_set(spec: &str) -> Vec<char> {
    let mut out = Vec::new();
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let unescape = |chars: &[char], i: &mut usize| -> char {
        if chars[*i] == '\\' && *i + 1 < chars.len() {
            *i += 1;
            match chars[*i] {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '\\' => '\\',
                c => c,
            }
        } else {
            chars[*i]
        }
    };
    while i < chars.len() {
        if chars[i] == '[' && chars.get(i + 1) == Some(&':') {
            let rest: String = chars[i + 2..].iter().collect();
            if let Some(end) = rest.find(":]") {
                let class = &rest[..end];
                let members: Vec<char> = (0u8..=127).map(char::from).filter(|c| match class {
                    "upper" => c.is_ascii_uppercase(),
                    "lower" => c.is_ascii_lowercase(),
                    "digit" => c.is_ascii_digit(),
                    "alpha" => c.is_ascii_alphabetic(),
                    "alnum" => c.is_ascii_alphanumeric(),
                    "space" => c.is_ascii_whitespace() || *c == '\x0b',
                    "blank" => *c == ' ' || *c == '\t',
                    "punct" => c.is_ascii_punctuation(),
                    "xdigit" => c.is_ascii_hexdigit(),
                    "cntrl" => c.is_ascii_control(),
                    "print" => !c.is_ascii_control(),
                    _ => false,
                }).collect();
                out.extend(members);
                i += 2 + end + 2;
                continue;
            }
        }
        let first = unescape(&chars, &mut i);
        if chars.get(i + 1) == Some(&'-') && i + 2 < chars.len() {
            let mut j = i + 2;
            let last = unescape(&chars, &mut j);
            if first <= last {
                out.extend((first as u32..=last as u32).filter_map(char::from_u32));
                i = j + 1;
                continue;
            }
        }
        out.push(first);
        i += 1;
    }
    out
}

pub(crate) async fn tr(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, ctx.args, "dscCt", "", &[("delete", 'd'), ("squeeze-repeats", 's'), ("complement", 'c')]);
    let Some(set1) = a.operands.first() else {
        ctx.error("missing operand");
        return 1;
    };
    let set1 = tr_set(set1);
    let set2 = a.operands.get(1).map(|s| tr_set(s)).unwrap_or_default();
    let complement = a.has('c') || a.has('C');
    let in_set1 = |c: char| set1.contains(&c) != complement;
    let input = String::from_utf8_lossy(&ctx.stdin()).into_owned();
    let mut out = String::with_capacity(input.len());
    let squeeze_set: &[char] = if a.has('d') || set2.is_empty() { &set1 } else { &set2 };
    let mut last: Option<char> = None;
    for c in input.chars() {
        let mapped = if a.has('d') {
            if in_set1(c) {
                continue;
            }
            c
        } else if !set2.is_empty() && in_set1(c) {
            if complement {
                *set2.last().unwrap_or(&c)
            } else {
                let idx = set1.iter().position(|x| *x == c).unwrap_or(0);
                *set2.get(idx).or(set2.last()).unwrap_or(&c)
            }
        } else {
            c
        };
        if a.has('s') && last == Some(mapped) && squeeze_set.contains(&mapped) {
            continue;
        }
        last = Some(mapped);
        out.push(mapped);
    }
    ctx.out(&out);
    0
}

pub(crate) async fn tee(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, ctx.args, "ai", "", &[("append", 'a'), ("ignore-interrupts", 'i')]);
    let data = ctx.stdin();
    let mut status = 0;
    for file in &a.operands {
        let result = if a.has('a') { ctx.fs.append(file, &data).await } else { ctx.fs.write(file, &data).await };
        if let Err(e) = result {
            ctx.error(&format!("{file}: {e}"));
            status = 1;
        }
    }
    ctx.out_bytes(&data);
    status
}

pub(crate) async fn rev(ctx: UtilCtx<'_>) -> i32 {
    let mut status = 0;
    for (name, data) in inputs(&ctx, ctx.args).await {
        match data {
            Ok(d) => {
                let (all, trailing) = lines(&d);
                let n = all.len();
                for (i, line) in all.into_iter().enumerate() {
                    ctx.out(&line.chars().rev().collect::<String>());
                    if i + 1 < n || trailing {
                        ctx.out("\n");
                    }
                }
            }
            Err(e) => {
                ctx.error(&format!("cannot open {name}: {e}"));
                status = 1;
            }
        }
    }
    status
}

pub(crate) async fn tac(ctx: UtilCtx<'_>) -> i32 {
    let mut status = 0;
    for (name, data) in inputs(&ctx, ctx.args).await {
        match data {
            Ok(d) => {
                for line in lines(&d).0.into_iter().rev() {
                    ctx.out(&format!("{line}\n"));
                }
            }
            Err(e) => {
                ctx.error(&format!("failed to open '{name}' for reading: {e}"));
                status = 1;
            }
        }
    }
    status
}

pub(crate) async fn nl(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, ctx.args, "", "bwsv", &[("body-numbering=", 'b'), ("number-width=", 'w'), ("number-separator=", 's')]);
    let all_lines = a.value('b') == Some("a");
    let width: usize = a.value('w').and_then(|w| w.parse().ok()).unwrap_or(6);
    let sep = a.value('s').unwrap_or("\t").to_string();
    let mut n: i64 = a.value('v').and_then(|v| v.parse().ok()).unwrap_or(1);
    for (name, data) in inputs(&ctx, &a.operands).await {
        let Ok(d) = data else {
            ctx.error(&format!("{name}: No such file or directory"));
            return 1;
        };
        for line in lines(&d).0 {
            if line.is_empty() && !all_lines {
                ctx.out("\n");
            } else {
                ctx.out(&format!("{n:>width$}{sep}{line}\n"));
                n += 1;
            }
        }
    }
    0
}

pub(crate) async fn paste(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, ctx.args, "s", "d", &[("delimiters=", 'd'), ("serial", 's')]);
    let delims: Vec<char> = a.value('d').map(|d| d.replace("\\t", "\t").replace("\\n", "\n").chars().collect()).unwrap_or_else(|| vec!['\t']);
    let delim = |i: usize| delims.get(i % delims.len().max(1)).copied().unwrap_or('\t');
    let mut columns = Vec::new();
    for (name, data) in inputs(&ctx, &a.operands).await {
        match data {
            Ok(d) => columns.push(lines(&d).0),
            Err(e) => {
                ctx.error(&format!("{name}: {e}"));
                return 1;
            }
        }
    }
    let mut out = String::new();
    if a.has('s') {
        for col in columns {
            for (i, line) in col.iter().enumerate() {
                if i > 0 {
                    out.push(delim(i - 1));
                }
                out.push_str(line);
            }
            out.push('\n');
        }
    } else {
        let rows = columns.iter().map(Vec::len).max().unwrap_or(0);
        for r in 0..rows {
            for (i, col) in columns.iter().enumerate() {
                if i > 0 {
                    out.push(delim(i - 1));
                }
                out.push_str(col.get(r).map(String::as_str).unwrap_or(""));
            }
            out.push('\n');
        }
    }
    ctx.out(&out);
    0
}

pub(crate) async fn seq(ctx: UtilCtx<'_>) -> i32 {
    // Negative numbers are operands, not options.
    let mut sep = "\n".to_string();
    let mut width = false;
    let mut nums = Vec::new();
    let mut i = 0;
    while i < ctx.args.len() {
        let arg = &ctx.args[i];
        match arg.as_str() {
            "-s" => {
                i += 1;
                sep = ctx.args.get(i).cloned().unwrap_or_default();
            }
            "-w" => width = true,
            s if s.starts_with("-s") => sep = s[2..].to_string(),
            s => nums.push(s.to_string()),
        }
        i += 1;
    }
    let parse = |s: &str| s.parse::<f64>().ok();
    let (first, step, last) = match nums.as_slice() {
        [l] => (Some(1.0), Some(1.0), parse(l)),
        [f, l] => (parse(f), Some(1.0), parse(l)),
        [f, s, l] => (parse(f), parse(s), parse(l)),
        _ => {
            ctx.error("missing operand");
            return 1;
        }
    };
    let (Some(first), Some(step), Some(last)) = (first, step, last) else {
        ctx.error(&format!("invalid floating point argument: '{}'", nums.join(" ")));
        return 1;
    };
    if step == 0.0 {
        ctx.error("invalid Zero increment value: '0'");
        return 1;
    }
    let decimals = nums.iter().map(|n| n.split_once('.').map_or(0, |(_, d)| d.len())).max().unwrap_or(0);
    let fmt = |v: f64| if decimals == 0 { format!("{}", v.round() as i64) } else { format!("{v:.decimals$}") };
    let pad = if width { fmt(first).len().max(fmt(last).len()) } else { 0 };
    let mut values = Vec::new();
    let mut v = first;
    let mut count = 0;
    while (step > 0.0 && v <= last + 1e-9) || (step < 0.0 && v >= last - 1e-9) {
        let s = fmt(v);
        values.push(if width { format!("{s:0>pad$}") } else { s });
        count += 1;
        v = first + step * count as f64;
        if count > 10_000_000 {
            break;
        }
    }
    if !values.is_empty() {
        ctx.out(&format!("{}\n", values.join(&sep)));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tr_sets_and_cut_lists() {
        assert_eq!(tr_set("a-e"), vec!['a', 'b', 'c', 'd', 'e']);
        assert_eq!(tr_set("[:digit:]").len(), 10);
        assert_eq!(tr_set("\\n"), vec!['\n']);
        assert_eq!(parse_list("1,3-4,6-").unwrap(), vec![(1, 1), (3, 4), (6, usize::MAX)]);
        assert!(parse_list("0").is_none());
    }

    #[test]
    fn sort_keys_pick_fields() {
        let key = parse_key("2,2n").unwrap();
        assert_eq!(key_text("a 10 x", &key, None).trim(), "10");
        let key = parse_key("2").unwrap();
        assert_eq!(key_text("a:b:c", &key, Some(':')), "b:c");
    }
}
