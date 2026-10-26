//! Executing parsed scripts.
//!
//! Important decisions:
//! - No processes. Subshells, pipeline stages and command substitutions run
//!   on a cloned [`State`] (`fork`), so they cannot change the parent's
//!   variables or cwd, exactly like bash's forked children.
//! - Pipeline stages run one after another; each stage's whole output becomes
//!   the next stage's input. Results match bash for terminating pipelines;
//!   a producer that never ends (`yes | head`) is not supported.
//! - `set -e` follows bash: a failure is ignored inside `if`/`while`/`until`
//!   conditions, in any non-final element of `&&`/`||`, and under `!`. The
//!   interpreter tracks this as a depth counter so it also covers functions
//!   called from such a context.

pub mod state;

mod cond;
mod redirect;
pub(crate) mod simple;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ply_io::Io;

use crate::expand::arith;
use crate::syntax::{AndOr, CaseEnd, Command, Compound, Connector, Pipeline, Script, parse};
use crate::utils::Registry;
use state::{Fds, Input, Sink, State, Value, Var};

pub(crate) type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Non-local control flow travelling up the call stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ctrl {
    Break(u32),
    Continue(u32),
    Return(i32),
    Exit(i32),
    /// The current command failed; its message is already on stderr.
    Abort,
    /// A fatal expansion error (`set -u`, `${x:?}`): ends the shell. Bash
    /// reports 127 for this at the top of `bash -c` and 1 everywhere else
    /// (subshells, command substitutions).
    Fatal,
}

pub(crate) type Status = Result<i32, Ctrl>;

/// Resource ceilings for one `exec`.
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_commands: u64,
    pub max_function_depth: usize,
    pub max_buffer_bytes: Option<usize>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_commands: 1_000_000, max_function_depth: 200, max_buffer_bytes: None }
    }
}

pub(crate) struct Runtime {
    pub io: Io,
    pub utils: Registry,
    pub limits: Limits,
    pub commands: AtomicU64,
    pub session: String,
    pub output_truncated: Arc<AtomicBool>,
}

/// What one `exec` produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: i32,
    pub truncated: bool,
}

impl ExecOutput {
    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// A shell session. State (variables, cwd, functions) persists across
/// [`Shell::exec`] calls, like one long-running bash process.
pub struct Shell {
    pub(crate) st: State,
    pub(crate) rt: Arc<Runtime>,
}

static SESSIONS: AtomicU64 = AtomicU64::new(1);

impl Shell {
    /// A new session doing its IO through `io` (as an `sh` actor).
    pub fn new(io: &Io) -> Shell {
        Self::with_registry(io, Registry::standard())
    }

    pub fn with_registry(io: &Io, utils: Registry) -> Shell {
        Self::with_limits(io, utils, Limits::default())
    }

    pub fn with_limits(io: &Io, utils: Registry, limits: Limits) -> Shell {
        let session = format!("sh-{}", SESSIONS.fetch_add(1, Ordering::SeqCst));
        let io = io.clone();
        let mut st = State { arg0: "bash".into(), cwd: "/".into(), random: 0x5eed, ..State::default() };
        for (name, value) in [
            ("HOME", "/"),
            ("PWD", "/"),
            ("IFS", " \t\n"),
            ("PATH", "/usr/local/bin:/usr/bin:/bin"),
            ("SHELL", "/bin/bash"),
            ("USER", "ply"),
            ("HOSTNAME", "ply"),
            ("BASH_VERSION", "5.2.0(1)-ply"),
            ("OPTIND", "1"),
        ] {
            let exported = !matches!(name, "IFS" | "BASH_VERSION" | "OPTIND");
            st.vars.declare_global(name, Var { exported, ..Var::str(value) });
        }
        Shell {
            st,
            rt: Arc::new(Runtime {
                io,
                utils,
                limits,
                commands: AtomicU64::new(0),
                session,
                output_truncated: Arc::default(),
            }),
        }
    }

    pub fn session(&self) -> &str {
        &self.rt.session
    }

    pub fn cwd(&self) -> &str {
        &self.st.cwd
    }

