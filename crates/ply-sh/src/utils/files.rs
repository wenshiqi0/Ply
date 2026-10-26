//! File utilities: cat ls mkdir rmdir rm cp mv touch realpath readlink
//! basename dirname stat du. Output and messages follow GNU coreutils in the
//! C locale.

use std::time::SystemTime;

use ply_io::{Entry, EntryKind};
use ply_libc::{Errno, path, time};

use super::{UtilCtx, parse_args};

macro_rules! args_or_exit {
    ($ctx:expr, $known:expr, $vals:expr, $long:expr) => {
        match parse_args($ctx.args, $known, $vals, $long) {
            Ok(a) => a,
            Err(e) => {
                $ctx.error(&e);
                return 2;
            }
        }
    };
}

pub(crate) async fn cat(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, "nbsEAuvT", "", &[("number", 'n'), ("show-ends", 'E')]);
    let operands = if a.operands.is_empty() { vec!["-".to_string()] } else { a.operands.clone() };
    let mut status = 0;
    let mut line_no = 1;
    for op in &operands {
        match ctx.read_input(op).await {
            Ok(data) => {
                if a.has('n') || a.has('b') || a.has('E') || a.has('A') {
                    let text = String::from_utf8_lossy(&data);
                    let mut out = String::new();
                    for piece in text.split_inclusive('\n') {
                        let (body, nl) = match piece.strip_suffix('\n') {
                            Some(b) => (b, true),
                            None => (piece, false),
                        };
                        let numbered = a.has('n') && !(a.has('b') && body.is_empty()) || (a.has('b') && !body.is_empty());
                        if numbered {
                            out.push_str(&format!("{line_no:>6}\t"));
                            line_no += 1;
                        }
                        out.push_str(body);
                        if nl {
                            if a.has('E') || a.has('A') {
                                out.push('$');
                            }
                            out.push('\n');
                        }
                    }
                    ctx.out(&out);
                } else {
                    ctx.out_bytes(&data);
                }
            }
            Err(e) => {
                ctx.error(&format!("{op}: {e}"));
                status = 1;
            }
        }
    }
    status
}

struct Listed {
    name: String,
    entry: Entry,
}

pub(crate) async fn ls(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(
        ctx,
        "aAl1dhrtSRFpC\0",
        "",
        &[("all", 'a'), ("almost-all", 'A'), ("human-readable", 'h'), ("reverse", 'r'), ("recursive", 'R'), ("color=", '\0'), ("classify", 'F'), ("directory", 'd')]
    );
    let operands = if a.operands.is_empty() { vec![".".to_string()] } else { a.operands.clone() };
    let mut status = 0;
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for op in &operands {
        match ctx.fs.stat(op).await {
            Ok(entry) if entry.kind == EntryKind::Dir && !a.has('d') => dirs.push(op.clone()),
            Ok(entry) => files.push(Listed { name: op.clone(), entry }),
            Err(e) => {
                ctx.error(&format!("cannot access '{op}': {e}"));
                status = 2;
            }
        }
    }
    sort_listing(&mut files, &a);
    dirs.sort();
    if a.has('r') {
        dirs.reverse();
    }
    let mut sections = 0;
    if !files.is_empty() {
        render(&ctx, &files, &a, false);
        sections += 1;
    }
    let headers = operands.len() > 1 || a.has('R');
    let mut queue: std::collections::VecDeque<String> = dirs.into();
    while let Some(dir) = queue.pop_front() {
        let mut items = match ctx.fs.read_dir(&dir).await {
            Ok(items) => items,
            Err(e) => {
                ctx.error(&format!("cannot open directory '{dir}': {e}"));
                status = 2;
                continue;
            }
        };
        if !(a.has('a') || a.has('A')) {
            items.retain(|i| !i.name.starts_with('.'));
        }
        let mut listed: Vec<Listed> = items.into_iter().map(|i| Listed { name: i.name, entry: i.entry }).collect();
        if a.has('a') {
            let self_entry = Entry { kind: EntryKind::Dir, size: 0, modified: SystemTime::UNIX_EPOCH };
            listed.push(Listed { name: ".".into(), entry: self_entry.clone() });
            listed.push(Listed { name: "..".into(), entry: self_entry });
        }
        sort_listing(&mut listed, &a);
        if headers {
            if sections > 0 {
                ctx.out("\n");
            }
            ctx.out(&format!("{dir}:\n"));
        }
        render(&ctx, &listed, &a, true);
        sections += 1;
        if a.has('R') {
            let mut subdirs: Vec<String> = listed
                .iter()
                .filter(|l| l.entry.kind == EntryKind::Dir && l.name != "." && l.name != "..")
                .map(|l| if dir.ends_with('/') { format!("{dir}{}", l.name) } else { format!("{dir}/{}", l.name) })
                .collect();
            subdirs.reverse();
            for d in subdirs {
                queue.push_front(d);
            }
        }
    }
    status
}

