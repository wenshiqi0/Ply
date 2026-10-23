//! Builtins: commands that need the shell's own state (variables, cwd,
//! options, control flow). Everything that only needs files and streams is
//! a utility (see `crate::utils`).

mod declare;
mod printf;
mod read;

use wendy_libc::Fs;

use crate::exec::state::{Fds, Value};
use crate::exec::{Ctrl, Shell, Status};
use crate::syntax::parse;

pub(crate) use declare::{DeclArg, declaration, is_declaration};

const BUILTINS: &[&str] = &[
    ":", "true", "false", "echo", "printf", "cd", "pwd", "exit", "return", "break", "continue",
    "eval", "source", ".", "test", "[", "type", "command", "builtin", "let", "wait", "exec", "trap",
    "shopt", "set", "shift", "unset", "read", "mapfile", "readarray", "getopts", "declare",
    "typeset", "local", "export", "readonly", "hash", "umask", "ulimit", "alias", "unalias",
    "times", "jobs", "disown", "enable", "help",
];

pub(crate) fn is_builtin(name: &str) -> bool {
    BUILTINS.contains(&name)
}

pub(crate) async fn run(sh: &mut Shell, argv: &[String], fds: &Fds) -> Status {
    let name = argv[0].as_str();
    let args = &argv[1..];
    match name {
        ":" | "true" => Ok(0),
        "false" => Ok(1),
        "echo" => Ok(printf::echo(args, fds)),
        "printf" => printf::printf(sh, args, fds),
        "cd" => cd(sh, args, fds).await,
        "pwd" => {
            fds.out.write_str(&format!("{}\n", sh.st.cwd));
            Ok(0)
        }
        "exit" => {
            let code = match args.first() {
                Some(a) => match a.parse::<i64>() {
                    Ok(n) => n.rem_euclid(256) as i32,
                    Err(_) => {
                        sh.report(fds, &format!("exit: {a}: numeric argument required"));
                        2
                    }
                },
                None => sh.st.last_status,
            };
            Err(Ctrl::Exit(code))
        }
        "return" => {
            if sh.st.func_depth == 0 && sh.st.source_depth == 0 {
                sh.report(fds, "return: can only `return' from a function or sourced script");
                return Ok(1);
            }
            let code = args.first().and_then(|a| a.parse::<i64>().ok()).map_or(sh.st.last_status, |n| n.rem_euclid(256) as i32);
            Err(Ctrl::Return(code))
        }
        "break" | "continue" => {
            let n = match args.first().map(|a| a.parse::<u32>()) {
                None => 1,
                Some(Ok(n)) if n > 0 => n,
                Some(_) => {
                    sh.report(fds, &format!("{name}: {}: loop count out of range", args[0]));
                    return Ok(1);
                }
            };
            if sh.st.loop_depth == 0 {
                sh.report(fds, &format!("{name}: only meaningful in a `for', `while', or `until' loop"));
                return Ok(0);
            }
            let n = n.min(sh.st.loop_depth as u32);
            Err(if name == "break" { Ctrl::Break(n) } else { Ctrl::Continue(n) })
        }
        "eval" => {
            let text = args.join(" ");
            match parse(&text) {
                Ok(script) => sh.exec_script(&script, fds).await,
                Err(e) => {
                    sh.report(fds, &format!("eval: line {}: {}", e.line, e.message));
                    Ok(2)
                }
            }
        }
        "source" | "." => source(sh, args, fds).await,
        "test" => Ok(test(sh, name, args, fds).await),
        "[" => {
            if args.last().map(String::as_str) != Some("]") {
                sh.report(fds, "[: missing `]'");
                return Ok(2);
            }
            Ok(test(sh, name, &args[..args.len() - 1], fds).await)
        }
        "type" => Ok(type_builtin(sh, args, fds)),
        "command" => command(sh, args, fds).await,
        "builtin" => {
            if args.is_empty() {
                return Ok(0);
            }
            if !is_builtin(&args[0]) {
                sh.report(fds, &format!("builtin: {}: not a shell builtin", args[0]));
                return Ok(1);
            }
            Box::pin(run(sh, args, fds)).await
        }
        "let" => {
            if args.is_empty() {
                sh.report(fds, "let: expression expected");
                return Ok(1);
            }
            let mut last = 0;
            for expr in args {
                last = sh.arith(expr, fds)?;
            }
            Ok((last == 0) as i32)
        }
        "wait" => Ok(sh.st.last_bg_status),
        "exec" => {
            if args.is_empty() {
                // Redirections were applied to this command only.
                return Ok(0);
            }
            let status = Box::pin(sh.dispatch(args, fds)).await?;
            Err(Ctrl::Exit(status))
        }
        "trap" => Ok(trap(sh, args, fds)),
        "shopt" => Ok(shopt(sh, args, fds)),
        "set" => Ok(set(sh, args, fds)),
        "shift" => {
            let n = args.first().and_then(|a| a.parse::<usize>().ok()).unwrap_or(1);
            if n > sh.st.positional.len() {
                return Ok(1);
            }
            sh.st.positional.drain(..n);
            Ok(0)
        }
        "unset" => Ok(unset(sh, args, fds)),
        "read" => read::read(sh, args, fds),
        "mapfile" | "readarray" => read::mapfile(sh, name, args, fds),
        "getopts" => Ok(read::getopts(sh, args, fds)),
        "declare" | "typeset" | "local" | "export" | "readonly" => {
            let items = args.iter().map(|a| DeclArg::from_plain(a)).collect();
            declaration(sh, name, items, fds).await
        }
        "umask" => {
            if args.is_empty() {
                fds.out.write_str("0022\n");
            }
            Ok(0)
        }
        "ulimit" => {
            if args.is_empty() || args.iter().any(|a| a == "-a") {
                fds.out.write_str("unlimited\n");
            }
            Ok(0)
        }
        // Accepted for compatibility; nothing to do without processes or aliases.
        "hash" | "alias" | "unalias" | "times" | "jobs" | "disown" | "enable" | "help" => Ok(0),
        _ => Ok(127),
    }
}

