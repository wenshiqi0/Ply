//! Word expansion, in bash's order: brace → tilde → parameter / command /
//! arithmetic → field splitting → pathname expansion → quote removal.
//!
//! Expansion first produces [`Piece`]s that remember whether each run of
//! text was quoted and whether it came from an expansion; splitting and
//! globbing then act only on the runs bash would touch. That bookkeeping is
//! what makes `"$@"`, `"$x"`, `$x` and `'*'` behave differently.

pub mod arith;
pub mod brace;
mod param;

use wendy_libc::glob;

use crate::exec::state::Fds;
use crate::exec::{BoxFut, Ctrl, Shell, ctrl_status};
use crate::syntax::{Part, Script, Word};

#[derive(Debug, Clone)]
pub(crate) enum Piece {
    Text { s: String, quoted: bool, split: bool },
    /// Forced field boundary (between the elements of `"$@"`).
    Break,
}

impl Piece {
    pub fn text(s: impl Into<String>, quoted: bool, split: bool) -> Piece {
        Piece::Text { s: s.into(), quoted, split }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Ctx {
    /// Inside double quotes: no splitting, no globbing.
    pub quoted: bool,
    /// Inside a `${x:-word}` word of an unquoted expansion: literal text is
    /// expansion output too, so it splits.
    pub in_param: bool,
    /// First part of a word (tilde expansion applies).
    pub word_start: bool,
}

impl Ctx {
    const TOP: Ctx = Ctx { quoted: false, in_param: false, word_start: true };
}

/// A field after splitting: runs of (text, quoted).
type Field = Vec<(String, bool)>;

impl Shell {
    /// Full expansion of command words: may yield zero or many fields.
    pub(crate) async fn expand_words(&mut self, words: &[Word], fds: &Fds) -> Result<Vec<String>, Ctrl> {
        let mut out = Vec::new();
        for word in words {
            for word in brace::expand(word) {
                let pieces = self.expand_parts(&word.0, Ctx::TOP, fds).await?;
                let ifs = self.ifs();
                for field in split_fields(pieces, &ifs) {
                    out.extend(self.glob_field(field, fds).await?);
                }
            }
        }
        Ok(out)
    }

    /// One string, no splitting or globbing (assignments, `case`, `[[ ]]`,
    /// redirection targets, here-docs).
    pub(crate) async fn expand_string(&mut self, word: &Word, fds: &Fds) -> Result<String, Ctrl> {
        let pieces = self.expand_parts(&word.0, Ctx::TOP, fds).await?;
        Ok(join_pieces(&pieces))
    }

    /// One string usable as a glob pattern: quoted text is escaped.
    pub(crate) async fn expand_pattern(&mut self, word: &Word, fds: &Fds) -> Result<String, Ctrl> {
        let pieces = self.expand_parts(&word.0, Ctx::TOP, fds).await?;
        let mut out = String::new();
        for piece in pieces {
            if let Piece::Text { s, quoted, .. } = piece {
                if quoted {
                    out.push_str(&glob::escape(&s));
                } else {
                    out.push_str(&s);
                }
            }
        }
        Ok(out)
    }

    pub(crate) fn ifs(&self) -> String {
        match self.st.vars.value("IFS") {
            Some(v) => v.scalar().unwrap_or_default(),
            None => " \t\n".into(),
        }
    }

    pub(crate) fn expand_parts<'a>(&'a mut self, parts: &'a [Part], ctx: Ctx, fds: &'a Fds) -> BoxFut<'a, Result<Vec<Piece>, Ctrl>> {
        Box::pin(async move {
            let mut out = Vec::new();
            for (i, part) in parts.iter().enumerate() {
                let ctx = Ctx { word_start: ctx.word_start && i == 0, ..ctx };
                match part {
                    Part::Lit(s) => {
                        if ctx.word_start && !ctx.quoted && s.starts_with('~') {
                            let (head, rest) = s.split_at(s.find('/').unwrap_or(s.len()));
                            match self.tilde(head) {
                                Some(home) => {
                                    out.push(Piece::text(home, true, false));
                                    if !rest.is_empty() {
                                        out.push(Piece::text(rest, false, ctx.in_param));
                                    }
                                }
                                None => out.push(Piece::text(s.clone(), ctx.quoted, ctx.in_param)),
                            }
                        } else {
                            out.push(Piece::text(s.clone(), ctx.quoted, ctx.in_param));
                        }
                    }
                    Part::Quoted(s) => out.push(Piece::text(s.clone(), true, false)),
                    Part::Double(inner) => {
                        // `""` still makes a field; only a lone "$@" may vanish.
                        if !is_lone_at(inner) {
                            out.push(Piece::text("", true, false));
                        }
                        let inner_ctx = Ctx { quoted: true, in_param: false, word_start: false };
                        out.extend(self.expand_parts(inner, inner_ctx, fds).await?);
                    }
                    Part::Param(p) => out.extend(self.expand_param(p, ctx, fds).await?),
                    Part::CmdSub(script) => {
                        let s = self.command_subst(script, fds).await?;
                        out.push(Piece::text(s, ctx.quoted, true));
                    }
                    Part::Arith(word) => {
                        let text = self.expand_string(word, fds).await?;
                        let n = self.arith(&text, fds)?;
                        out.push(Piece::text(n.to_string(), ctx.quoted, true));
                    }
                }
            }
            Ok(out)
        })
    }