fn sort_listing(items: &mut [Listed], a: &super::Args) {
    if a.has('t') {
        items.sort_by(|x, y| y.entry.modified.cmp(&x.entry.modified).then(x.name.cmp(&y.name)));
    } else if a.has('S') {
        items.sort_by(|x, y| y.entry.size.cmp(&x.entry.size).then(x.name.cmp(&y.name)));
    } else {
        items.sort_by(|x, y| x.name.cmp(&y.name));
    }
    if a.has('r') {
        items.reverse();
    }
}

fn render(ctx: &UtilCtx<'_>, items: &[Listed], a: &super::Args, with_total: bool) {
    let classify = |l: &Listed| -> String {
        if (a.has('F') || a.has('p')) && l.entry.kind == EntryKind::Dir { format!("{}/", l.name) } else { l.name.clone() }
    };
    if !a.has('l') {
        for l in items {
            ctx.out(&format!("{}\n", classify(l)));
        }
        return;
    }
    let now = SystemTime::now();
    let sizes: Vec<String> = items
        .iter()
        .map(|l| {
            let size = if l.entry.kind == EntryKind::Dir { 4096 } else { l.entry.size };
            if a.has('h') { human(size) } else { size.to_string() }
        })
        .collect();
    let width = sizes.iter().map(String::len).max().unwrap_or(1);
    if with_total {
        let blocks: u64 = items.iter().map(|l| if l.entry.kind == EntryKind::Dir { 4 } else { l.entry.size.div_ceil(4096) * 4 }).sum();
        ctx.out(&format!("total {}\n", if a.has('h') { human(blocks * 1024) } else { blocks.to_string() }));
    }
    for (l, size) in items.iter().zip(sizes) {
        let (mode, links) = if l.entry.kind == EntryKind::Dir { ("drwxr-xr-x", 2) } else { ("-rw-r--r--", 1) };
        ctx.out(&format!(
            "{mode} {links} ply ply {size:>width$} {} {}\n",
            time::ls_time(l.entry.modified, now),
            classify(l)
        ));
    }
}

/// `ls -h` / `du -h` sizes: 1023, 1.0K, 15K, 1.5M ...
pub(crate) fn human(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["K", "M", "G", "T", "P", "E"];
    if bytes < 1024 {
        return bytes.to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    value /= 1024.0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        let rounded = (value * 10.0).ceil() / 10.0;
        if rounded < 10.0 {
            return format!("{rounded:.1}{}", UNITS[unit]);
        }
    }
    format!("{}{}", value.ceil() as u64, UNITS[unit])
}

pub(crate) async fn mkdir(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, "pv", "m", &[("parents", 'p'), ("verbose", 'v'), ("mode=", 'm')]);
    if a.operands.is_empty() {
        ctx.error("missing operand");
        return 1;
    }
    let mut status = 0;
    for dir in &a.operands {
        match ctx.fs.mkdir(dir, a.has('p')).await {
            Ok(()) => {
                if a.has('v') {
                    ctx.out(&format!("mkdir: created directory '{dir}'\n"));
                }
            }
            Err(e) => {
                ctx.error(&format!("cannot create directory '{dir}': {e}"));
                status = 1;
            }
        }
    }
    status
}