    /// Change directory without running `cd` (host-side setup).
    pub fn set_cwd(&mut self, dir: &str) {
        self.st.cwd = ply_libc::path::resolve(&self.st.cwd.clone(), dir);
        let _ = self.st.vars.set_str("PWD", self.st.cwd.clone());
    }

    pub fn set_var(&mut self, name: &str, value: &str, export: bool) {
        let _ = self.st.vars.set_str(name, value);
        if export && let Some(var) = self.st.vars.get_mut(name) {
            var.exported = true;
        }
    }

    pub fn var(&self, name: &str) -> Option<String> {
        self.st.vars.value(name).and_then(Value::scalar)
    }

    pub fn set_args(&mut self, arg0: &str, args: &[String]) {
        self.st.arg0 = arg0.to_string();
        self.st.positional = args.to_vec();
    }

    pub async fn exec(&mut self, script: &str) -> ExecOutput {
        self.exec_with_stdin(script, Vec::new()).await
    }

    pub fn capture(&self, stdin: Vec<u8>) -> Fds {
        Fds { stdin: Input::new(stdin), out: self.new_sink(), err: self.new_sink() }
    }

    pub async fn exec_with_stdin(&mut self, script: &str, stdin: Vec<u8>) -> ExecOutput {
        self.exec_with_fds(script, self.capture(stdin)).await
    }

    pub async fn exec_with_fds(&mut self, script: &str, fds: Fds) -> ExecOutput {
        self.rt.output_truncated.store(false, Ordering::Relaxed);
        self.rt.commands.store(0, Ordering::SeqCst);

        let status = match parse(script) {
            Err(e) => {
                fds.err.write_str(&format!("{}: -c: line {}: {}\n", self.st.arg0, e.line, e.message));
                2
            }
            Ok(ast) => {
                let result = self.exec_script(&ast, &fds).await;
                match result {
                    Ok(s) => s,
                    Err(Ctrl::Exit(n)) | Err(Ctrl::Return(n)) => n,
                    Err(Ctrl::Break(_)) | Err(Ctrl::Continue(_)) => self.st.last_status,
                    Err(Ctrl::Abort) => 1,
                    Err(Ctrl::Fatal) => 127,
                }
            }
        };
        let status = self.run_exit_trap(&fds, status).await;
        self.st.last_status = status;

        let out = ExecOutput { stdout: fds.out.take(), stderr: fds.err.take(), status, truncated: self.rt.output_truncated.load(Ordering::Relaxed) };
        out
    }

    async fn run_exit_trap(&mut self, fds: &Fds, status: i32) -> i32 {
        let Some(trap) = self.st.exit_trap.take() else { return status };
        self.st.last_status = status;
        let Ok(ast) = parse(&trap) else { return status };
        match self.exec_script(&ast, fds).await {
            Err(Ctrl::Exit(n)) => n,
            _ => status,
        }
    }

    /// A child sharing the runtime but with its own copy of the state.
    pub(crate) fn fork(&self) -> Shell {
        let mut st = self.st.clone();
        st.exit_trap = None;
        Shell { st, rt: self.rt.clone() }
    }

    /// `bash: line N: msg` on the given stderr.
    pub(crate) fn report(&self, fds: &Fds, msg: &str) {
        fds.err.write_str(&format!("{}: line {}: {msg}\n", self.st.arg0, self.st.line));
    }

    pub(crate) fn new_sink(&self) -> Sink {
        match self.rt.limits.max_buffer_bytes {
            Some(limit) => Sink::bounded(limit, self.rt.output_truncated.clone()),
            None => Sink::new(),
        }
    }

    pub(crate) fn count_command(&self, fds: &Fds) -> Result<(), Ctrl> {
        if self.rt.output_truncated.load(Ordering::Relaxed) { return Err(Ctrl::Exit(1)); }
        let n = self.rt.commands.fetch_add(1, Ordering::SeqCst) + 1;
        if n > self.rt.limits.max_commands {
            if n == self.rt.limits.max_commands + 1 {
                self.report(fds, "command limit exceeded");
            }
            return Err(Ctrl::Exit(1));
        }
        Ok(())
    }

