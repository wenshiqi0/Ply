//! Interpreter state: variables (with dynamic scoping), functions, options,
//! and the in-process stdin/stdout/stderr streams.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use crate::syntax::Command;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Indexed(BTreeMap<i64, String>),
    Assoc(BTreeMap<String, String>),
}

impl Value {
    /// What `$name` gives: the string, or element 0 / key "0" of an array.
    pub fn scalar(&self) -> Option<String> {
        match self {
            Value::Str(s) => Some(s.clone()),
            Value::Indexed(m) => m.get(&0).cloned(),
            Value::Assoc(m) => m.get("0").cloned(),
        }
    }

    pub fn elements(&self) -> Vec<String> {
        match self {
            Value::Str(s) => vec![s.clone()],
            Value::Indexed(m) => m.values().cloned().collect(),
            Value::Assoc(m) => m.values().cloned().collect(),
        }
    }

    pub fn keys(&self) -> Vec<String> {
        match self {
            Value::Str(_) => vec!["0".into()],
            Value::Indexed(m) => m.keys().map(|k| k.to_string()).collect(),
            Value::Assoc(m) => m.keys().cloned().collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Var {
    /// `None`: declared but unset (`declare x`, `local x`).
    pub value: Option<Value>,
    pub exported: bool,
    pub readonly: bool,
    pub integer: bool,
}

impl Var {
    pub fn str(s: impl Into<String>) -> Self {
        Var { value: Some(Value::Str(s.into())), exported: false, readonly: false, integer: false }
    }
}

/// Globals plus a stack of function-local scopes. Lookup walks from the
/// innermost scope outwards (bash's dynamic scoping).
#[derive(Debug, Clone, Default)]
pub struct Vars {
    global: HashMap<String, Var>,
    scopes: Vec<HashMap<String, Var>>,
}

impl Vars {
    pub fn get(&self, name: &str) -> Option<&Var> {
        self.scopes.iter().rev().find_map(|s| s.get(name)).or_else(|| self.global.get(name))
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Var> {
        for scope in self.scopes.iter_mut().rev() {
            if scope.contains_key(name) {
                return scope.get_mut(name);
            }
        }
        self.global.get_mut(name)
    }

    pub fn value(&self, name: &str) -> Option<&Value> {
        self.get(name).and_then(|v| v.value.as_ref())
    }

    /// Set (creating in the global scope if new). Fails on readonly.
    pub fn set(&mut self, name: &str, value: Value) -> Result<(), String> {
        if let Some(var) = self.get_mut(name) {
            if var.readonly {
                return Err(format!("{name}: readonly variable"));
            }
            var.value = Some(value);
            return Ok(());
        }
        self.global.insert(
            name.to_string(),
            Var { value: Some(value), exported: false, readonly: false, integer: false },
        );
        Ok(())
    }

    pub fn set_str(&mut self, name: &str, value: impl Into<String>) -> Result<(), String> {
        self.set(name, Value::Str(value.into()))
    }

    /// Declare in the innermost scope (function locals, temp env).
    pub fn declare_local(&mut self, name: &str, var: Var) {
        match self.scopes.last_mut() {
            Some(scope) => scope.insert(name.to_string(), var),
            None => self.global.insert(name.to_string(), var),
        };
    }

    pub fn declare_global(&mut self, name: &str, var: Var) {
        self.global.insert(name.to_string(), var);
    }

    pub fn is_local(&self, name: &str) -> bool {
        self.scopes.last().is_some_and(|s| s.contains_key(name))
    }

    pub fn unset(&mut self, name: &str) -> Result<(), String> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(var) = scope.get_mut(name) {
                if var.readonly {
                    return Err(format!("{name}: cannot unset: readonly variable"));
                }
                // A local stays declared but unset, shadowing outer values.
                var.value = None;
                return Ok(());
            }
        }
        if self.global.get(name).is_some_and(|v| v.readonly) {
            return Err(format!("{name}: cannot unset: readonly variable"));
        }
        self.global.remove(name);
        Ok(())
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub fn in_function_scope(&self) -> bool {
        !self.scopes.is_empty()
    }

    /// Exported variables, innermost definition winning.
    pub fn exported(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        let layers = std::iter::once(&self.global).chain(self.scopes.iter());
        for layer in layers {
            for (name, var) in layer {
                match (&var.value, var.exported) {
                    (Some(v), true) => {
                        out.insert(name.clone(), v.scalar().unwrap_or_default());
                    }
                    _ => {
                        out.remove(name);
                    }
                }
            }
        }
        // Re-apply: a later non-exported shadow removed the outer export above.
        out
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.global.keys().cloned().collect();
        for scope in &self.scopes {
            names.extend(scope.keys().cloned());
        }
        names.sort();
        names.dedup();
        names
    }
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub errexit: bool,
    pub nounset: bool,
    pub pipefail: bool,
    pub xtrace: bool,
    pub noglob: bool,
    pub noclobber: bool,
    pub nullglob: bool,
    pub dotglob: bool,
    pub failglob: bool,
}

impl Options {
    /// `$-`
    pub fn flags(&self) -> String {
        let mut s = String::new();
        if self.errexit {
            s.push('e');
        }
        if self.noglob {
            s.push('f');
        }
        s.push('h');
        if self.nounset {
            s.push('u');
        }
        if self.xtrace {
            s.push('x');
        }
        if self.noclobber {
            s.push('C');
        }
        s.push('B');
        s
    }
}

#[derive(Clone, Default)]
pub struct State {
    pub vars: Vars,
    pub functions: HashMap<String, Arc<Command>>,
    pub positional: Vec<String>,
    pub arg0: String,
    pub cwd: String,
    pub opts: Options,
    pub last_status: i32,
    /// Status of the last command substitution in the current command.
    pub last_subst_status: Option<i32>,
    /// > 0 while running a context where `set -e` is ignored.
    pub cond_depth: usize,
    pub func_depth: usize,
    pub loop_depth: usize,
    pub line: usize,
    pub exit_trap: Option<String>,
    pub random: u32,
    pub last_bg_status: i32,
    pub source_depth: usize,
    /// `getopts` position inside a group of short options (`-abc`).
    pub getopts_pos: usize,
}

/// An output stream. Writes from several fds pointing at the same sink stay
/// in order, which is what `2>&1` needs.
#[derive(Clone, Default)]
pub struct Sink(Arc<Mutex<Vec<u8>>>);

impl Sink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write(&self, data: &[u8]) {
        self.0.lock().expect("sink poisoned").extend_from_slice(data);
    }

    pub fn write_str(&self, s: &str) {
        self.write(s.as_bytes());
    }

    pub fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.0.lock().expect("sink poisoned"))
    }