pub(crate) async fn rmdir(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, "pv", "", &[("parents", 'p')]);
    let mut status = 0;
    for dir in &a.operands {
        let mut target = dir.trim_end_matches('/').to_string();
        loop {
            if let Err(e) = ctx.fs.remove_dir(&target).await {
                ctx.error(&format!("failed to remove '{target}': {e}"));
                status = 1;
                break;
            }
            if !a.has('p') {
                break;
            }
            let parent = path::dirname(&target);
            if parent == "." || parent == "/" || parent == target {
                break;
            }
            target = parent;
        }
    }
    status
}

pub(crate) async fn rm(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, "rRfivdI", "", &[("recursive", 'r'), ("force", 'f'), ("verbose", 'v'), ("dir", 'd')]);
    let recursive = a.has('r') || a.has('R');
    let force = a.has('f');
    if a.operands.is_empty() && !force {
        ctx.error("missing operand");
        return 1;
    }
    let mut status = 0;
    for op in &a.operands {
        if recursive && ctx.fs.resolve(op) == "/" {
            ctx.error("it is dangerous to operate recursively on '/'");
            status = 1;
            continue;
        }
        let entry = match ctx.fs.stat(op).await {
            Ok(e) => e,
            Err(e) => {
                if !(force && e.errno == Errno::ENOENT) {
                    ctx.error(&format!("cannot remove '{op}': {e}"));
                    status = 1;
                }
                continue;
            }
        };
        let result = match (entry.kind, recursive, a.has('d')) {
            (EntryKind::Dir, true, _) => ctx.fs.remove_all(op).await,
            (EntryKind::Dir, false, true) => ctx.fs.remove_dir(op).await,
            (EntryKind::Dir, false, false) => {
                ctx.error(&format!("cannot remove '{op}': Is a directory"));
                status = 1;
                continue;
            }
            (EntryKind::File, _, _) => ctx.fs.remove_file(op).await,
        };
        match result {
            Ok(()) => {
                if a.has('v') {
                    ctx.out(&format!("removed '{op}'\n"));
                }
            }
            Err(e) => {
                ctx.error(&format!("cannot remove '{op}': {e}"));
                status = 1;
            }
        }
    }
    status
}

/// Where `src` lands for `cp`/`mv` given the destination operand.
async fn landing(ctx: &UtilCtx<'_>, src: &str, dest: &str, force_dir: bool) -> String {
    if force_dir || ctx.fs.is_dir(dest).await {
        path::join(dest, &path::basename(src))
    } else {
        dest.to_string()
    }
}

pub(crate) async fn cp(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, "rRafnpvLPdHuT", "t", &[("recursive", 'r'), ("archive", 'a'), ("force", 'f'), ("no-clobber", 'n'), ("verbose", 'v'), ("target-directory=", 't')]);
    let recursive = a.has('r') || a.has('R') || a.has('a');
    let (sources, dest, into_dir) = match a.value('t') {
        Some(t) => (a.operands.clone(), t.to_string(), true),
        None if a.operands.len() >= 2 => {
            let (dest, sources) = a.operands.split_last().unwrap_or((&a.operands[0], &[]));
            (sources.to_vec(), dest.clone(), a.operands.len() > 2)
        }
        None => {
            ctx.error(&match a.operands.first() {
                Some(op) => format!("missing destination file operand after '{op}'"),
                None => "missing file operand".into(),
            });
            return 1;
        }
    };
    if into_dir && !ctx.fs.is_dir(&dest).await {
        ctx.error(&format!("target '{dest}': Not a directory"));
        return 1;
    }
    let mut status = 0;
    for src in &sources {
        let entry = match ctx.fs.stat(src).await {
            Ok(e) => e,
            Err(e) => {
                ctx.error(&format!("cannot stat '{src}': {e}"));
                status = 1;
                continue;
            }
        };
        if entry.kind == EntryKind::Dir && !recursive {
            ctx.error(&format!("-r not specified; omitting directory '{src}'"));
            status = 1;
            continue;
        }
        let target = if a.has('T') { dest.clone() } else { landing(&ctx, src, &dest, into_dir).await };
        if a.has('n') && ctx.fs.exists(&target).await {
            continue;
        }
        let result = if entry.kind == EntryKind::Dir {
            ctx.fs.copy_tree(src, &target).await
        } else {
            ctx.fs.copy_file(src, &target).await
        };
        match result {
            Ok(()) => {
                if a.has('v') {
                    ctx.out(&format!("'{src}' -> '{target}'\n"));
                }
            }
            Err(e) if e.errno == Errno::EINVAL => {
                ctx.error(&format!("cannot copy a directory, '{src}', into itself, '{target}'"));
                status = 1;
            }
            Err(e) => {
                ctx.error(&format!("cannot create regular file '{target}': {e}"));
                status = 1;
            }
        }
    }
    status
}