async fn cd(sh: &mut Shell, args: &[String], fds: &Fds) -> Status {
    let args: Vec<&String> = args.iter().filter(|a| !matches!(a.as_str(), "-L" | "-P" | "-e" | "-@")).collect();
    let var = |sh: &Shell, n: &str| sh.st.vars.value(n).and_then(Value::scalar);
    let (target, announce) = match args.first().map(|s| s.as_str()) {
        None => match var(sh, "HOME") {
            Some(h) => (h, false),
            None => {
                sh.report(fds, "cd: HOME not set");
                return Ok(1);
            }
        },
        Some("-") => match var(sh, "OLDPWD") {
            Some(o) => (o, true),
            None => {
                sh.report(fds, "cd: OLDPWD not set");
                return Ok(1);
            }
        },
        Some(dir) => (dir.to_string(), false),
    };
    if args.len() > 1 {
        sh.report(fds, "cd: too many arguments");
        return Ok(1);
    }
    let fs = Fs::new(sh.rt.io.labeled("cd"), sh.st.cwd.clone());
    match fs.stat(&target).await {
        Ok(entry) if entry.kind == wendy_io::EntryKind::Dir => {}
        Ok(_) => {
            sh.report(fds, &format!("cd: {target}: Not a directory"));
            return Ok(1);
        }
        Err(e) => {
            sh.report(fds, &format!("cd: {target}: {e}"));
            return Ok(1);
        }
    }
    let new = fs.resolve(&target);
    let old = std::mem::replace(&mut sh.st.cwd, new.clone());
    let _ = sh.st.vars.set_str("OLDPWD", old);
    let _ = sh.st.vars.set_str("PWD", new.clone());
    if announce {
        fds.out.write_str(&format!("{new}\n"));
    }
    Ok(0)
}

async fn source(sh: &mut Shell, args: &[String], fds: &Fds) -> Status {
    let Some(file) = args.first() else {
        sh.report(fds, "source: filename argument required");
        return Ok(2);
    };
    let fs = Fs::new(sh.rt.io.labeled("source"), sh.st.cwd.clone());
    let text = match fs.read(file).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            sh.report(fds, &format!("{file}: {e}"));
            return Ok(1);
        }
    };
    let script = match parse(&text) {
        Ok(s) => s,
        Err(e) => {
            fds.err.write_str(&format!("{file}: line {}: {}\n", e.line, e.message));
            return Ok(2);
        }
    };
    let saved = (args.len() > 1).then(|| std::mem::replace(&mut sh.st.positional, args[1..].to_vec()));
    sh.st.source_depth += 1;
    let result = sh.exec_script(&script, fds).await;
    sh.st.source_depth -= 1;
    if let Some(saved) = saved {
        sh.st.positional = saved;
    }
    match result {
        Err(Ctrl::Return(n)) => Ok(n),
        other => other,
    }
}

async fn test(sh: &mut Shell, name: &str, args: &[String], fds: &Fds) -> i32 {
    match sh.run_test(args).await {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(msg) => {
            sh.report(fds, &format!("{name}: {msg}"));
            2
        }
    }
}

/// What a command name refers to, for `type` and `command -v`.
enum Kind {
    Function,
    Builtin,
    Utility,
    Missing,
}

