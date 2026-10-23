//! One simple command: expand, assign, redirect, then run a function, a
//! builtin or a utility.

use std::sync::Arc;

use serde_json::json;
use wendy_libc::Fs;
use wendy_trace::{Scope, Verdict};

use super::state::{Fds, Value, Var};
use super::{Ctrl, Shell, Status};
use crate::builtins::{self, DeclArg};
use crate::syntax::{Assign, AssignValue, Command, DeclItem, Part, SimpleCommand};
use crate::utils::UtilCtx;

/// An assignment value after expansion, before attributes (`-a`, `-A`,
/// `-i`) decide what it becomes.
#[derive(Debug, Clone)]
pub(crate) enum RawValue {
    Scalar(String),
    Array(Vec<ArrayElem>),
}

#[derive(Debug, Clone)]
pub(crate) enum ArrayElem {
    Plain(String),
    Keyed(String, String),
}

impl Shell {
    pub(crate) async fn exec_simple(&mut self, cmd: &SimpleCommand, fds: &Fds) -> Status {
        self.st.line = cmd.line;
        self.st.last_subst_status = None;
        let argv = self.expand_words(&cmd.words, fds).await?;

        if argv.is_empty() {
            for assign in &cmd.assigns {
                let key = match &assign.index {
                    Some(w) => Some(self.expand_string(w, fds).await?),
                    None => None,
                };
                let value = self.assign_value(assign, fds).await?;
                if let Err(e) = self.store(assign, key.as_deref(), value, fds) {
                    self.report(fds, &e);
                    return Err(Ctrl::Abort);
                }
            }
            if self.st.opts.xtrace && !cmd.assigns.is_empty() {
                let shown: Vec<String> = cmd.assigns.iter().map(|a| a.name.clone()).collect();
                fds.err.write_str(&format!("+ {}\n", shown.join(" ")));
            }
            let (_, pending) = self.open_redirects(&cmd.redirects, fds).await?;
            self.flush_redirects(pending, fds).await;
            return Ok(self.st.last_subst_status.unwrap_or(0));
        }

        if let Some(items) = &cmd.decl
            && argv.len() == 1
            && builtins::is_declaration(&argv[0])
            && !self.st.functions.contains_key(&argv[0])
        {
            let mut args = Vec::new();
            for item in items {
                match item {
                    DeclItem::Word(w) => {
                        for field in self.expand_words(std::slice::from_ref(w), fds).await? {
                            args.push(DeclArg::Plain(field));
                        }
                    }
                    DeclItem::Assign(a) => {
                        let key = match &a.index {
                            Some(w) => Some(self.expand_string(w, fds).await?),
                            None => None,
                        };
                        let value = self.raw_assign_value(a, fds).await?;
                        args.push(DeclArg::Assign { name: a.name.clone(), key, value, append: a.append });
                    }
                }
            }
            if self.st.opts.xtrace {
                fds.err.write_str(&format!("+ {}\n", argv[0]));
            }
            let (rfds, pending) = self.open_redirects(&cmd.redirects, fds).await?;
            let result = builtins::declaration(self, &argv[0], args, &rfds).await;
            self.flush_redirects(pending, fds).await;
            return result;
        }

        // Prefix assignments live in a temporary, exported scope.
        let mut temp = Vec::new();
        for assign in &cmd.assigns {
            let value = self.assign_value(assign, fds).await?;
            temp.push((assign.name.clone(), value));
        }

        if self.st.opts.xtrace {
            let shown: Vec<String> = argv.iter().map(|a| quote_for_trace(a)).collect();
            fds.err.write_str(&format!("+ {}\n", shown.join(" ")));
        }

        let (rfds, pending) = self.open_redirects(&cmd.redirects, fds).await?;

        let has_temp = !temp.is_empty();
        if has_temp {
            self.st.vars.push_scope();
            for (name, value) in temp {
                self.st.vars.declare_local(&name, Var { value: Some(value), exported: true, readonly: false, integer: false });
            }
        }
        let result = self.dispatch(&argv, &rfds).await;
        if has_temp {
            self.st.vars.pop_scope();
        }
        self.flush_redirects(pending, fds).await;
        result
    }