pub(crate) async fn mv(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, "fnviuT", "t", &[("force", 'f'), ("no-clobber", 'n'), ("verbose", 'v'), ("target-directory=", 't')]);
    let (sources, dest, into_dir) = match a.value('t') {
        Some(t) => (a.operands.clone(), t.to_string(), true),
        None if a.operands.len() >= 2 => {
            let (dest, sources) = a.operands.split_last().unwrap_or((&a.operands[0], &[]));
            (sources.to_vec(), dest.clone(), a.operands.len() > 2)
        }
        None => {
            ctx.error(&match a.operands.first() {
                Some(op) => format!("missing destination file operand after '{op}'"),
                None => "missing file operand".into(),
            });
            return 1;
        }
    };
    if into_dir && !ctx.fs.is_dir(&dest).await {
        ctx.error(&format!("target '{dest}': Not a directory"));
        return 1;
    }
    let mut status = 0;
    for src in &sources {
        if let Err(e) = ctx.fs.stat(src).await {
            ctx.error(&format!("cannot stat '{src}': {e}"));
            status = 1;
            continue;
        }
        let target = if a.has('T') { dest.clone() } else { landing(&ctx, src, &dest, into_dir).await };
        if a.has('n') && ctx.fs.exists(&target).await {
            continue;
        }
        if ctx.fs.resolve(&target).starts_with(&format!("{}/", ctx.fs.resolve(src))) {
            ctx.error(&format!("cannot move '{src}' to a subdirectory of itself, '{target}'"));
            status = 1;
            continue;
        }
        let mut result = ctx.fs.rename(src, &target).await;
        if let Err(e) = &result
            && e.errno == Errno::EXDEV
        {
            // Across mounts: copy, then remove the original.
            result = match ctx.fs.copy_tree(src, &target).await {
                Ok(()) => ctx.fs.remove_all(src).await,
                Err(e) => Err(e),
            };
        }
        match result {
            Ok(()) => {
                if a.has('v') {
                    ctx.out(&format!("renamed '{src}' -> '{target}'\n"));
                }
            }
            Err(e) => {
                ctx.error(&format!("cannot move '{src}' to '{target}': {e}"));
                status = 1;
            }
        }
    }
    status
}

pub(crate) async fn touch(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, "acmh", "dtr", &[("no-create", 'c'), ("date=", 'd'), ("reference=", 'r')]);
    if a.operands.is_empty() {
        ctx.error("missing file operand");
        return 1;
    }
    let mut status = 0;
    for file in &a.operands {
        let result = match ctx.fs.read(file).await {
            // Rewriting the same bytes is how a storage without utimes bumps mtime.
            Ok(data) => ctx.fs.write(file, &data).await,
            Err(e) if e.errno == Errno::ENOENT => {
                if a.has('c') {
                    continue;
                }
                ctx.fs.write(file, b"").await
            }
            Err(e) if e.errno == Errno::EISDIR => Ok(()),
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            ctx.error(&format!("cannot touch '{file}': {e}"));
            status = 1;
        }
    }
    status
}