fn kind_of(sh: &Shell, name: &str) -> Kind {
    if sh.st.functions.contains_key(name) {
        Kind::Function
    } else if is_builtin(name) {
        Kind::Builtin
    } else if sh.rt.utils.resolve(name).is_some() {
        Kind::Utility
    } else {
        Kind::Missing
    }
}

fn type_builtin(sh: &mut Shell, args: &[String], fds: &Fds) -> i32 {
    let terse = args.iter().any(|a| a == "-t");
    let path_only = args.iter().any(|a| a == "-p" || a == "-P");
    let mut status = 0;
    for name in args.iter().filter(|a| !a.starts_with('-')) {
        let line = match (kind_of(sh, name), terse) {
            (Kind::Function, true) => Some("function".to_string()),
            (Kind::Builtin, true) => Some("builtin".to_string()),
            (Kind::Utility, true) => Some("file".to_string()),
            (Kind::Function, false) if !path_only => Some(format!("{name} is a function")),
            (Kind::Builtin, false) if !path_only => Some(format!("{name} is a shell builtin")),
            (Kind::Utility, _) if path_only => Some(utility_path(name)),
            (Kind::Utility, false) => Some(format!("{name} is {}", utility_path(name))),
            (Kind::Missing, _) => {
                if !terse && !path_only {
                    sh.report(fds, &format!("type: {name}: not found"));
                }
                status = 1;
                None
            }
            _ => None,
        };
        if let Some(line) = line {
            fds.out.write_str(&format!("{line}\n"));
        }
    }
    status
}

/// Utilities answer to the usual absolute paths (`/usr/bin/cat`), so
/// `$(command -v cat)` and `#!/bin/sh`-style invocations keep working.
fn utility_path(name: &str) -> String {
    format!("/usr/bin/{name}")
}

async fn command(sh: &mut Shell, args: &[String], fds: &Fds) -> Status {
    match args.first().map(String::as_str) {
        Some("-v") | Some("-V") => {
            let verbose = args[0] == "-V";
            let mut status = 0;
            for name in &args[1..] {
                let line = match kind_of(sh, name) {
                    Kind::Function if verbose => format!("{name} is a function"),
                    Kind::Builtin if verbose => format!("{name} is a shell builtin"),
                    Kind::Utility if verbose => format!("{name} is {}", utility_path(name)),
                    Kind::Function | Kind::Builtin => name.clone(),
                    Kind::Utility => utility_path(name),
                    Kind::Missing => {
                        if verbose {
                            sh.report(fds, &format!("command: {name}: not found"));
                        }
                        status = 1;
                        continue;
                    }
                };
                fds.out.write_str(&format!("{line}\n"));
            }
            Ok(status)
        }
        Some(_) => {
            let args = if args[0] == "-p" { &args[1..] } else { args };
            if args.is_empty() {
                return Ok(0);
            }
            // Skip functions: run the builtin or utility of that name.
            let saved = sh.st.functions.remove(&args[0]);
            let result = Box::pin(sh.dispatch(args, fds)).await;
            if let Some(body) = saved {
                sh.st.functions.insert(args[0].clone(), body);
            }
            result
        }
        None => Ok(0),
    }
}

fn trap(sh: &mut Shell, args: &[String], fds: &Fds) -> i32 {
    match args {
        [] => {
            if let Some(t) = &sh.st.exit_trap {
                fds.out.write_str(&format!("trap -- '{}' EXIT\n", t.replace('\'', r"'\''")));
            }
            0
        }
        [flag] if flag == "-p" || flag == "-l" => 0,
        [action, signals @ ..] => {
            for signal in signals {
                if matches!(signal.as_str(), "EXIT" | "0" | "SIGEXIT") {
                    sh.st.exit_trap = if action == "-" || action.is_empty() { None } else { Some(action.clone()) };
                }
                // Other signals never arrive in this shell; accepted and ignored.
            }
            0
        }
    }
}

fn shopt(sh: &mut Shell, args: &[String], fds: &Fds) -> i32 {
    let mut set = None;
    let mut quiet = false;
    let mut names = Vec::new();
    for a in args {
        match a.as_str() {
            "-s" => set = Some(true),
            "-u" => set = Some(false),
            "-q" => quiet = true,
            "-p" | "-o" => {}
            name => names.push(name),
        }
    }
    let mut status = 0;
    for name in names {
        let slot = match name {
            "nullglob" => Some(&mut sh.st.opts.nullglob),
            "dotglob" => Some(&mut sh.st.opts.dotglob),
            "failglob" => Some(&mut sh.st.opts.failglob),
            "extglob" | "globstar" | "expand_aliases" | "lastpipe" | "inherit_errexit" | "nocasematch"
            | "nocaseglob" | "checkwinsize" | "histappend" | "sourcepath" => None,
            _ => {
                sh.report(fds, &format!("shopt: {name}: invalid shell option name"));
                status = 1;
                continue;
            }
        };
        match (slot, set) {
            (Some(slot), Some(v)) => *slot = v,
            (Some(slot), None) => {
                if !*slot {
                    status = 1;
                }
                if !quiet {
                    fds.out.write_str(&format!("{name}\t{}\n", if *slot { "on" } else { "off" }));
                }
            }
            (None, _) => {}
        }
    }
    status
}

