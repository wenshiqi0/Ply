//! grep / egrep / fgrep.

use regex::Regex;
use wendy_io::EntryKind;
use wendy_libc::glob::Pattern;

use super::regex_util;
use super::{UtilCtx, lines, parse_args};

pub(crate) async fn egrep(ctx: UtilCtx<'_>) -> i32 {
    run(ctx, Some('E')).await
}

pub(crate) async fn fgrep(ctx: UtilCtx<'_>) -> i32 {
    run(ctx, Some('F')).await
}

pub(crate) async fn grep(ctx: UtilCtx<'_>) -> i32 {
    run(ctx, None).await
}

struct Opts {
    invert: bool,
    number: bool,
    count: bool,
    files_with: bool,
    files_without: bool,
    only: bool,
    quiet: bool,
    after: usize,
    before: usize,
    max: Option<usize>,
    with_name: bool,
}

async fn run(ctx: UtilCtx<'_>, forced: Option<char>) -> i32 {
    let a = match parse_args(
        ctx.args,
        "ivncClLhHrREFGoqswxaIUz",
        "efABCm\u{1}\u{2}\u{3}",
        &[
            ("ignore-case", 'i'), ("invert-match", 'v'), ("line-number", 'n'), ("count", 'c'),
            ("files-with-matches", 'l'), ("files-without-match", 'L'), ("no-filename", 'h'),
            ("with-filename", 'H'), ("recursive", 'r'), ("dereference-recursive", 'R'),
            ("extended-regexp", 'E'), ("fixed-strings", 'F'), ("basic-regexp", 'G'),
            ("only-matching", 'o'), ("quiet", 'q'), ("silent", 'q'), ("no-messages", 's'),
            ("word-regexp", 'w'), ("line-regexp", 'x'), ("regexp=", 'e'), ("file=", 'f'),
            ("after-context=", 'A'), ("before-context=", 'B'), ("context=", 'C'), ("max-count=", 'm'),
            ("include=", '\u{1}'), ("exclude=", '\u{2}'), ("exclude-dir=", '\u{3}'), ("color", 'U'),
            ("colour", 'U'), ("color=", 'z'), ("binary-files=", 'z'), ("text", 'a'),
        ],
    ) {
        Ok(a) => a,
        Err(e) => {
            ctx.error(&e);
            ctx.fds.err.write_str("Usage: grep [OPTION]... PATTERNS [FILE]...\n");
            return 2;
        }
    };

    let mut operands = a.operands.clone();
    let mut patterns: Vec<String> = a.values('e').into_iter().map(str::to_string).collect();
    for file in a.values('f') {
        match ctx.read_input(file).await {
            Ok(d) => patterns.extend(lines(&d).0),
            Err(e) => {
                ctx.error(&format!("{file}: {e}"));
                return 2;
            }
        }
    }
    if patterns.is_empty() {
        if operands.is_empty() {
            ctx.fds.err.write_str("Usage: grep [OPTION]... PATTERNS [FILE]...\n");
            return 2;
        }
        patterns = operands.remove(0).split('\n').map(str::to_string).collect();
    }
    let mode = forced.or(if a.has('F') { Some('F') } else if a.has('E') { Some('E') } else { None });
    let mut parts = Vec::new();
    for p in &patterns {
        let body = if mode == Some('F') {
            regex::escape(p)
        } else {
            match regex_util::translate(p, mode == Some('E')) {
                Ok(b) => b,
                Err(e) => {
                    ctx.error(&e);
                    return 2;
                }
            }
        };
        let body = if a.has('x') {
            format!("^(?:{body})$")
        } else if a.has('w') {
            format!("\\b(?:{body})\\b")
        } else {
            format!("(?:{body})")
        };
        parts.push(body);
    }
    let re = match regex::RegexBuilder::new(&parts.join("|")).case_insensitive(a.has('i')).build() {
        Ok(re) => re,
        Err(e) => {
            ctx.error(&format!("invalid regular expression: {}", e.to_string().lines().last().unwrap_or("").trim()));
            return 2;
        }
    };

    let recursive = a.has('r') || a.has('R');
    if operands.is_empty() {
        operands.push(if recursive { ".".into() } else { "-".into() });
    }
    let context = a.value('C').and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut opts = Opts {
        invert: a.has('v'),
        number: a.has('n'),
        count: a.has('c'),
        files_with: a.has('l'),
        files_without: a.has('L'),
        only: a.has('o'),
        quiet: a.has('q'),
        after: a.value('A').and_then(|v| v.parse().ok()).unwrap_or(context),
        before: a.value('B').and_then(|v| v.parse().ok()).unwrap_or(context),
        max: a.value('m').and_then(|v| v.parse().ok()),
        with_name: false,
    };
    let includes: Vec<Pattern> = a.values('\u{1}').into_iter().map(Pattern::new).collect();
    let excludes: Vec<Pattern> = a.values('\u{2}').into_iter().map(Pattern::new).collect();
    let exclude_dirs: Vec<Pattern> = a.values('\u{3}').into_iter().map(Pattern::new).collect();

    // Collect (shown name, contents) for every input.
    let mut inputs: Vec<(String, Vec<u8>)> = Vec::new();
    let mut errors = false;
    for op in &operands {
        if op == "-" {
            inputs.push(("(standard input)".into(), ctx.stdin()));
            continue;
        }
        match ctx.fs.stat(op).await {
            Ok(entry) if entry.kind == EntryKind::Dir => {
                if !recursive {
                    if !a.has('s') {
                        ctx.error(&format!("{op}: Is a directory"));
                    }
                    errors = true;
                    continue;
                }
                let walked = match ctx.fs.walk(op).await {
                    Ok(w) => w,
                    Err(e) => {
                        if !a.has('s') {
                            ctx.error(&format!("{op}: {e}"));
                        }
                        errors = true;
                        continue;
                    }
                };
                let root = ctx.fs.resolve(op);
                let mut skipped_dirs: Vec<String> = Vec::new();
                for (abs, entry) in walked {
                    if skipped_dirs.iter().any(|d| abs.starts_with(&format!("{d}/"))) {
                        continue;
                    }
                    let base = wendy_libc::path::basename(&abs);
                    if entry.kind == EntryKind::Dir {
                        if abs != root && exclude_dirs.iter().any(|p| p.matches(&base)) {
                            skipped_dirs.push(abs);
                        }
                        continue;
                    }
                    if !includes.is_empty() && !includes.iter().any(|p| p.matches(&base)) {
                        continue;
                    }
                    if excludes.iter().any(|p| p.matches(&base)) {
                        continue;
                    }
                    let rel = &abs[root.len()..];
                    let shown = if op == "." && !a.operands.iter().any(|o| o == ".") {
                        rel.trim_start_matches('/').to_string()
                    } else if root == "/" {
                        abs.clone()
                    } else {
                        format!("{}{rel}", op.trim_end_matches('/'))
                    };
                    match ctx.fs.read(&abs).await {
                        Ok(data) => inputs.push((shown, data)),
                        Err(e) => {
                            if !a.has('s') {
                                ctx.error(&format!("{shown}: {e}"));
                            }
                            errors = true;
                        }
                    }
                }
            }
            Ok(_) => match ctx.fs.read(op).await {
                Ok(data) => inputs.push((op.clone(), data)),
                Err(e) => {
                    if !a.has('s') {
                        ctx.error(&format!("{op}: {e}"));
                    }
                    errors = true;
                }
            },
            Err(e) => {
                if !a.has('s') {
                    ctx.error(&format!("{op}: {e}"));
                }
                errors = true;
            }
        }
    }
    opts.with_name = if a.has('h') { false } else { a.has('H') || recursive || operands.len() > 1 };

    let mut matched_any = false;
    for (name, data) in &inputs {
        let found = search(&ctx, &re, name, data, &opts);
        matched_any |= found;
        if found && opts.quiet {
            return 0;
        }
    }
    if errors && !(opts.quiet && matched_any) {
        2
    } else if matched_any {
        0
    } else {
        1
    }
}