pub(crate) async fn realpath(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, "emsqzL P", "", &[("canonicalize-existing", 'e'), ("canonicalize-missing", 'm'), ("quiet", 'q')]);
    let mut status = 0;
    for op in &a.operands {
        let resolved = ctx.fs.resolve(op);
        let parent_ok = ctx.fs.is_dir(&path::dirname(&resolved)).await;
        let exists = ctx.fs.exists(&resolved).await;
        let ok = if a.has('m') { true } else if a.has('e') { exists } else { exists || parent_ok };
        if ok {
            ctx.out(&format!("{resolved}\n"));
        } else {
            if !a.has('q') {
                ctx.error(&format!("{op}: No such file or directory"));
            }
            status = 1;
        }
    }
    status
}

pub(crate) async fn readlink(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, "femnqsvz", "", &[("canonicalize", 'f'), ("canonicalize-existing", 'e'), ("canonicalize-missing", 'm')]);
    if !(a.has('f') || a.has('e') || a.has('m')) {
        // There are no symlinks, so plain `readlink` never has anything to print.
        return 1;
    }
    let mut status = 0;
    for op in &a.operands {
        let resolved = ctx.fs.resolve(op);
        if a.has('e') && !ctx.fs.exists(&resolved).await {
            status = 1;
            continue;
        }
        ctx.out(&format!("{resolved}\n"));
    }
    status
}

pub(crate) async fn basename(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, "az", "s", &[("multiple", 'a'), ("suffix=", 's'), ("zero", 'z')]);
    if a.operands.is_empty() {
        ctx.error("missing operand");
        return 1;
    }
    let (names, suffix): (Vec<&String>, Option<String>) = if a.has('a') || a.value('s').is_some() {
        (a.operands.iter().collect(), a.value('s').map(str::to_string))
    } else {
        (vec![&a.operands[0]], a.operands.get(1).cloned())
    };
    let end = if a.has('z') { "\0" } else { "\n" };
    for name in names {
        let mut base = path::basename(name);
        if let Some(s) = &suffix
            && base.len() > s.len()
            && base.ends_with(s.as_str())
        {
            base.truncate(base.len() - s.len());
        }
        ctx.out(&format!("{base}{end}"));
    }
    0
}

pub(crate) async fn dirname(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, "z", "", &[("zero", 'z')]);
    if a.operands.is_empty() {
        ctx.error("missing operand");
        return 1;
    }
    let end = if a.has('z') { "\0" } else { "\n" };
    for name in &a.operands {
        ctx.out(&format!("{}{end}", path::dirname(name)));
    }
    0
}

pub(crate) async fn stat(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, "LtfF", "c", &[("format=", 'c'), ("printf=", 'c'), ("dereference", 'L')]);
    let mut status = 0;
    for op in &a.operands {
        let entry = match ctx.fs.stat(op).await {
            Ok(e) => e,
            Err(e) => {
                ctx.error(&format!("cannot stat '{op}': {e}"));
                status = 1;
                continue;
            }
        };
        let is_dir = entry.kind == EntryKind::Dir;
        let size = if is_dir { 4096 } else { entry.size };
        let kind = if is_dir { "directory" } else if size == 0 { "regular empty file" } else { "regular file" };
        let mtime = time::unix_seconds(entry.modified);
        match a.value('c') {
            Some(format) => {
                let mut out = String::new();
                let mut chars = format.chars();
                while let Some(c) = chars.next() {
                    if c != '%' {
                        out.push(c);
                        continue;
                    }
                    match chars.next() {
                        Some('n') => out.push_str(op),
                        Some('s') => out.push_str(&size.to_string()),
                        Some('F') => out.push_str(kind),
                        Some('Y') | Some('Z') | Some('X') | Some('W') => out.push_str(&mtime.to_string()),
                        Some('y') | Some('z') | Some('x') | Some('w') => out.push_str(&time::iso_full(entry.modified)),
                        Some('a') => out.push_str(if is_dir { "755" } else { "644" }),
                        Some('A') => out.push_str(if is_dir { "drwxr-xr-x" } else { "-rw-r--r--" }),
                        Some('U') | Some('G') => out.push_str("ply"),
                        Some('u') | Some('g') => out.push_str("1000"),
                        Some('h') => out.push_str(if is_dir { "2" } else { "1" }),
                        Some('%') => out.push('%'),
                        Some(other) => {
                            out.push('%');
                            out.push(other);
                        }
                        None => out.push('%'),
                    }
                }
                ctx.out(&format!("{out}\n"));
            }
            None => {
                let t = time::iso_full(entry.modified);
                ctx.out(&format!(
                    "  File: {op}\n  Size: {size}\t\tBlocks: {}\tIO Block: 4096   {kind}\nAccess: ({}/{})  Uid: ( 1000/   ply)   Gid: ( 1000/   ply)\nModify: {t}\n",
                    size.div_ceil(512),
                    if is_dir { "0755" } else { "0644" },
                    if is_dir { "drwxr-xr-x" } else { "-rw-r--r--" },
                ));
            }
        }
    }
    status
}