    pub fn same(&self, other: &Sink) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// An input stream with a shared read position: `while read l; do ...; done`
/// and the commands inside the loop all consume the same input.
#[derive(Clone, Default)]
pub struct Input(Arc<Mutex<(Vec<u8>, usize)>>);

impl Input {
    pub fn new(data: Vec<u8>) -> Self {
        Input(Arc::new(Mutex::new((data, 0))))
    }

    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    /// Everything not yet consumed.
    pub fn read_all(&self) -> Vec<u8> {
        let mut guard = self.0.lock().expect("input poisoned");
        let (data, pos) = &mut *guard;
        let rest = data[*pos..].to_vec();
        *pos = data.len();
        rest
    }

    /// Bytes up to and including `delim`; `None` at end of input.
    pub fn read_until(&self, delim: u8) -> Option<Vec<u8>> {
        let mut guard = self.0.lock().expect("input poisoned");
        let (data, pos) = &mut *guard;
        if *pos >= data.len() {
            return None;
        }
        let end = data[*pos..].iter().position(|b| *b == delim).map_or(data.len(), |i| *pos + i + 1);
        let chunk = data[*pos..end].to_vec();
        *pos = end;
        Some(chunk)
    }

    pub fn read_n(&self, n: usize) -> Option<Vec<u8>> {
        let mut guard = self.0.lock().expect("input poisoned");
        let (data, pos) = &mut *guard;
        if *pos >= data.len() {
            return None;
        }
        let end = (*pos + n).min(data.len());
        let chunk = data[*pos..end].to_vec();
        *pos = end;
        Some(chunk)
    }
}

/// The three standard streams a command runs with.
#[derive(Clone, Default)]
pub struct Fds {
    pub stdin: Input,
    pub out: Sink,
    pub err: Sink,
}