/// Search one input; returns whether anything matched.
fn search(ctx: &UtilCtx<'_>, re: &Regex, name: &str, data: &[u8], o: &Opts) -> bool {
    let (all, _) = lines(data);
    let is_match = |line: &str| re.is_match(line) != o.invert;
    let prefix = |sep: char, n: usize| -> String {
        let mut p = String::new();
        if o.with_name {
            p.push_str(name);
            p.push(sep);
        }
        if o.number {
            p.push_str(&n.to_string());
            p.push(sep);
        }
        p
    };

    let mut count = 0usize;
    let mut out = String::new();
    let mut last_printed: Option<usize> = None;
    let mut after_left = 0usize;
    for (i, line) in all.iter().enumerate() {
        if o.max.is_some_and(|m| count >= m) {
            // GNU still prints trailing context after the last allowed match.
            if after_left > 0 && !(o.count || o.files_with || o.files_without || o.quiet) {
                out.push_str(&format!("{}{line}\n", prefix('-', i + 1)));
                last_printed = Some(i);
                after_left -= 1;
                continue;
            }
            break;
        }
        if is_match(line) {
            count += 1;
            if o.count || o.files_with || o.files_without || o.quiet {
                continue;
            }
            // Leading context, with a `--` between disjoint groups.
            let from = i.saturating_sub(o.before);
            let from = last_printed.map_or(from, |l| from.max(l + 1));
            if (o.before > 0 || o.after > 0) && last_printed.is_some_and(|l| from > l + 1) {
                out.push_str("--\n");
            }
            for (j, ctx_line) in all.iter().enumerate().take(i).skip(from) {
                out.push_str(&format!("{}{ctx_line}\n", prefix('-', j + 1)));
            }
            if o.only && !o.invert {
                for m in re.find_iter(line).filter(|m| !m.as_str().is_empty()) {
                    out.push_str(&format!("{}{}\n", prefix(':', i + 1), m.as_str()));
                }
            } else {
                out.push_str(&format!("{}{line}\n", prefix(':', i + 1)));
            }
            last_printed = Some(i);
            after_left = o.after;
        } else if after_left > 0 && !(o.count || o.files_with || o.files_without || o.quiet) {
            out.push_str(&format!("{}{line}\n", prefix('-', i + 1)));
            last_printed = Some(i);
            after_left -= 1;
        }
    }
    if o.quiet {
        return count > 0;
    }
    if o.files_with {
        if count > 0 {
            ctx.out(&format!("{name}\n"));
        }
    } else if o.files_without {
        if count == 0 {
            ctx.out(&format!("{name}\n"));
        }
    } else if o.count {
        if o.with_name {
            ctx.out(&format!("{name}:{count}\n"));
        } else {
            ctx.out(&format!("{count}\n"));
        }
    } else {
        ctx.out(&out);
    }
    count > 0
}