fn set(sh: &mut Shell, args: &[String], fds: &Fds) -> i32 {
    if args.is_empty() {
        let names = sh.st.vars.names();
        for name in names {
            if let Some(v) = sh.st.vars.value(&name).and_then(Value::scalar) {
                fds.out.write_str(&format!("{name}={}\n", declare::shell_quote(&v)));
            }
        }
        return 0;
    }
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            sh.st.positional = args[i + 1..].to_vec();
            return 0;
        }
        let (on, flags) = match arg.chars().next() {
            Some('-') => (true, &arg[1..]),
            Some('+') => (false, &arg[1..]),
            _ => {
                sh.st.positional = args[i..].to_vec();
                return 0;
            }
        };
        if flags.is_empty() {
            // `set -` turns off -x and -v; the rest are positional.
            sh.st.opts.xtrace = false;
            sh.st.positional = args[i + 1..].to_vec();
            return 0;
        }
        for flag in flags.chars() {
            match flag {
                'e' => sh.st.opts.errexit = on,
                'u' => sh.st.opts.nounset = on,
                'x' => sh.st.opts.xtrace = on,
                'f' => sh.st.opts.noglob = on,
                'C' => sh.st.opts.noclobber = on,
                'h' | 'B' | 'H' | 'm' | 'v' | 'a' | 'b' | 'n' | 'P' | 'T' | 'E' => {}
                'o' => {
                    i += 1;
                    let Some(opt) = args.get(i) else {
                        for (name, value) in [
                            ("errexit", sh.st.opts.errexit),
                            ("noclobber", sh.st.opts.noclobber),
                            ("noglob", sh.st.opts.noglob),
                            ("nounset", sh.st.opts.nounset),
                            ("pipefail", sh.st.opts.pipefail),
                            ("xtrace", sh.st.opts.xtrace),
                        ] {
                            fds.out.write_str(&format!("{name:<15}\t{}\n", if value { "on" } else { "off" }));
                        }
                        return 0;
                    };
                    match opt.as_str() {
                        "errexit" => sh.st.opts.errexit = on,
                        "nounset" => sh.st.opts.nounset = on,
                        "pipefail" => sh.st.opts.pipefail = on,
                        "xtrace" => sh.st.opts.xtrace = on,
                        "noglob" => sh.st.opts.noglob = on,
                        "noclobber" => sh.st.opts.noclobber = on,
                        "posix" | "braceexpand" | "hashall" | "interactive-comments" | "monitor" | "vi"
                        | "emacs" | "history" | "allexport" | "verbose" | "errtrace" | "functrace" => {}
                        other => {
                            sh.report(fds, &format!("set: {other}: invalid option name"));
                            return 2;
                        }
                    }
                }
                other => {
                    sh.report(fds, &format!("set: -{other}: invalid option"));
                    return 2;
                }
            }
        }
        i += 1;
    }
    0
}

fn unset(sh: &mut Shell, args: &[String], fds: &Fds) -> i32 {
    let mut functions = false;
    let mut status = 0;
    for arg in args {
        match arg.as_str() {
            "-f" => functions = true,
            "-v" => functions = false,
            "-n" => {}
            name => {
                if functions {
                    sh.st.functions.remove(name);
                    continue;
                }
                if let Some((base, rest)) = name.split_once('[')
                    && let Some(key) = rest.strip_suffix(']')
                {
                    let key = key.to_string();
                    match sh.st.vars.value(base).cloned() {
                        Some(Value::Indexed(mut m)) => {
                            if let Ok(i) = sh.arith(&key, fds) {
                                m.remove(&i);
                            }
                            let _ = sh.st.vars.set(base, Value::Indexed(m));
                        }
                        Some(Value::Assoc(mut m)) => {
                            m.remove(&key);
                            let _ = sh.st.vars.set(base, Value::Assoc(m));
                        }
                        _ => {}
                    }
                    continue;
                }
                if sh.st.vars.get(name).is_none() {
                    // With no variable of that name, `unset` removes a function.
                    sh.st.functions.remove(name);
                } else if let Err(e) = sh.st.vars.unset(name) {
                    sh.report(fds, &format!("unset: {e}"));
                    status = 1;
                }
            }
        }
    }
    status
}