pub(crate) async fn du(ctx: UtilCtx<'_>) -> i32 {
    let a = args_or_exit!(ctx, "shacbkm", "d", &[("summarize", 's'), ("human-readable", 'h'), ("all", 'a'), ("total", 'c'), ("max-depth=", 'd')]);
    let operands = if a.operands.is_empty() { vec![".".to_string()] } else { a.operands.clone() };
    let max_depth: Option<usize> = if a.has('s') { Some(0) } else { a.value('d').and_then(|d| d.parse().ok()) };
    let show = |bytes: u64| if a.has('h') { human(bytes) } else if a.has('b') { bytes.to_string() } else { bytes.div_ceil(1024).to_string() };
    let mut status = 0;
    let mut grand = 0;
    for op in &operands {
        let tree = match ctx.fs.walk(op).await {
            Ok(t) => t,
            Err(e) => {
                ctx.error(&format!("cannot access '{op}': {e}"));
                status = 1;
                continue;
            }
        };
        let root = ctx.fs.resolve(op);
        let usage = |e: &Entry| if e.kind == EntryKind::Dir { 4096 } else { e.size.div_ceil(4096) * 4096 };
        // Directory totals, deepest first so each prints after its children.
        let mut rows: Vec<(usize, String, u64)> = Vec::new();
        for (abs, entry) in &tree {
            let rel_depth = if *abs == root { 0 } else { abs[root.len()..].matches('/').count() };
            let total: u64 = if entry.kind == EntryKind::Dir {
                tree.iter().filter(|(p, _)| p == abs || p.starts_with(&format!("{abs}/"))).map(|(_, e)| usage(e)).sum()
            } else {
                usage(entry)
            };
            let shown = if *abs == root { op.clone() } else { format!("{}{}", op.trim_end_matches('/'), &abs[root.len()..]) };
            if entry.kind == EntryKind::Dir || a.has('a') || *abs == root {
                rows.push((rel_depth, shown, total));
            }
        }
        let root_total = rows.first().map(|r| r.2).unwrap_or(0);
        grand += root_total;
        rows.reverse();
        for (depth, shown, total) in rows {
            if max_depth.is_some_and(|m| depth > m) {
                continue;
            }
            ctx.out(&format!("{}\t{shown}\n", show(total)));
        }
    }
    if a.has('c') {
        ctx.out(&format!("{}\ttotal\n", show(grand)));
    }
    status
}

#[cfg(test)]
mod tests {
    use super::human;

    #[test]
    fn human_sizes_round_up_like_gnu() {
        assert_eq!(human(1000), "1000");
        assert_eq!(human(1024), "1.0K");
        assert_eq!(human(1536), "1.5K");
        assert_eq!(human(15 * 1024), "15K");
        assert_eq!(human(3 * 1024 * 1024 / 2), "1.5M");
    }
}