    /// Run `argv` as a function, builtin or utility (in that order).
    pub(crate) async fn dispatch(&mut self, argv: &[String], fds: &Fds) -> Status {
        let name = argv[0].as_str();
        let tracer = self.rt.io.tracer().clone();
        let kind;
        let result = if let Some(body) = self.st.functions.get(name).cloned() {
            kind = "function";
            self.call_function(body, argv, fds).await
        } else if builtins::is_builtin(name) {
            kind = "builtin";
            builtins::run(self, argv, fds).await
        } else if let Some(util) = self.rt.utils.get(name) {
            kind = "utility";
            let env = self.st.vars.exported();
            let fs = Fs::new(self.rt.io.labeled(name), self.st.cwd.clone());
            let ctx = UtilCtx { name, args: &argv[1..], fds, fs: &fs, env: &env };
            Ok(util.run(ctx).await)
        } else {
            kind = "missing";
            self.report(fds, &format!("{name}: command not found"));
            Ok(127)
        };
        let status = match &result {
            Ok(s) => Some(*s),
            Err(Ctrl::Exit(n)) | Err(Ctrl::Return(n)) => Some(*n),
            _ => None,
        };
        tracer.emit(
            Scope::Command,
            "cmd.run",
            if kind == "missing" { Verdict::Error } else { Verdict::Info },
            json!({
                "session": self.rt.session,
                "argv": argv.iter().take(32).map(|a| a.chars().take(256).collect::<String>()).collect::<Vec<_>>(),
                "kind": kind,
                "status": status,
                "line": self.st.line,
            }),
        );
        result
    }

    pub(crate) async fn call_function(&mut self, body: Arc<Command>, argv: &[String], fds: &Fds) -> Status {
        if self.st.func_depth >= self.rt.limits.max_function_depth {
            self.report(fds, &format!("{}: maximum function nesting level exceeded ({})", argv[0], self.st.func_depth));
            return Err(Ctrl::Abort);
        }
        let saved = std::mem::replace(&mut self.st.positional, argv[1..].to_vec());
        self.st.func_depth += 1;
        self.st.vars.push_scope();
        let result = self.exec_command(&body, fds).await;
        self.st.vars.pop_scope();
        self.st.func_depth -= 1;
        self.st.positional = saved;
        match result {
            Err(Ctrl::Return(n)) => Ok(n),
            other => other,
        }
    }

    /// The value an assignment would store (expanded, not yet stored).
    pub(crate) async fn assign_value(&mut self, assign: &Assign, fds: &Fds) -> Result<Value, Ctrl> {
        Ok(match &assign.value {
            AssignValue::Scalar(word) => {
                let text = self.expand_string(word, fds).await?;
                let integer = self.st.vars.get(&assign.name).is_some_and(|v| v.integer);
                if integer {
                    let mut n = self.arith(&text, fds)?;
                    if assign.append {
                        let current = self.st.vars.value(&assign.name).and_then(Value::scalar).unwrap_or_default();
                        n += self.arith(&current, fds)?;
                    }
                    Value::Str(n.to_string())
                } else {
                    Value::Str(text)
                }
            }
            AssignValue::Array(_) => {
                let assoc = matches!(self.st.vars.value(&assign.name), Some(Value::Assoc(_)));
                let RawValue::Array(elems) = self.raw_assign_value(assign, fds).await? else {
                    unreachable!("array assignment yields array elements")
                };
                self.build_array(elems, assoc, fds)?
            }
        })
    }

