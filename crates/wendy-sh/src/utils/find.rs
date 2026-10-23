//! find: `-name -iname -path -ipath -regex -type -size -empty -newer
//! -maxdepth -mindepth -prune -print -print0 -delete -true -false`,
//! `! -not ( ) -a -and -o -or`. `-exec` needs a shell and is reported as
//! unsupported rather than silently skipped.

use wendy_io::{Entry, EntryKind};
use wendy_libc::glob::Pattern;

use super::UtilCtx;

enum Expr {
    Name(Pattern, bool),
    Path(Pattern, bool),
    Regex(regex::Regex),
    Type(EntryKind),
    Size(char, u64, u64),
    Empty,
    Newer(std::time::SystemTime),
    Prune,
    Print(bool),
    Delete,
    True,
    False,
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

struct Parser<'a> {
    toks: &'a [String],
    pos: usize,
    has_action: bool,
    maxdepth: Option<usize>,
    mindepth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&str> {
        self.toks.get(self.pos).map(String::as_str)
    }

    fn arg(&mut self, primary: &str) -> Result<String, String> {
        self.pos += 1;
        self.toks.get(self.pos - 1).cloned().ok_or_else(|| format!("missing argument to `{primary}'"))
    }

    fn or(&mut self, ctx: &FindCtx) -> Result<Expr, String> {
        let mut left = self.and(ctx)?;
        while matches!(self.peek(), Some("-o") | Some("-or")) {
            self.pos += 1;
            left = Expr::Or(Box::new(left), Box::new(self.and(ctx)?));
        }
        Ok(left)
    }

    fn and(&mut self, ctx: &FindCtx) -> Result<Expr, String> {
        let mut left = self.not(ctx)?;
        loop {
            match self.peek() {
                Some("-a") | Some("-and") => self.pos += 1,
                Some("-o") | Some("-or") | Some(")") | None => break,
                _ => {}
            }
            left = Expr::And(Box::new(left), Box::new(self.not(ctx)?));
        }
        Ok(left)
    }

    fn not(&mut self, ctx: &FindCtx) -> Result<Expr, String> {
        if matches!(self.peek(), Some("!") | Some("-not")) {
            self.pos += 1;
            return Ok(Expr::Not(Box::new(self.not(ctx)?)));
        }
        self.primary(ctx)
    }

    fn primary(&mut self, ctx: &FindCtx) -> Result<Expr, String> {
        let Some(tok) = self.peek().map(str::to_string) else { return Err("expected an expression".into()) };
        self.pos += 1;
        Ok(match tok.as_str() {
            "(" => {
                let e = self.or(ctx)?;
                if self.peek() != Some(")") {
                    return Err("missing `)'".into());
                }
                self.pos += 1;
                e
            }
            "-name" | "-iname" => {
                let p = self.arg(&tok)?;
                let icase = tok == "-iname";
                Expr::Name(Pattern::new(&if icase { p.to_lowercase() } else { p }), icase)
            }
            "-path" | "-wholename" | "-ipath" | "-iwholename" => {
                let p = self.arg(&tok)?;
                let icase = tok.starts_with("-i");
                Expr::Path(Pattern::new(&if icase { p.to_lowercase() } else { p }), icase)
            }
            "-regex" | "-iregex" => {
                let p = self.arg(&tok)?;
                let re = regex::RegexBuilder::new(&format!("^(?:{})$", super::regex_util::translate(&p, false)?))
                    .case_insensitive(tok == "-iregex")
                    .build()
                    .map_err(|e| e.to_string())?;
                Expr::Regex(re)
            }
            "-type" => match self.arg(&tok)?.as_str() {
                "f" => Expr::Type(EntryKind::File),
                "d" => Expr::Type(EntryKind::Dir),
                // No symlinks, sockets, pipes or devices exist.
                "l" | "s" | "p" | "b" | "c" => Expr::False,
                other => return Err(format!("Unknown argument to -type: {other}")),
            },
            "-size" => {
                let spec = self.arg(&tok)?;
                let (cmp, rest) = match spec.chars().next() {
                    Some(c @ ('+' | '-')) => (c, &spec[1..]),
                    _ => ('=', spec.as_str()),
                };
                let (num, unit) = rest.split_at(rest.trim_end_matches(|c: char| c.is_ascii_alphabetic()).len());
                let n: u64 = num.parse().map_err(|_| format!("invalid -size argument `{spec}'"))?;
                let unit = match unit {
                    "c" => 1,
                    "w" => 2,
                    "k" => 1024,
                    "M" => 1024 * 1024,
                    "G" => 1024 * 1024 * 1024,
                    "" | "b" => 512,
                    _ => return Err(format!("invalid -size argument `{spec}'")),
                };
                Expr::Size(cmp, n, unit)
            }
            "-empty" => Expr::Empty,
            "-newer" => {
                let reference = self.arg(&tok)?;
                let t = ctx.newer.iter().find(|(p, _)| *p == reference).map(|(_, t)| *t).ok_or_else(|| format!("'{reference}': No such file or directory"))?;
                Expr::Newer(t)
            }
            "-maxdepth" => {
                self.maxdepth = Some(self.arg(&tok)?.parse().map_err(|_| "invalid -maxdepth".to_string())?);
                Expr::True
            }
            "-mindepth" => {
                self.mindepth = self.arg(&tok)?.parse().map_err(|_| "invalid -mindepth".to_string())?;
                Expr::True
            }
            "-depth" | "-xdev" | "-mount" | "-follow" | "-noleaf" | "-ignore_readdir_race" => Expr::True,
            "-prune" => Expr::Prune,
            "-print" => {
                self.has_action = true;
                Expr::Print(false)
            }
            "-print0" => {
                self.has_action = true;
                Expr::Print(true)
            }
            "-delete" => {
                self.has_action = true;
                Expr::Delete
            }
            "-true" => Expr::True,
            "-false" => Expr::False,
            "-exec" | "-execdir" | "-ok" | "-okdir" => return Err(format!("{tok} is not supported (pipe to xargs or loop in the shell instead)")),
            other => return Err(format!("unknown predicate `{other}'")),
        })
    }
}