    fn tilde(&self, head: &str) -> Option<String> {
        let var = |name: &str| self.st.vars.value(name).and_then(|v| v.scalar());
        match head {
            "~" => var("HOME"),
            "~+" => Some(self.st.cwd.clone()),
            "~-" => var("OLDPWD"),
            _ => None,
        }
    }

    /// `$(...)`: run in a subshell, capture stdout, drop trailing newlines.
    pub(crate) fn command_subst<'a>(&'a mut self, script: &'a Script, fds: &'a Fds) -> BoxFut<'a, Result<String, Ctrl>> {
        Box::pin(async move {
            let mut child = self.fork();
            let sub = Fds { stdin: fds.stdin.clone(), out: self.new_sink(), err: fds.err.clone() };
            let result = child.exec_script(script, &sub).await;
            let status = ctrl_status(result, &child);
            self.st.last_subst_status = Some(status);
            self.st.last_status = status;
            let mut bytes = sub.out.take();
            bytes.retain(|b| *b != 0);
            let mut s = String::from_utf8_lossy(&bytes).into_owned();
            while s.ends_with('\n') {
                s.pop();
            }
            Ok(s)
        })
    }

    async fn glob_field(&mut self, field: Field, _fds: &Fds) -> Result<Vec<String>, Ctrl> {
        let literal: String = field.iter().map(|(s, _)| s.as_str()).collect();
        if self.st.opts.noglob {
            return Ok(vec![literal]);
        }
        let mut pattern = String::new();
        for (s, quoted) in &field {
            if *quoted {
                pattern.push_str(&glob::escape(s));
            } else {
                pattern.push_str(s);
            }
        }
        if !glob::has_meta(&pattern) {
            return Ok(vec![literal]);
        }
        let fs = wendy_libc::Fs::new(self.rt.io.clone(), self.st.cwd.clone());
        let matches = glob::expand_with(&fs, &pattern, self.st.opts.dotglob).await;
        if matches.is_empty() {
            if self.st.opts.nullglob {
                return Ok(Vec::new());
            }
            return Ok(vec![literal]);
        }
        Ok(matches)
    }
}

fn is_lone_at(parts: &[Part]) -> bool {
    match parts {
        [Part::Param(p)] => {
            p.op == crate::syntax::ParamOp::None
                && !p.indirect
                && (p.name == "@"
                    || p.index.as_ref().and_then(|w| w.as_plain()).is_some_and(|i| i == "@"))
        }
        _ => false,
    }
}