    pub(crate) fn exec_script<'a>(&'a mut self, script: &'a Script, fds: &'a Fds) -> BoxFut<'a, Status> {
        Box::pin(async move {
            // Even an empty loop body yields, so host cancellation and
            // wall-clock deadlines can retire the interpreter future.
            tokio::task::yield_now().await;
            let mut status = 0;
            for item in &script.0 {
                if item.background {
                    // No job control: run now, in a subshell, and report success.
                    let mut child = self.fork();
                    let result = child.exec_and_or(&item.and_or, fds).await;
                    self.st.last_bg_status = ctrl_status(result, &child);
                    status = 0;
                } else {
                    status = self.exec_and_or(&item.and_or, fds).await?;
                }
                self.st.last_status = status;
            }
            Ok(status)
        })
    }

    async fn exec_and_or(&mut self, ao: &AndOr, fds: &Fds) -> Status {
        let only = ao.rest.is_empty();
        let mut status = self.exec_list_element(&ao.first, fds, only).await?;
        for (i, (connector, pipeline)) in ao.rest.iter().enumerate() {
            let run = match connector {
                Connector::And => status == 0,
                Connector::Or => status != 0,
            };
            if run {
                let last = i + 1 == ao.rest.len();
                status = self.exec_list_element(pipeline, fds, last).await?;
            }
        }
        Ok(status)
    }

    async fn exec_list_element(&mut self, pipeline: &Pipeline, fds: &Fds, last: bool) -> Status {
        if !last {
            self.st.cond_depth += 1;
        }
        let result = self.exec_pipeline(pipeline, fds).await;
        if !last {
            self.st.cond_depth -= 1;
        }
        let status = result?;
        self.st.last_status = status;
        if last && status != 0 && !pipeline.negate && self.st.opts.errexit && self.st.cond_depth == 0 {
            return Err(Ctrl::Exit(status));
        }
        Ok(status)
    }

    async fn exec_pipeline(&mut self, pipeline: &Pipeline, fds: &Fds) -> Status {
        if pipeline.negate {
            self.st.cond_depth += 1;
        }
        let result = self.exec_pipeline_inner(pipeline, fds).await;
        if pipeline.negate {
            self.st.cond_depth -= 1;
        }
        let status = result?;
        Ok(if pipeline.negate { (status == 0) as i32 } else { status })
    }

    async fn exec_pipeline_inner(&mut self, pipeline: &Pipeline, fds: &Fds) -> Status {
        if pipeline.commands.len() == 1 {
            let status = self.exec_command(&pipeline.commands[0], fds).await?;
            self.set_pipestatus(&[status]);
            return Ok(status);
        }
        let mut input = fds.stdin.clone();
        let mut statuses = Vec::with_capacity(pipeline.commands.len());
        for (i, command) in pipeline.commands.iter().enumerate() {
            let last = i + 1 == pipeline.commands.len();
            let out = if last { fds.out.clone() } else { self.new_sink() };
            let stage = Fds { stdin: input.clone(), out: out.clone(), err: fds.err.clone() };
            let mut child = self.fork();
            let result = child.exec_command(command, &stage).await;
            statuses.push(ctrl_status(result, &child));
            if !last {
                input = Input::new(out.take());
            }
        }
        self.set_pipestatus(&statuses);
        let last = *statuses.last().unwrap_or(&0);
        Ok(if self.st.opts.pipefail { statuses.iter().rev().copied().find(|s| *s != 0).unwrap_or(0) } else { last })
    }

    fn set_pipestatus(&mut self, statuses: &[i32]) {
        let map = statuses.iter().enumerate().map(|(i, s)| (i as i64, s.to_string())).collect();
        let _ = self.st.vars.set("PIPESTATUS", Value::Indexed(map));
    }

    pub(crate) fn exec_command<'a>(&'a mut self, command: &'a Command, fds: &'a Fds) -> BoxFut<'a, Status> {
        Box::pin(async move {
            self.count_command(fds)?;
            if self.rt.commands.load(Ordering::Relaxed) % 64 == 0 { tokio::task::yield_now().await; }
            let result = match command {
                Command::Simple(simple) => self.exec_simple(simple, fds).await,
                Command::Compound { body, redirects } if redirects.is_empty() => self.exec_compound(body, fds).await,
                Command::Compound { body, redirects } => match self.open_redirects(redirects, fds).await {
                    Ok((inner, pending)) => {
                        let result = self.exec_compound(body, &inner).await;
                        self.flush_redirects(pending, fds).await;
                        result
                    }
                    Err(e) => Err(e),
                },
                Command::FunctionDef { name, body } => {
                    self.st.functions.insert(name.clone(), body.clone());
                    Ok(0)
                }
            };
            match result {
                Err(Ctrl::Abort) => Ok(1),
                other => other,
            }
        })
    }

    async fn exec_condition(&mut self, script: &Script, fds: &Fds) -> Status {
        self.st.cond_depth += 1;
        let result = self.exec_script(script, fds).await;
        self.st.cond_depth -= 1;
        result
    }

    async fn exec_compound(&mut self, body: &Compound, fds: &Fds) -> Status {
        match body {
            Compound::Group(script) => self.exec_script(script, fds).await,
            Compound::Subshell(script) => {
                let mut child = self.fork();
                let result = child.exec_script(script, fds).await;
                Ok(ctrl_status(result, &child))
            }
            Compound::If { branches, otherwise } => {
                for (cond, then) in branches {
                    if self.exec_condition(cond, fds).await? == 0 {
                        return self.exec_script(then, fds).await;
                    }
                }
                match otherwise {
                    Some(script) => self.exec_script(script, fds).await,
                    None => Ok(0),
                }
            }
            Compound::While { cond, body, until } => {
                let mut status = 0;
                self.st.loop_depth += 1;
                let result = async {
                    loop {
                        let c = self.exec_condition(cond, fds).await?;
                        if (c == 0) == *until {
                            return Ok(status);
                        }
                        match self.exec_script(body, fds).await {
                            Ok(s) => status = s,
                            Err(Ctrl::Break(n)) if n > 1 => return Err(Ctrl::Break(n - 1)),
                            Err(Ctrl::Break(_)) => return Ok(status),
                            Err(Ctrl::Continue(n)) if n > 1 => return Err(Ctrl::Continue(n - 1)),
                            Err(Ctrl::Continue(_)) => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
                .await;
                self.st.loop_depth -= 1;
                result
            }
            Compound::For { var, words, body } => {
                let items = match words {
                    Some(words) => self.expand_words(words, fds).await?,
                    None => self.st.positional.clone(),
                };
                let mut status = 0;
                self.st.loop_depth += 1;
                let result = async {
                    for item in items {
                        if let Err(e) = self.st.vars.set_str(var, item) {
                            self.report(fds, &e);
                            return Ok(1);
                        }
                        match self.exec_script(body, fds).await {
                            Ok(s) => status = s,
                            Err(Ctrl::Break(n)) if n > 1 => return Err(Ctrl::Break(n - 1)),
                            Err(Ctrl::Break(_)) => break,
                            Err(Ctrl::Continue(n)) if n > 1 => return Err(Ctrl::Continue(n - 1)),
                            Err(Ctrl::Continue(_)) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    Ok(status)
                }
                .await;
                self.st.loop_depth -= 1;
                result
            }
            Compound::ArithFor { init, cond, step, body } => {
                self.arith_word(init, fds).await?;
                let mut status = 0;
                self.st.loop_depth += 1;
                let result = async {
                    loop {
                        let empty_cond = cond.0.is_empty();
                        if !empty_cond && self.arith_word(cond, fds).await? == 0 {
                            return Ok(status);
                        }
                        match self.exec_script(body, fds).await {
                            Ok(s) => status = s,
                            Err(Ctrl::Break(n)) if n > 1 => return Err(Ctrl::Break(n - 1)),
                            Err(Ctrl::Break(_)) => return Ok(status),
                            Err(Ctrl::Continue(n)) if n > 1 => return Err(Ctrl::Continue(n - 1)),
                            Err(Ctrl::Continue(_)) => {}
                            Err(e) => return Err(e),
                        }
                        self.arith_word(step, fds).await?;
                    }
                }
                .await;
                self.st.loop_depth -= 1;
                result
            }
            Compound::Case { word, arms } => {
                let subject = self.expand_string(word, fds).await?;
                let mut fall_into = false;
                let mut status = 0;
                for arm in arms {
                    let mut hit = fall_into;
                    if !hit {
                        for pattern in &arm.patterns {
                            let pat = self.expand_pattern(pattern, fds).await?;
                            if ply_libc::glob::Pattern::new(&pat).matches(&subject) {
                                hit = true;
                                break;
                            }
                        }
                    }
                    if !hit {
                        continue;
                    }
                    status = self.exec_script(&arm.body, fds).await?;
                    match arm.end {
                        CaseEnd::Break => return Ok(status),
                        CaseEnd::FallThrough => fall_into = true,
                        CaseEnd::Continue => fall_into = false,
                    }
                }
                Ok(status)
            }
            Compound::Arith(word) => Ok((self.arith_word(word, fds).await? == 0) as i32),
            Compound::Cond(expr) => self.eval_cond(expr, fds).await,
        }
    }

    /// Expand a word and evaluate it as arithmetic; errors abort the command.
    pub(crate) async fn arith_word(&mut self, word: &crate::syntax::Word, fds: &Fds) -> Result<i64, Ctrl> {
        let text = self.expand_string(word, fds).await?;
        self.arith(&text, fds)
    }

    pub(crate) fn arith(&mut self, text: &str, fds: &Fds) -> Result<i64, Ctrl> {
        let mut vars = ArithState { st: &mut self.st, depth: 0 };
        arith::eval(text, &mut vars).map_err(|e| {
            self.report(fds, &format!("{}: {e}", text.trim()));
            Ctrl::Abort
        })
    }
}

/// The status a finished child (subshell, pipeline stage) reports upward.
pub(crate) fn ctrl_status(result: Status, child: &Shell) -> i32 {
    match result {
        Ok(s) => s,
        Err(Ctrl::Exit(n)) | Err(Ctrl::Return(n)) => n,
        Err(Ctrl::Break(_)) | Err(Ctrl::Continue(_)) => child.st.last_status,
        Err(Ctrl::Abort) | Err(Ctrl::Fatal) => 1,
    }
}

/// Variables as seen by the arithmetic evaluator.
pub(crate) struct ArithState<'a> {
    pub st: &'a mut State,
    pub depth: usize,
}

impl arith::ArithVars for ArithState<'_> {
    fn get(&mut self, name: &str, index: Option<i64>) -> Result<i64, String> {
        let raw = match (self.st.vars.value(name), index) {
            (None, _) => {
                if self.st.opts.nounset {
                    return Err(format!("{name}: unbound variable"));
                }
                return Ok(0);
            }
            (Some(Value::Indexed(m)), Some(i)) => m.get(&i).cloned().unwrap_or_default(),
            (Some(Value::Assoc(m)), Some(i)) => m.get(&i.to_string()).cloned().unwrap_or_default(),
            (Some(v), _) => v.scalar().unwrap_or_default(),
        };
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(0);
        }
        if let Ok(n) = raw.parse::<i64>() {
            return Ok(n);
        }
        if self.depth > 32 {
            return Err("expression recursion level exceeded".into());
        }
        self.depth += 1;
        let raw = raw.to_string();
        let result = arith::eval(&raw, self);
        self.depth -= 1;
        result
    }

    fn set(&mut self, name: &str, index: Option<i64>, value: i64) -> Result<(), String> {
        match index {
            None => match self.st.vars.value(name) {
                Some(Value::Indexed(m)) => {
                    let mut m = m.clone();
                    m.insert(0, value.to_string());
                    self.st.vars.set(name, Value::Indexed(m))
                }
                _ => self.st.vars.set_str(name, value.to_string()),
            },
            Some(i) => match self.st.vars.value(name).cloned() {
                Some(Value::Assoc(mut m)) => {
                    m.insert(i.to_string(), value.to_string());
                    self.st.vars.set(name, Value::Assoc(m))
                }
                Some(Value::Indexed(mut m)) => {
                    m.insert(i, value.to_string());
                    self.st.vars.set(name, Value::Indexed(m))
                }
                Some(Value::Str(s)) => {
                    let mut m = std::collections::BTreeMap::new();
                    m.insert(0, s);
                    m.insert(i, value.to_string());
                    self.st.vars.set(name, Value::Indexed(m))
                }
                None => {
                    let mut m = std::collections::BTreeMap::new();
                    m.insert(i, value.to_string());
                    self.st.vars.set(name, Value::Indexed(m))
                }
            },
        }
    }
}
