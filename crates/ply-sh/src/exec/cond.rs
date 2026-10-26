//! `[[ ... ]]` and the `test` / `[` builtin. File tests are IO (stat calls),
//! so they go through the mounted IO layer.

use ply_io::EntryKind;
use ply_libc::Fs;
use ply_libc::glob::Pattern;

use super::state::{Fds, Value};
use super::{BoxFut, Ctrl, Shell, Status};
use crate::expand::Piece;
use crate::syntax::{CondExpr, Word};

impl Shell {
    pub(crate) fn eval_cond<'a>(&'a mut self, expr: &'a CondExpr, fds: &'a Fds) -> BoxFut<'a, Status> {
        Box::pin(async move {
            match self.cond_bool(expr, fds).await {
                Ok(b) => Ok((!b) as i32),
                Err(Ctrl::Abort) => Ok(2),
                Err(e) => Err(e),
            }
        })
    }

    fn cond_bool<'a>(&'a mut self, expr: &'a CondExpr, fds: &'a Fds) -> BoxFut<'a, Result<bool, Ctrl>> {
        Box::pin(async move {
            Ok(match expr {
                CondExpr::Word(w) => !self.expand_string(w, fds).await?.is_empty(),
                CondExpr::Not(e) => !self.cond_bool(e, fds).await?,
                CondExpr::And(a, b) => self.cond_bool(a, fds).await? && self.cond_bool(b, fds).await?,
                CondExpr::Or(a, b) => self.cond_bool(a, fds).await? || self.cond_bool(b, fds).await?,
                CondExpr::Unary(op, w) => {
                    let arg = self.expand_string(w, fds).await?;
                    self.test_unary(op, &arg).await
                }
                CondExpr::Binary(l, op, r) => {
                    let left = self.expand_string(l, fds).await?;
                    match op.as_str() {
                        "==" | "=" | "!=" => {
                            let pat = self.expand_pattern(r, fds).await?;
                            Pattern::new(&pat).matches(&left) == (op != "!=")
                        }
                        "=~" => {
                            let re = self.expand_regex(r, fds).await?;
                            match regex::Regex::new(&re) {
                                Ok(re) => match re.captures(&left) {
                                    Some(caps) => {
                                        let groups = caps
                                            .iter()
                                            .enumerate()
                                            .map(|(i, m)| (i as i64, m.map_or(String::new(), |m| m.as_str().to_string())))
                                            .collect();
                                        let _ = self.st.vars.set("BASH_REMATCH", Value::Indexed(groups));
                                        true
                                    }
                                    None => {
                                        let _ = self.st.vars.set("BASH_REMATCH", Value::Indexed(Default::default()));
                                        false
                                    }
                                },
                                Err(_) => return Err(Ctrl::Abort),
                            }
                        }
                        "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" => {
                            let a = self.arith(&left, fds)?;
                            let right = self.expand_string(r, fds).await?;
                            let b = self.arith(&right, fds)?;
                            compare_ints(op, a, b)
                        }
                        _ => {
                            let right = self.expand_string(r, fds).await?;
                            self.test_binary(op, &left, &right).await.map_err(|e| {
                                self.report(fds, &e);
                                Ctrl::Abort
                            })?
                        }
                    }
                }
            })
        })
    }

    /// Like `expand_pattern`, but quoted text is escaped for a regex.
    async fn expand_regex(&mut self, word: &Word, fds: &Fds) -> Result<String, Ctrl> {
        let pieces = self.expand_parts(&word.0, crate::expand::Ctx { quoted: false, in_param: false, word_start: true }, fds).await?;
        let mut out = String::new();
        for piece in pieces {
            if let Piece::Text { s, quoted, .. } = piece {
                out.push_str(&if quoted { regex::escape(&s) } else { s });
            }
        }
        Ok(out)
    }

    fn test_fs(&self) -> Fs {
        Fs::new(self.rt.io.clone(), self.st.cwd.clone())
    }

    pub(crate) async fn test_unary(&mut self, op: &str, arg: &str) -> bool {
        let fs = self.test_fs();
        match op {
            "-z" => arg.is_empty(),
            "-n" => !arg.is_empty(),
            "-e" | "-a" | "-r" | "-w" | "-O" | "-G" => fs.exists(arg).await,
            "-f" => fs.is_file(arg).await,
            "-d" => fs.is_dir(arg).await,
            "-x" => fs.is_dir(arg).await,
            "-s" => fs.stat(arg).await.is_ok_and(|e| e.kind == EntryKind::Dir || e.size > 0),
            "-v" => {
                let (name, index) = match arg.split_once('[') {
                    Some((n, rest)) => (n, rest.strip_suffix(']')),
                    None => (arg, None),
                };
                match (self.st.vars.value(name), index) {
                    (None, _) => false,
                    (Some(_), None) => true,
                    (Some(Value::Indexed(m)), Some(i)) => i.parse().is_ok_and(|i: i64| m.contains_key(&i)),
                    (Some(Value::Assoc(m)), Some(k)) => m.contains_key(k),
                    (Some(Value::Str(_)), Some(i)) => i == "0",
                }
            }
            "-o" => match arg {
                "errexit" => self.st.opts.errexit,
                "nounset" => self.st.opts.nounset,
                "pipefail" => self.st.opts.pipefail,
                "xtrace" => self.st.opts.xtrace,
                "noglob" => self.st.opts.noglob,
                "noclobber" => self.st.opts.noclobber,
                _ => false,
            },
            // No symlinks, devices, pipes, sockets or terminals in this world.
            _ => false,
        }
    }

    pub(crate) async fn test_binary(&mut self, op: &str, a: &str, b: &str) -> Result<bool, String> {
        Ok(match op {
            "=" | "==" => a == b,
            "!=" => a != b,
            "<" => a < b,
            ">" => a > b,
            "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" => {
                let parse = |s: &str| {
                    s.trim().parse::<i64>().map_err(|_| format!("{s}: integer expression expected"))
                };
                compare_ints(op, parse(a)?, parse(b)?)
            }
            "-nt" | "-ot" => {
                let fs = self.test_fs();
                let (ma, mb) = (fs.stat(a).await.ok().map(|e| e.modified), fs.stat(b).await.ok().map(|e| e.modified));
                match (ma, mb, op) {
                    (Some(x), Some(y), "-nt") => x > y,
                    (Some(_), None, "-nt") => true,
                    (Some(x), Some(y), _) => x < y,
                    (None, Some(_), _) => true,
                    _ => false,
                }
            }
            "-ef" => {
                let fs = self.test_fs();
                fs.exists(a).await && fs.resolve(a) == fs.resolve(b)
            }
            _ => return Err(format!("{op}: binary operator expected")),
        })
    }

    /// POSIX `test` over already-expanded arguments.
    pub(crate) async fn run_test(&mut self, args: &[String]) -> Result<bool, String> {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        match a.as_slice() {
            [] => Ok(false),
            [x] => Ok(!x.is_empty()),
            ["!", x] => Ok(x.is_empty()),
            [op, x] if is_unary(op) => Ok(self.test_unary(op, x).await),
            [op, _] => Err(format!("{op}: unary operator expected")),
            [x, op, y] if is_binary(op) => self.test_binary(op, x, y).await,
            ["!", rest @ ..] if rest.len() == 2 => Ok(!Box::pin(self.run_test(&args[1..])).await?),
            ["(", x, ")"] => Ok(!x.is_empty()),
            _ => {
                let mut p = TestParser { args: &a, pos: 0 };
                let tree = p.or()?;
                if p.pos != a.len() {
                    return Err(format!("{}: too many arguments", a[p.pos]));
                }
                self.eval_test_tree(&tree).await
            }
        }
    }

    fn eval_test_tree<'a>(&'a mut self, tree: &'a TestTree) -> BoxFut<'a, Result<bool, String>> {
        Box::pin(async move {
            Ok(match tree {
                TestTree::Str(s) => !s.is_empty(),
                TestTree::Unary(op, x) => self.test_unary(op, x).await,
                TestTree::Binary(x, op, y) => self.test_binary(op, x, y).await?,
                TestTree::Not(t) => !self.eval_test_tree(t).await?,
                TestTree::And(a, b) => self.eval_test_tree(a).await? && self.eval_test_tree(b).await?,
                TestTree::Or(a, b) => self.eval_test_tree(a).await? || self.eval_test_tree(b).await?,
            })
        })
    }
}