pub(crate) fn join_pieces(pieces: &[Piece]) -> String {
    let mut out = String::new();
    let mut prev_break = false;
    for piece in pieces {
        match piece {
            Piece::Text { s, .. } => {
                out.push_str(s);
                prev_break = false;
            }
            Piece::Break => {
                if !prev_break {
                    out.push(' ');
                }
                prev_break = true;
            }
        }
    }
    out
}

/// IFS field splitting over expansion output.
pub(crate) fn split_fields(pieces: Vec<Piece>, ifs: &str) -> Vec<Field> {
    let mut fields: Vec<Field> = Vec::new();
    let mut cur: Field = Vec::new();
    let mut cur_exists = false;
    let finish = |fields: &mut Vec<Field>, cur: &mut Field, exists: &mut bool| {
        if *exists || !cur.is_empty() {
            fields.push(std::mem::take(cur));
        }
        *exists = false;
    };
    let push = |cur: &mut Field, c: char, quoted: bool| match cur.last_mut() {
        Some((s, q)) if *q == quoted => s.push(c),
        _ => cur.push((c.to_string(), quoted)),
    };

    for piece in pieces {
        match piece {
            Piece::Break => finish(&mut fields, &mut cur, &mut cur_exists),
            Piece::Text { s, quoted: true, .. } => {
                cur_exists = true;
                for c in s.chars() {
                    push(&mut cur, c, true);
                }
            }
            Piece::Text { s, quoted: false, split: false } => {
                if !s.is_empty() {
                    cur_exists = true;
                }
                for c in s.chars() {
                    push(&mut cur, c, false);
                }
            }
            Piece::Text { s, quoted: false, split: true } => {
                if ifs.is_empty() {
                    if !s.is_empty() {
                        cur_exists = true;
                    }
                    for c in s.chars() {
                        push(&mut cur, c, false);
                    }
                    continue;
                }
                let mut after_ws_split = false;
                for c in s.chars() {
                    if ifs.contains(c) {
                        if c.is_whitespace() {
                            if cur_exists || !cur.is_empty() {
                                finish(&mut fields, &mut cur, &mut cur_exists);
                                after_ws_split = true;
                            }
                        } else {
                            if !after_ws_split {
                                cur_exists = true;
                                finish(&mut fields, &mut cur, &mut cur_exists);
                            }
                            after_ws_split = false;
                        }
                    } else {
                        cur_exists = true;
                        after_ws_split = false;
                        push(&mut cur, c, false);
                    }
                }
            }
        }
    }
    finish(&mut fields, &mut cur, &mut cur_exists);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(fields: Vec<Field>) -> Vec<String> {
        fields.into_iter().map(|f| f.into_iter().map(|(s, _)| s).collect()).collect()
    }

    #[test]
    fn splits_only_unquoted_expansions() {
        let pieces = vec![Piece::text("a b", false, true), Piece::text("c d", true, false)];
        assert_eq!(strs(split_fields(pieces, " \t\n")), ["a", "bc d"]);
        let pieces = vec![Piece::text("  a  b  ", false, true)];
        assert_eq!(strs(split_fields(pieces, " \t\n")), ["a", "b"]);
        let pieces = vec![Piece::text("a,,b", false, true)];
        assert_eq!(strs(split_fields(pieces, ",")), ["a", "", "b"]);
        let pieces = vec![Piece::text("", true, false)];
        assert_eq!(strs(split_fields(pieces, " ")), [""]);
        let pieces = vec![Piece::text("", false, true)];
        assert!(split_fields(pieces, " ").is_empty());
        let pieces = vec![Piece::text("x", true, false), Piece::Break, Piece::text("y z", true, false)];
        assert_eq!(strs(split_fields(pieces, " ")), ["x", "y z"]);
    }
}