    /// An assignment's expanded value before attributes give it a type.
    pub(crate) async fn raw_assign_value(&mut self, assign: &Assign, fds: &Fds) -> Result<RawValue, Ctrl> {
        Ok(match &assign.value {
            AssignValue::Scalar(word) => RawValue::Scalar(self.expand_string(word, fds).await?),
            AssignValue::Array(words) => {
                let mut elems = Vec::new();
                for word in words {
                    // `[key]=value` elements are recognized from the source text.
                    let keyed = matches!(word.0.first(), Some(Part::Lit(s)) if s.starts_with('['));
                    if keyed {
                        let text = self.expand_string(word, fds).await?;
                        if let Some((key, value)) = text[1..].split_once("]=") {
                            elems.push(ArrayElem::Keyed(key.to_string(), value.to_string()));
                            continue;
                        }
                        elems.push(ArrayElem::Plain(text));
                        continue;
                    }
                    for field in self.expand_words(std::slice::from_ref(word), fds).await? {
                        elems.push(ArrayElem::Plain(field));
                    }
                }
                RawValue::Array(elems)
            }
        })
    }

    pub(crate) fn build_array(&mut self, elems: Vec<ArrayElem>, assoc: bool, fds: &Fds) -> Result<Value, Ctrl> {
        if assoc {
            let mut map = std::collections::BTreeMap::new();
            for elem in elems {
                match elem {
                    ArrayElem::Keyed(k, v) => {
                        map.insert(k, v);
                    }
                    ArrayElem::Plain(p) => {
                        self.report(fds, &format!("{p}: must use subscript when assigning associative array"));
                    }
                }
            }
            return Ok(Value::Assoc(map));
        }
        let mut items = std::collections::BTreeMap::new();
        let mut next = 0i64;
        for elem in elems {
            match elem {
                ArrayElem::Keyed(k, v) => {
                    let i = self.arith(&k, fds)?;
                    items.insert(i, v);
                    next = i + 1;
                }
                ArrayElem::Plain(v) => {
                    items.insert(next, v);
                    next += 1;
                }
            }
        }
        Ok(Value::Indexed(items))
    }

    /// Store an assignment's value in the shell (honoring `+=` and `[i]=`).
    /// `key` is the already-expanded subscript of `name[key]=...`.
    pub(crate) fn store(&mut self, assign: &Assign, key: Option<&str>, value: Value, fds: &Fds) -> Result<(), String> {
        let name = assign.name.as_str();
        if let Some(key) = key {
            let mut text = value.scalar().unwrap_or_default();
            if assign.append {
                let current = match self.st.vars.value(name).cloned() {
                    Some(Value::Assoc(m)) => m.get(key).cloned(),
                    Some(Value::Indexed(m)) => self.arith(key, fds).ok().and_then(|i| m.get(&i).cloned()),
                    _ => None,
                };
                text = current.unwrap_or_default() + &text;
            }
            return self.assign_element(name, key, text, fds);
        }
        if !assign.append {
            return self.st.vars.set(name, value);
        }
        let merged = match (self.st.vars.value(name).cloned(), value) {
            (Some(Value::Indexed(mut m)), Value::Indexed(add)) => {
                let start = m.keys().next_back().map_or(0, |k| k + 1);
                for (next, (_, v)) in (start..).zip(add) {
                    m.insert(next, v);
                }
                Value::Indexed(m)
            }
            (Some(Value::Assoc(mut m)), Value::Assoc(add)) => {
                m.extend(add);
                Value::Assoc(m)
            }
            (Some(Value::Str(s)), Value::Indexed(add)) => {
                let mut m: std::collections::BTreeMap<i64, String> = std::iter::once((0, s)).collect();
                for (next, (_, v)) in (1..).zip(add) {
                    m.insert(next, v);
                }
                Value::Indexed(m)
            }
            (Some(Value::Indexed(mut m)), Value::Str(s)) => {
                let first = m.get(&0).cloned().unwrap_or_default();
                m.insert(0, first + &s);
                Value::Indexed(m)
            }
            (Some(v), Value::Str(s)) => {
                let integer = self.st.vars.get(name).is_some_and(|v| v.integer);
                if integer { Value::Str(s) } else { Value::Str(v.scalar().unwrap_or_default() + &s) }
            }
            (_, v) => v,
        };
        self.st.vars.set(name, merged)
    }
}

fn quote_for_trace(arg: &str) -> String {
    if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c)) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}
