//! env printenv sleep uname whoami hostname.

use super::UtilCtx;

pub(crate) async fn env(ctx: UtilCtx<'_>) -> i32 {
    let mut vars = ctx.env.clone();
    let mut rest = ctx.args.iter().peekable();
    while let Some(arg) = rest.peek() {
        if arg.as_str() == "-i" || arg.as_str() == "-" {
            vars.clear();
        } else if let Some(name) = arg.strip_prefix("-u") {
            let name = if name.is_empty() {
                rest.next();
                rest.peek().map(|s| s.to_string()).unwrap_or_default()
            } else {
                name.to_string()
            };
            vars.remove(&name);
        } else if let Some((k, v)) = arg.split_once('=') {
            vars.insert(k.to_string(), v.to_string());
        } else {
            // TODO(ply-sh): `env VAR=x cmd` needs the shell to run `cmd`;
            // it becomes a builtin once the interpreter exposes a runner to utilities.
            ctx.error(&format!("running '{arg}' through env is not supported; use `VAR=x {arg}` instead"));
            return 125;
        }
        rest.next();
    }
    for (k, v) in vars {
        ctx.out(&format!("{k}={v}\n"));
    }
    0
}

pub(crate) async fn printenv(ctx: UtilCtx<'_>) -> i32 {
    if ctx.args.is_empty() {
        for (k, v) in ctx.env {
            ctx.out(&format!("{k}={v}\n"));
        }
        return 0;
    }
    let mut status = 0;
    for name in ctx.args {
        match ctx.env.get(name) {
            Some(v) => ctx.out(&format!("{v}\n")),
            None => status = 1,
        }
    }
    status
}

pub(crate) async fn sleep(ctx: UtilCtx<'_>) -> i32 {
    let mut total = 0.0f64;
    for arg in ctx.args {
        let (num, mult) = match arg.chars().last() {
            Some('s') => (&arg[..arg.len() - 1], 1.0),
            Some('m') => (&arg[..arg.len() - 1], 60.0),
            Some('h') => (&arg[..arg.len() - 1], 3600.0),
            Some('d') => (&arg[..arg.len() - 1], 86400.0),
            _ => (arg.as_str(), 1.0),
        };
        match num.parse::<f64>() {
            Ok(v) if v >= 0.0 => total += v * mult,
            _ => {
                ctx.error(&format!("invalid time interval '{arg}'"));
                return 1;
            }
        }
    }
    if ctx.args.is_empty() {
        ctx.error("missing operand");
        return 1;
    }
    tokio::time::sleep(std::time::Duration::from_secs_f64(total)).await;
    0
}

pub(crate) async fn uname(ctx: UtilCtx<'_>) -> i32 {
    let all = ctx.args.iter().any(|a| a == "-a" || a == "--all");
    let mut parts = Vec::new();
    let want = |f: &str| all || ctx.args.iter().any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains(f));
    if want("s") || ctx.args.is_empty() {
        parts.push("Linux");
    }
    if want("n") {
        parts.push("ply");
    }
    if want("r") {
        parts.push("6.0.0-ply");
    }
    if want("v") {
        parts.push("#1 SMP");
    }
    if want("m") {
        parts.push("x86_64");
    }
    if all || want("o") {
        parts.push("GNU/Linux");
    }
    ctx.out(&format!("{}\n", parts.join(" ")));
    0
}

pub(crate) async fn whoami(ctx: UtilCtx<'_>) -> i32 {
    ctx.out(&format!("{}\n", ctx.env.get("USER").map(String::as_str).unwrap_or("ply")));
    0
}

pub(crate) async fn hostname(ctx: UtilCtx<'_>) -> i32 {
    ctx.out(&format!("{}\n", ctx.env.get("HOSTNAME").map(String::as_str).unwrap_or("ply")));
    0
}
