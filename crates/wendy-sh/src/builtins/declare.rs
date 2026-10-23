//! `declare` / `typeset` / `local` / `export` / `readonly`.

use std::collections::BTreeMap;

use crate::exec::simple::{ArrayElem, RawValue};
use crate::exec::state::{Fds, Value, Var};
use crate::exec::{Ctrl, Shell, Status};
use crate::syntax::is_name;

pub(crate) enum DeclArg {
    Plain(String),
    Assign { name: String, key: Option<String>, value: RawValue, append: bool },
}

impl DeclArg {
    /// Interpret an already-expanded word (`command export A=1`).
    pub(crate) fn from_plain(word: &str) -> DeclArg {
        if let Some((lhs, value)) = word.split_once('=') {
            let (lhs, append) = match lhs.strip_suffix('+') {
                Some(l) => (l, true),
                None => (lhs, false),
            };
            let (name, key) = match lhs.split_once('[') {
                Some((n, rest)) if rest.ends_with(']') => (n, Some(rest[..rest.len() - 1].to_string())),
                _ => (lhs, None),
            };
            if is_name(name) {
                return DeclArg::Assign { name: name.into(), key, value: RawValue::Scalar(value.into()), append };
            }
        }
        DeclArg::Plain(word.to_string())
    }
}

pub(crate) fn is_declaration(name: &str) -> bool {
    crate::syntax::DECLARATION_BUILTINS.contains(&name)
}

#[derive(Default)]
struct Flags {
    array: bool,
    assoc: bool,
    integer: bool,
    export: Option<bool>,
    readonly: bool,
    global: bool,
    print: bool,
    functions: bool,
    function_names: bool,
}

pub(crate) async fn declaration(sh: &mut Shell, cmd: &str, items: Vec<DeclArg>, fds: &Fds) -> Status {
    let mut flags = Flags::default();
    match cmd {
        "export" => flags.export = Some(true),
        "readonly" => flags.readonly = true,
        _ => {}
    }
    let mut targets = Vec::new();
    let mut options_done = false;
    for item in items {
        match item {
            DeclArg::Plain(p) if !options_done && (p.starts_with('-') || p.starts_with('+')) && p.len() > 1 => {
                if p == "--" {
                    options_done = true;
                    continue;
                }
                let on = p.starts_with('-');
                for c in p[1..].chars() {
                    match c {
                        'a' => flags.array = on,
                        'A' => flags.assoc = on,
                        'i' => flags.integer = on,
                        'x' => flags.export = Some(on),
                        'r' => flags.readonly = on,
                        'g' => flags.global = on,
                        'p' => flags.print = true,
                        'f' => flags.functions = true,
                        'F' => flags.function_names = true,
                        'n' if cmd == "export" => flags.export = Some(false),
                        'l' | 'u' | 't' | 'n' => {}
                        other => {
                            sh.report(fds, &format!("{cmd}: -{other}: invalid option"));
                            return Ok(2);
                        }
                    }
                }
            }
            other => {
                options_done = true;
                targets.push(other);
            }
        }
    }

    if cmd == "local" && !sh.st.vars.in_function_scope() {
        sh.report(fds, "local: can only be used in a function");
        return Ok(1);
    }

    if flags.functions || flags.function_names {
        let mut names: Vec<&String> = sh.st.functions.keys().collect();
        names.sort();
        for name in names {
            fds.out.write_str(&format!("declare -f {name}\n"));
        }
        return Ok(0);
    }

    if targets.is_empty() || (flags.print && targets.iter().all(|t| matches!(t, DeclArg::Plain(_)))) {
        return Ok(print(sh, cmd, &flags, &targets, fds));
    }

    let local = cmd == "local" || (matches!(cmd, "declare" | "typeset") && sh.st.vars.in_function_scope() && !flags.global);
    let mut status = 0;
    for target in targets {
        let (name, key, value, append) = match target {
            DeclArg::Plain(name) => (name, None, None, false),
            DeclArg::Assign { name, key, value, append } => (name, key, Some(value), append),
        };
        if !is_name(&name) {
            sh.report(fds, &format!("{cmd}: `{name}': not a valid identifier"));
            status = 1;
            continue;
        }
        if sh.st.vars.get(&name).is_some_and(|v| v.readonly) && (value.is_some() || flags.readonly) {
            sh.report(fds, &format!("{name}: readonly variable"));
            status = 1;
            continue;
        }

        // Make sure the variable exists in the right scope.
        if local && !sh.st.vars.is_local(&name) {
            sh.st.vars.declare_local(&name, Var { value: None, exported: false, readonly: false, integer: false });
        } else if !local && sh.st.vars.get(&name).is_none() {
            sh.st.vars.declare_global(&name, Var { value: None, exported: false, readonly: false, integer: false });
        }
        if flags.assoc && !matches!(sh.st.vars.value(&name), Some(Value::Assoc(_))) {
            set_value(sh, &name, Value::Assoc(BTreeMap::new()));
        } else if flags.array && !matches!(sh.st.vars.value(&name), Some(Value::Indexed(_)) | Some(Value::Assoc(_))) {
            let existing = sh.st.vars.value(&name).and_then(Value::scalar);
            let map = existing.map(|s| std::iter::once((0, s)).collect()).unwrap_or_default();
            set_value(sh, &name, Value::Indexed(map));
        }
        if let Some(var) = sh.st.vars.get_mut(&name) {
            if flags.integer {
                var.integer = true;
            }
            if let Some(export) = flags.export {
                var.exported = export;
            }
        }

        if let Some(raw) = value {
            let integer = sh.st.vars.get(&name).is_some_and(|v| v.integer);
            let assoc = matches!(sh.st.vars.value(&name), Some(Value::Assoc(_)));
            let result: Result<(), Ctrl> = match (raw, key) {
                (RawValue::Scalar(text), Some(key)) => {
                    let text = if integer { sh.arith(&text, fds)?.to_string() } else { text };
                    sh.assign_element(&name, &key, text, fds).map_err(|e| {
                        sh.report(fds, &e);
                        Ctrl::Abort
                    })
                }
                (RawValue::Scalar(text), None) => {
                    let mut text = if integer { sh.arith(&text, fds)?.to_string() } else { text };
                    if append {
                        let current = sh.st.vars.value(&name).and_then(Value::scalar).unwrap_or_default();
                        text = if integer {
                            (sh.arith(&current, fds)? + text.parse::<i64>().unwrap_or(0)).to_string()
                        } else {
                            current + &text
                        };
                    }
                    let value = match sh.st.vars.value(&name).cloned() {
                        Some(Value::Indexed(mut m)) => {
                            m.insert(0, text);
                            Value::Indexed(m)
                        }
                        Some(Value::Assoc(mut m)) => {
                            m.insert("0".into(), text);
                            Value::Assoc(m)
                        }
                        _ => Value::Str(text),
                    };
                    set_value(sh, &name, value);
                    Ok(())
                }
                (RawValue::Array(elems), _) => {
                    let elems = if integer {
                        let mut out = Vec::new();
                        for e in elems {
                            out.push(match e {
                                ArrayElem::Plain(v) => ArrayElem::Plain(sh.arith(&v, fds)?.to_string()),
                                ArrayElem::Keyed(k, v) => ArrayElem::Keyed(k, sh.arith(&v, fds)?.to_string()),
                            });
                        }
                        out
                    } else {
                        elems
                    };
                    let built = sh.build_array(elems, assoc, fds)?;
                    let merged = match (append, sh.st.vars.value(&name).cloned(), built) {
                        (true, Some(Value::Indexed(mut m)), Value::Indexed(add)) => {
                            let start = m.keys().next_back().map_or(0, |k| k + 1);
                            for (next, (_, v)) in (start..).zip(add) {
                                m.insert(next, v);
                            }
                            Value::Indexed(m)
                        }
                        (true, Some(Value::Assoc(mut m)), Value::Assoc(add)) => {
                            m.extend(add);
                            Value::Assoc(m)
                        }
                        (_, _, built) => built,
                    };
                    set_value(sh, &name, merged);
                    Ok(())
                }
            };
            if let Err(e) = result {
                if e == Ctrl::Abort {
                    status = 1;
                    continue;
                }
                return Err(e);
            }
        }
        if flags.readonly
            && let Some(var) = sh.st.vars.get_mut(&name)
        {
            var.readonly = true;
        }
    }
    Ok(status)
}