/// Pre-resolved `-newer` reference times.
struct FindCtx {
    newer: Vec<(String, std::time::SystemTime)>,
}

struct Visit<'a> {
    shown: &'a str,
    name: &'a str,
    entry: &'a Entry,
}

#[derive(Default)]
struct Effects {
    pruned: bool,
    out: String,
    delete: bool,
}

fn eval(e: &Expr, v: &Visit<'_>, fx: &mut Effects) -> bool {
    match e {
        Expr::Name(p, icase) => p.matches(&if *icase { v.name.to_lowercase() } else { v.name.to_string() }),
        Expr::Path(p, icase) => p.matches(&if *icase { v.shown.to_lowercase() } else { v.shown.to_string() }),
        Expr::Regex(re) => re.is_match(v.shown),
        Expr::Type(k) => v.entry.kind == *k,
        Expr::Size(cmp, n, unit) => {
            let units = v.entry.size.div_ceil(*unit);
            match cmp {
                '+' => units > *n,
                '-' => units < *n,
                _ => units == *n,
            }
        }
        Expr::Empty => v.entry.size == 0,
        Expr::Newer(t) => v.entry.modified > *t,
        Expr::Prune => {
            fx.pruned = true;
            true
        }
        Expr::Print(nul) => {
            fx.out.push_str(v.shown);
            fx.out.push(if *nul { '\0' } else { '\n' });
            true
        }
        Expr::Delete => {
            fx.delete = true;
            true
        }
        Expr::True => true,
        Expr::False => false,
        Expr::Not(inner) => !eval(inner, v, fx),
        Expr::And(a, b) => eval(a, v, fx) && eval(b, v, fx),
        Expr::Or(a, b) => eval(a, v, fx) || eval(b, v, fx),
    }
}

pub(crate) async fn find(ctx: UtilCtx<'_>) -> i32 {
    let split = ctx.args.iter().position(|a| a.starts_with('-') || a == "(" || a == "!").unwrap_or(ctx.args.len());
    let paths: Vec<String> = if split == 0 { vec![".".into()] } else { ctx.args[..split].to_vec() };
    let expr_toks = &ctx.args[split..];

    let mut newer = Vec::new();
    for w in expr_toks.windows(2) {
        if w[0] == "-newer"
            && let Ok(e) = ctx.fs.stat(&w[1]).await
        {
            newer.push((w[1].clone(), e.modified));
        }
    }
    let fctx = FindCtx { newer };
    let mut p = Parser { toks: expr_toks, pos: 0, has_action: false, maxdepth: None, mindepth: 0 };
    let expr = if expr_toks.is_empty() {
        Expr::True
    } else {
        match p.or(&fctx) {
            Ok(e) if p.pos == expr_toks.len() => e,
            Ok(_) => {
                ctx.error(&format!("paths must precede expression: `{}'", expr_toks[p.pos]));
                return 1;
            }
            Err(e) => {
                ctx.error(&e);
                return 1;
            }
        }
    };
    let expr = if p.has_action { expr } else { Expr::And(Box::new(expr), Box::new(Expr::Print(false))) };

    let mut status = 0;
    let mut deletions = Vec::new();
    for start in &paths {
        let tree = match ctx.fs.walk(start).await {
            Ok(t) => t,
            Err(e) => {
                ctx.error(&format!("'{start}': {e}"));
                status = 1;
                continue;
            }
        };
        let root = ctx.fs.resolve(start);
        let mut pruned: Vec<String> = Vec::new();
        for (abs, entry) in &tree {
            if pruned.iter().any(|d| abs.starts_with(&format!("{d}/"))) {
                continue;
            }
            let rel = if *abs == root { "" } else if root == "/" { &abs[1..] } else { &abs[root.len() + 1..] };
            let depth = if rel.is_empty() { 0 } else { rel.matches('/').count() + 1 };
            if p.maxdepth.is_some_and(|m| depth > m) {
                continue;
            }
            let shown = if rel.is_empty() {
                start.clone()
            } else if start.ends_with('/') {
                format!("{start}{rel}")
            } else {
                format!("{start}/{rel}")
            };
            let name = if rel.is_empty() { wendy_libc::path::basename(start) } else { wendy_libc::path::basename(abs) };
            let mut fx = Effects::default();
            if depth >= p.mindepth {
                eval(&expr, &Visit { shown: &shown, name: &name, entry }, &mut fx);
            }
            ctx.out(&fx.out);
            if fx.pruned && entry.kind == EntryKind::Dir {
                pruned.push(abs.clone());
            }
            if fx.delete {
                deletions.push((shown, abs.clone()));
            }
            if p.maxdepth.is_some_and(|m| depth == m) && entry.kind == EntryKind::Dir {
                pruned.push(abs.clone());
            }
        }
    }
    // -delete works bottom-up so directories are empty when removed.
    for (shown, abs) in deletions.into_iter().rev() {
        let result = match ctx.fs.stat(&abs).await {
            Ok(e) if e.kind == EntryKind::Dir => ctx.fs.remove_dir(&abs).await,
            Ok(_) => ctx.fs.remove_file(&abs).await,
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            ctx.error(&format!("cannot delete '{shown}': {e}"));
            status = 1;
        }
    }
    status
}