fn compare_ints(op: &str, a: i64, b: i64) -> bool {
    match op {
        "-eq" => a == b,
        "-ne" => a != b,
        "-lt" => a < b,
        "-le" => a <= b,
        "-gt" => a > b,
        _ => a >= b,
    }
}

fn is_unary(op: &str) -> bool {
    op.len() == 2 && op.starts_with('-') && "abcdefghknoprstuvwxzGLNOS".contains(&op[1..])
}

fn is_binary(op: &str) -> bool {
    matches!(op, "=" | "==" | "!=" | "<" | ">" | "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" | "-nt" | "-ot" | "-ef")
}

enum TestTree {
    Str(String),
    Unary(String, String),
    Binary(String, String, String),
    Not(Box<TestTree>),
    And(Box<TestTree>, Box<TestTree>),
    Or(Box<TestTree>, Box<TestTree>),
}

struct TestParser<'a> {
    args: &'a [&'a str],
    pos: usize,
}

impl<'a> TestParser<'a> {
    fn peek(&self, n: usize) -> Option<&'a str> {
        self.args.get(self.pos + n).copied()
    }

    fn or(&mut self) -> Result<TestTree, String> {
        let mut left = self.and()?;
        while self.peek(0) == Some("-o") {
            self.pos += 1;
            left = TestTree::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<TestTree, String> {
        let mut left = self.not()?;
        while self.peek(0) == Some("-a") {
            self.pos += 1;
            left = TestTree::And(Box::new(left), Box::new(self.not()?));
        }
        Ok(left)
    }

    fn not(&mut self) -> Result<TestTree, String> {
        if self.peek(0) == Some("!") && self.peek(1).is_some() {
            self.pos += 1;
            return Ok(TestTree::Not(Box::new(self.not()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<TestTree, String> {
        let Some(first) = self.peek(0) else { return Err("argument expected".into()) };
        if first == "(" {
            self.pos += 1;
            let inner = self.or()?;
            if self.peek(0) != Some(")") {
                return Err("`)' expected".into());
            }
            self.pos += 1;
            return Ok(inner);
        }
        if let Some(op) = self.peek(1)
            && is_binary(op)
            && self.peek(2).is_some()
        {
            let tree = TestTree::Binary(first.into(), op.into(), self.peek(2).unwrap_or_default().into());
            self.pos += 3;
            return Ok(tree);
        }
        if is_unary(first)
            && let Some(arg) = self.peek(1)
        {
            self.pos += 2;
            return Ok(TestTree::Unary(first.into(), arg.into()));
        }
        self.pos += 1;
        Ok(TestTree::Str(first.into()))
    }
}