/// Set without the readonly check (attributes were handled by the caller).
fn set_value(sh: &mut Shell, name: &str, value: Value) {
    if let Some(var) = sh.st.vars.get_mut(name) {
        var.value = Some(value);
    } else {
        let _ = sh.st.vars.set(name, value);
    }
}

fn print(sh: &mut Shell, cmd: &str, flags: &Flags, targets: &[DeclArg], fds: &Fds) -> i32 {
    let names: Vec<String> = if targets.is_empty() {
        sh.st.vars.names()
    } else {
        targets.iter().filter_map(|t| if let DeclArg::Plain(n) = t { Some(n.clone()) } else { None }).collect()
    };
    let mut status = 0;
    for name in names {
        let Some(var) = sh.st.vars.get(&name).cloned() else {
            if !targets.is_empty() {
                sh.report(fds, &format!("{cmd}: {name}: not found"));
                status = 1;
            }
            continue;
        };
        if targets.is_empty() {
            let wanted = match cmd {
                "export" => var.exported,
                "readonly" => var.readonly,
                _ => (!flags.readonly || var.readonly) && (flags.export != Some(true) || var.exported),
            };
            if !wanted {
                continue;
            }
        }
        fds.out.write_str(&format!("{}\n", describe(&name, &var)));
    }
    status
}

/// `declare -p` form of a variable.
pub(crate) fn describe(name: &str, var: &Var) -> String {
    let mut attrs = String::new();
    match &var.value {
        Some(Value::Indexed(_)) => attrs.push('a'),
        Some(Value::Assoc(_)) => attrs.push('A'),
        _ => {}
    }
    if var.integer {
        attrs.push('i');
    }
    if var.readonly {
        attrs.push('r');
    }
    if var.exported {
        attrs.push('x');
    }
    let attrs = if attrs.is_empty() { "--".to_string() } else { format!("-{attrs}") };
    match &var.value {
        None => format!("declare {attrs} {name}"),
        Some(Value::Str(s)) => format!("declare {attrs} {name}={}", dquote(s)),
        Some(Value::Indexed(m)) => {
            let body: Vec<String> = m.iter().map(|(k, v)| format!("[{k}]={}", dquote(v))).collect();
            format!("declare {attrs} {name}=({})", body.join(" "))
        }
        Some(Value::Assoc(m)) => {
            let body: Vec<String> = m.iter().map(|(k, v)| format!("[{k}]={} ", dquote(v))).collect();
            format!("declare {attrs} {name}=({})", body.concat())
        }
    }
}

fn dquote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Quote a value the way `set` prints it.
pub(crate) fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "_-./:,@%+=".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}
