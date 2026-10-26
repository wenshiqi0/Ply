//! Recursive-descent parser for bash syntax, working directly on characters.
//!
//! Bash's grammar is context sensitive (keywords only count in command
//! position, `}` only closes a group when it stands alone, here-doc bodies
//! start after the *line* that mentions them), so there is no separate token
//! stream: each grammar rule reads the characters it needs.
//!
//! Here-docs: when `<<DELIM` is parsed, its body is read immediately from the
//! line after the current one, and `heredoc_end` remembers where the body
//! ends. The next newline the parser consumes jumps over the body.

use std::sync::Arc;

use super::ast::*;

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

type PResult<T> = Result<T, ParseError>;

pub fn parse(src: &str) -> PResult<Script> {
    let mut p = Parser::new(src);
    let script = p.parse_list(&Stop::Eof)?;
    p.skip_linebreaks();
    if let Some(c) = p.peek() {
        return Err(p.unexpected_char(c));
    }
    Ok(script)
}

/// Parse text with double-quote rules (here-doc bodies, arithmetic).
pub fn parse_dq_text(src: &str, drop_dquotes: bool) -> PResult<Word> {
    let mut p = Parser::new(src);
    let parts = p.parse_dq_parts(None, drop_dquotes)?;
    Ok(Word(parts))
}

/// What ends the list being parsed.
enum Stop {
    Eof,
    Keywords(&'static [&'static str]),
    /// `)` (subshell, `$(...)`)
    Paren,
    /// `}` group
    Brace,
    /// case arm body: `;;`, `;&`, `;;&` or `esac`
    CaseArm,
}

const META: &[char] = &[' ', '\t', '\n', ';', '&', '|', '<', '>', '(', ')'];

struct Parser {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    heredoc_end: Option<usize>,
    nesting: usize,
}

fn is_name_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

fn is_name_char(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

pub fn is_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(is_name_start) && chars.all(is_name_char)
}

impl Parser {
    fn new(src: &str) -> Self {
        Parser { chars: src.chars().collect(), pos: 0, line: 1, heredoc_end: None, nesting: 0 }
    }

    // ---------- character helpers ----------

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.pos + n).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn starts_with(&self, s: &str) -> bool {
        s.chars().enumerate().all(|(i, c)| self.peek_at(i) == Some(c))
    }

    fn eat(&mut self, s: &str) -> bool {
        if self.starts_with(s) {
            for _ in s.chars() {
                self.bump();
            }
            true
        } else {
            false
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.chars.len()
    }

    fn err<T>(&self, message: impl Into<String>) -> PResult<T> {
        Err(ParseError { line: self.line, message: message.into() })
    }

    fn unexpected_char(&self, c: char) -> ParseError {
        let token = match c {
            '\n' => "newline".to_string(),
            _ => {
                let rest: String = self.chars[self.pos..].iter().take(3).collect();
                [";;&", ";;", ";&", "&&", "||", ">>", "<<"]
                    .iter()
                    .find(|op| rest.starts_with(**op))
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| c.to_string())
            }
        };
        ParseError { line: self.line, message: format!("syntax error near unexpected token `{token}'") }
    }

    /// Spaces, tabs, line continuations and comments (not the newline).
    fn skip_blanks(&mut self) {
        loop {
            match self.peek() {
                Some(' ') | Some('\t') => {
                    self.bump();
                }
                Some('\\') if self.peek_at(1) == Some('\n') => {
                    self.bump();
                    self.bump();
                }
                Some('#') => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.bump();
                    }
                }
                _ => break,
            }
        }
    }

    /// Consume one newline, jumping over any here-doc bodies it introduces.
    fn newline(&mut self) -> bool {
        if self.peek() != Some('\n') {
            return false;
        }
        self.bump();
        if let Some(end) = self.heredoc_end.take()
            && end > self.pos
        {
            while self.pos < end {
                self.bump();
            }
        }
        true
    }

    fn skip_linebreaks(&mut self) {
        loop {
            self.skip_blanks();
            if !self.newline() {
                break;
            }
        }
    }

    /// The bare word at the cursor if it is followed by a delimiter (used to
    /// recognize keywords without consuming them).
    fn peek_bare_word(&self) -> Option<String> {
        let mut i = self.pos;
        let mut word = String::new();
        while let Some(&c) = self.chars.get(i) {
            if META.contains(&c) || matches!(c, '"' | '\'' | '\\' | '$' | '`') {
                break;
            }
            word.push(c);
            i += 1;
        }
        if word.is_empty() { None } else { Some(word) }
    }

    fn at_keyword(&self, kw: &str) -> bool {
        self.peek_bare_word().as_deref() == Some(kw)
    }

    fn eat_keyword(&mut self, kw: &str) -> bool {
        if self.at_keyword(kw) {
            for _ in kw.chars() {
                self.bump();
            }
            true
        } else {
            false
        }
    }

    fn expect_keyword(&mut self, kw: &str) -> PResult<()> {
        self.skip_linebreaks();
        if self.eat_keyword(kw) {
            Ok(())
        } else if let Some(c) = self.peek() {
            let found = self.peek_bare_word().unwrap_or_else(|| c.to_string());
            self.err(format!("syntax error near unexpected token `{found}' (expected `{kw}')"))
        } else {
            self.err(format!("syntax error: unexpected end of file (expected `{kw}')"))
        }
    }

    fn at_stop(&self, stop: &Stop) -> bool {
        match stop {
            Stop::Eof => self.peek() == Some(')') && false,
            Stop::Keywords(kws) => kws.iter().any(|kw| self.at_keyword(kw)),
            Stop::Paren => self.peek() == Some(')'),
            Stop::Brace => self.at_keyword("}"),
            Stop::CaseArm => {
                self.starts_with(";;") || self.starts_with(";&") || self.at_keyword("esac")
            }
        }
    }

    // ---------- lists, pipelines, commands ----------

    fn parse_list(&mut self, stop: &Stop) -> PResult<Script> {
        let mut items = Vec::new();
        loop {
            self.skip_linebreaks();
            if self.at_end() || self.at_stop(stop) {
                break;
            }
            if let Some(c) = self.peek()
                && matches!(c, ')' | ';' | '&' | '|')
            {
                return Err(self.unexpected_char(c));
            }
            let and_or = self.parse_and_or()?;
            self.skip_blanks();
            let mut background = false;
            let mut separated = true;
            match self.peek() {
                Some('&') if self.peek_at(1) != Some('&') => {
                    self.bump();
                    background = true;
                }
                Some(';') if !matches!(self.peek_at(1), Some(';') | Some('&')) => {
                    self.bump();
                }
                Some('\n') => {
                    self.newline();
                }
                _ => separated = false,
            }
            items.push(ListItem { and_or, background });
            if !separated {
                self.skip_blanks();
                if !(self.at_end() || self.at_stop(stop)) {
                    let c = self.peek().unwrap_or(' ');
                    return Err(self.unexpected_char(c));
                }
                break;
            }
        }
        Ok(Script(items))
    }

    fn parse_and_or(&mut self) -> PResult<AndOr> {
        let first = self.parse_pipeline()?;
        let mut rest = Vec::new();
        loop {
            self.skip_blanks();
            let connector = if self.eat("&&") {
                Connector::And
            } else if self.eat("||") {
                Connector::Or
            } else {
                break;
            };
            self.skip_linebreaks();
            rest.push((connector, self.parse_pipeline()?));
        }
        Ok(AndOr { first, rest })
    }

    fn parse_pipeline(&mut self) -> PResult<Pipeline> {
        self.skip_blanks();
        // `time` is accepted and ignored (no timing report).
        if self.eat_keyword("time") {
            self.skip_blanks();
        }
        let mut negate = false;
        while self.eat_keyword("!") {
            negate = !negate;
            self.skip_blanks();
        }
        let mut commands = vec![self.parse_command()?];
        loop {
            self.skip_blanks();
            if self.peek() == Some('|') && self.peek_at(1) != Some('|') {
                self.bump();
                let both = self.peek() == Some('&');
                if both {
                    self.bump();
                    // `a |& b` == `a 2>&1 | b`
                    if let Some(last) = commands.last_mut() {
                        push_redirect(last, Redirect { fd: Some(2), kind: RedirKind::DupOut, target: Word::lit("1") });
                    }
                }
                self.skip_linebreaks();
                commands.push(self.parse_command()?);
            } else {
                break;
            }
        }
        Ok(Pipeline { negate, commands })
    }

    fn parse_command(&mut self) -> PResult<Command> {
        if self.nesting >= 64 { return self.err("maximum shell syntax nesting exceeded (64)"); }
        self.nesting += 1;
        let result = self.parse_command_inner();
        self.nesting -= 1;
        result
    }

    fn parse_command_inner(&mut self) -> PResult<Command> {
        self.skip_blanks();
        let compound = if self.at_keyword("{") {
            self.bump();
            let body = self.required_list(&Stop::Brace)?;
            self.expect_keyword("}")?;
            Some(Compound::Group(body))
        } else if self.peek() == Some('(') {
            if self.peek_at(1) == Some('(') {
                self.pos += 2;
                let text = self.read_until_double_paren()?;
                Some(Compound::Arith(parse_dq_text(&text, true).map_err(|e| self.relocate(e))?))
            } else {
                self.bump();
                let body = self.required_list(&Stop::Paren)?;
                self.skip_linebreaks();
                if !self.eat(")") {
                    return self.err("syntax error: expected `)'");
                }
                Some(Compound::Subshell(body))
            }
        } else if self.eat_keyword("if") {
            Some(self.parse_if()?)
        } else if self.eat_keyword("while") {
            Some(self.parse_while(false)?)
        } else if self.eat_keyword("until") {
            Some(self.parse_while(true)?)
        } else if self.eat_keyword("for") {
            Some(self.parse_for()?)
        } else if self.eat_keyword("case") {
            Some(self.parse_case()?)
        } else if self.eat_keyword("[[") {
            let expr = self.parse_cond_or()?;
            self.skip_cond_space();
            if !self.eat_keyword("]]") {
                return self.err("syntax error in conditional expression: expected `]]'");
            }
            Some(Compound::Cond(expr))
        } else if self.eat_keyword("function") {
            self.skip_blanks();
            let name = self.peek_bare_word().filter(|w| !w.is_empty());
            let Some(name) = name else { return self.err("syntax error: expected function name") };
            self.pos += name.chars().count();
            self.skip_blanks();
            if self.eat("(") {
                self.skip_blanks();
                if !self.eat(")") {
                    return self.err("syntax error: expected `)' after `('");
                }
            }
            return self.parse_function_body(name);
        } else {
            for kw in ["then", "else", "elif", "fi", "do", "done", "esac", "}", "in", "]]"] {
                if self.at_keyword(kw) {
                    return self.err(format!("syntax error near unexpected token `{kw}'"));
                }
            }
            None
        };

        match compound {
            Some(body) => {
                let redirects = self.parse_trailing_redirects()?;
                Ok(Command::Compound { body, redirects })
            }
            None => self.parse_simple(),
        }
    }

    fn parse_function_body(&mut self, name: String) -> PResult<Command> {
        self.skip_linebreaks();
        let body = self.parse_command()?;
        if !matches!(body, Command::Compound { .. }) {
            return self.err(format!("syntax error: function `{name}' needs a compound command body"));
        }
        Ok(Command::FunctionDef { name, body: Arc::new(body) })
    }

    fn parse_trailing_redirects(&mut self) -> PResult<Vec<Redirect>> {
        let mut redirects = Vec::new();
        loop {
            self.skip_blanks();
            match self.try_redirect()? {
                Some(r) => redirects.push(r),
                None => break,
            }
        }
        Ok(redirects)
    }

    fn at_command_end(&self) -> bool {
        match self.peek() {
            None | Some('\n') | Some(';') | Some('|') | Some(')') => true,
            Some('&') => self.peek_at(1) != Some('>'),
            _ => false,
        }
    }

    fn parse_simple(&mut self) -> PResult<Command> {
        let line = self.line;
        let mut assigns = Vec::new();
        let mut words: Vec<Word> = Vec::new();
        let mut redirects = Vec::new();
        let mut decl: Option<Vec<DeclItem>> = None;
        loop {
            self.skip_blanks();
            if self.at_command_end() {
                break;
            }
            if let Some(r) = self.try_redirect()? {
                redirects.push(r);
                continue;
            }
            if words.is_empty()
                && let Some(a) = self.try_assignment()?
            {
                assigns.push(a);
                continue;
            }
            if let Some(items) = decl.as_mut() {
                match self.try_assignment()? {
                    Some(a) => items.push(DeclItem::Assign(a)),
                    None => items.push(DeclItem::Word(self.parse_word()?)),
                }
                continue;
            }
            if self.peek() == Some('(') {
                // `name () { ...; }`
                if words.len() == 1 && assigns.is_empty() && redirects.is_empty()
                    && let Some(name) = words[0].as_plain().map(str::to_string)
                {
                    self.bump();
                    self.skip_blanks();
                    if !self.eat(")") {
                        return self.err("syntax error near unexpected token `('");
                    }
                    return self.parse_function_body(name);
                }
                return self.err("syntax error near unexpected token `('");
            }
            let start = self.pos;
            let word = self.parse_word()?;
            if self.pos == start {
                let c = self.peek().unwrap_or(' ');
                return Err(self.unexpected_char(c));
            }
            if words.is_empty() && word.as_plain().is_some_and(|w| DECLARATION_BUILTINS.contains(&w)) {
                decl = Some(Vec::new());
            }
            words.push(word);
        }
        if assigns.is_empty() && words.is_empty() && redirects.is_empty() {
            return match self.peek() {
                Some(c) => Err(self.unexpected_char(c)),
                None => self.err("syntax error: unexpected end of file"),
            };
        }
        Ok(Command::Simple(SimpleCommand { assigns, words, redirects, line, decl }))
    }

    /// A list that bash requires to be non-empty (conditions, bodies).
    fn required_list(&mut self, stop: &Stop) -> PResult<Script> {
        let list = self.parse_list(stop)?;
        if list.0.is_empty() {
            self.skip_linebreaks();
            return match self.peek() {
                Some(c) => {
                    let token = self.peek_bare_word().unwrap_or_else(|| c.to_string());
                    self.err(format!("syntax error near unexpected token `{token}'"))
                }
                None => self.err("syntax error: unexpected end of file"),
            };
        }
        Ok(list)
    }

    fn parse_if(&mut self) -> PResult<Compound> {
        let mut branches = Vec::new();
        let mut otherwise = None;
        let cond = self.required_list(&Stop::Keywords(&["then"]))?;
        self.expect_keyword("then")?;
        let body = self.required_list(&Stop::Keywords(&["elif", "else", "fi"]))?;
        branches.push((cond, body));
        loop {
            self.skip_linebreaks();
            if self.eat_keyword("elif") {
                let cond = self.required_list(&Stop::Keywords(&["then"]))?;
                self.expect_keyword("then")?;
                let body = self.required_list(&Stop::Keywords(&["elif", "else", "fi"]))?;
                branches.push((cond, body));
            } else if self.eat_keyword("else") {
                otherwise = Some(self.required_list(&Stop::Keywords(&["fi"]))?);
                self.expect_keyword("fi")?;
                break;
            } else {
                self.expect_keyword("fi")?;
                break;
            }
        }
        Ok(Compound::If { branches, otherwise })
    }

    fn parse_while(&mut self, until: bool) -> PResult<Compound> {
        let cond = self.required_list(&Stop::Keywords(&["do"]))?;
        self.expect_keyword("do")?;
        let body = self.required_list(&Stop::Keywords(&["done"]))?;
        self.expect_keyword("done")?;
        Ok(Compound::While { cond, body, until })
    }

    fn parse_for(&mut self) -> PResult<Compound> {
        self.skip_blanks();
        if self.eat("((") {
            let text = self.read_until_double_paren()?;
            let parts: Vec<&str> = text.splitn(3, ';').collect();
            if parts.len() != 3 {
                return self.err("syntax error: expected `for (( init; cond; step ))'");
            }
            let w = |s: &str| parse_dq_text(s, true);
            let (init, cond, step) = (w(parts[0]), w(parts[1]), w(parts[2]));
            let (init, cond, step) = (
                init.map_err(|e| self.relocate(e))?,
                cond.map_err(|e| self.relocate(e))?,
                step.map_err(|e| self.relocate(e))?,
            );
            self.skip_blanks();
            self.eat(";");
            self.skip_linebreaks();
            let body = self.parse_do_body()?;
            return Ok(Compound::ArithFor { init, cond, step, body });
        }
        let var = match self.peek_bare_word() {
            Some(w) if is_name(&w) => w,
            _ => return self.err("syntax error: expected a variable name after `for'"),
        };
        self.pos += var.chars().count();
        self.skip_linebreaks();
        let words = if self.eat_keyword("in") {
            let mut words = Vec::new();
            loop {
                self.skip_blanks();
                if self.at_command_end() {
                    break;
                }
                words.push(self.parse_word()?);
            }
            if self.peek() == Some(';') {
                self.bump();
            }
            Some(words)
        } else {
            if self.peek() == Some(';') {
                self.bump();
            }
            None
        };
        self.skip_linebreaks();
        let body = self.parse_do_body()?;
        Ok(Compound::For { var, words, body })
    }

    fn parse_do_body(&mut self) -> PResult<Script> {
        if self.at_keyword("{") {
            // `for x in a; { ...; }` (bash extension)
            self.bump();
            let body = self.parse_list(&Stop::Brace)?;
            self.expect_keyword("}")?;
            return Ok(body);
        }
        self.expect_keyword("do")?;
        let body = self.required_list(&Stop::Keywords(&["done"]))?;
        self.expect_keyword("done")?;
        Ok(body)
    }

    fn parse_case(&mut self) -> PResult<Compound> {
        self.skip_blanks();
        let word = self.parse_word()?;
        self.skip_linebreaks();
        self.expect_keyword("in")?;
        let mut arms = Vec::new();
        loop {
            self.skip_linebreaks();
            if self.eat_keyword("esac") {
                break;
            }
            self.eat("(");
            let mut patterns = Vec::new();
            loop {
                self.skip_blanks();
                patterns.push(self.parse_word()?);
                self.skip_blanks();
                if !self.eat("|") {
                    break;
                }
            }
            if !self.eat(")") {
                return self.err("syntax error in case pattern: expected `)'");
            }
            let body = self.parse_list(&Stop::CaseArm)?;
            self.skip_linebreaks();
            let end = if self.eat(";;&") {
                CaseEnd::Continue
            } else if self.eat(";;") {
                CaseEnd::Break
            } else if self.eat(";&") {
                CaseEnd::FallThrough
            } else if self.at_keyword("esac") {
                CaseEnd::Break
            } else {
                return self.err("syntax error in case: expected `;;' or `esac'");
            };
            arms.push(CaseArm { patterns, body, end });
        }
        Ok(Compound::Case { word, arms })
    }

    // ---------- [[ ]] ----------

    fn skip_cond_space(&mut self) {
        loop {
            self.skip_blanks();
            if !self.newline() {
                break;
            }
        }
    }

    fn parse_cond_or(&mut self) -> PResult<CondExpr> {
        let mut left = self.parse_cond_and()?;
        loop {
            self.skip_cond_space();
            if self.eat("||") {
                let right = self.parse_cond_and()?;
                left = CondExpr::Or(Box::new(left), Box::new(right));
            } else {
                return Ok(left);
            }
        }
    }

    fn parse_cond_and(&mut self) -> PResult<CondExpr> {
        let mut left = self.parse_cond_not()?;
        loop {
            self.skip_cond_space();
            if self.eat("&&") {
                let right = self.parse_cond_not()?;
                left = CondExpr::And(Box::new(left), Box::new(right));
            } else {
                return Ok(left);
            }
        }
    }

    fn parse_cond_not(&mut self) -> PResult<CondExpr> {
        self.skip_cond_space();
        if self.eat_keyword("!") {
            return Ok(CondExpr::Not(Box::new(self.parse_cond_not()?)));
        }
        self.parse_cond_primary()
    }

    fn cond_word(&mut self) -> PResult<Word> {
        self.skip_cond_space();
        if self.at_keyword("]]") || self.at_end() {
            return self.err("syntax error in conditional expression");
        }
        self.parse_word()
    }

    fn parse_cond_primary(&mut self) -> PResult<CondExpr> {
        self.skip_cond_space();
        if self.eat("(") {
            let inner = self.parse_cond_or()?;
            self.skip_cond_space();
            if !self.eat(")") {
                return self.err("syntax error in conditional expression: expected `)'");
            }
            return Ok(inner);
        }
        let first = self.cond_word()?;
        if let Some(op) = first.as_plain()
            && op.len() == 2
            && op.starts_with('-')
            && UNARY_TESTS.contains(&op)
        {
            self.skip_cond_space();
            if !self.at_keyword("]]") && !self.starts_with("&&") && !self.starts_with("||") {
                let operand = self.cond_word()?;
                return Ok(CondExpr::Unary(op.to_string(), operand));
            }
        }
        self.skip_blanks();
        let op = if self.eat("<") {
            Some("<".to_string())
        } else if self.eat(">") {
            Some(">".to_string())
        } else {
            match self.peek_bare_word() {
                Some(w) if BINARY_TESTS.contains(&w.as_str()) => {
                    self.pos += w.chars().count();
                    Some(w)
                }
                _ => None,
            }
        };
        match op {
            Some(op) => {
                let right = if op == "=~" { self.parse_regex_word()? } else { self.cond_word()? };
                Ok(CondExpr::Binary(first, op, right))
            }
            None => Ok(CondExpr::Word(first)),
        }
    }

    /// The right side of `=~`: like a word, but `(`, `)` and `|` are regex
    /// characters, not shell operators.
    fn parse_regex_word(&mut self) -> PResult<Word> {
        self.skip_blanks();
        let mut parts = Vec::new();
        let mut depth = 0usize;
        loop {
            match self.peek() {
                None => break,
                Some(' ') | Some('\t') | Some('\n') if depth == 0 => break,
                Some('(') => {
                    depth += 1;
                    self.bump();
                    push_lit(&mut parts, '(');
                }
                Some(')') if depth > 0 => {
                    depth -= 1;
                    self.bump();
                    push_lit(&mut parts, ')');
                }
                Some(c) if depth == 0 && (c == ')' || (c == '&' && self.peek_at(1) == Some('&'))) => break,
                Some('|') => {
                    self.bump();
                    push_lit(&mut parts, '|');
                }
                Some(_) => {
                    if !self.parse_word_piece(&mut parts, &[' ', '\t', '\n', '(', ')', '|'])? {
                        break;
                    }
                }
            }
        }
        Ok(Word(parts))
    }

    // ---------- redirections & assignments ----------

    fn try_redirect(&mut self) -> PResult<Option<Redirect>> {
        let save = (self.pos, self.line);
        let mut fd = None;
        let mut digits = String::new();
        while let Some(c) = self.peek().filter(|c| c.is_ascii_digit()) {
            digits.push(c);
            self.bump();
        }
        if !digits.is_empty() {
            if matches!(self.peek(), Some('<') | Some('>')) {
                fd = digits.parse().ok();
            } else {
                (self.pos, self.line) = save;
                return Ok(None);
            }
        }
        let kind = if fd.is_none() && self.eat("&>>") {
            RedirKind::AppendErr
        } else if fd.is_none() && self.eat("&>") {
            RedirKind::OutErr
        } else if self.eat("<<<") {
            RedirKind::HereString
        } else if self.starts_with("<<") {
            self.pos += 2;
            let strip = self.eat("-");
            return self.parse_heredoc(fd, strip).map(Some);
        } else if self.eat("<>") {
            RedirKind::ReadWrite
        } else if self.eat("<&") {
            RedirKind::DupIn
        } else if self.eat("<") {
            RedirKind::In
        } else if self.eat(">>") {
            RedirKind::Append
        } else if self.eat(">&") {
            RedirKind::DupOut
        } else if self.eat(">|") {
            RedirKind::Clobber
        } else if self.eat(">") {
            RedirKind::Out
        } else {
            (self.pos, self.line) = save;
            return Ok(None);
        };
        self.skip_blanks();
        if self.at_command_end() || matches!(self.peek(), Some('<') | Some('>')) {
            let c = self.peek().unwrap_or('\n');
            return Err(self.unexpected_char(c));
        }
        let target = self.parse_word()?;
        Ok(Some(Redirect { fd, kind, target }))
    }

    fn parse_heredoc(&mut self, fd: Option<u32>, strip_tabs: bool) -> PResult<Redirect> {
        self.skip_blanks();
        let delim_word = self.parse_word()?;
        let quoted = delim_word.0.iter().any(|p| !matches!(p, Part::Lit(_)));
        let delim: String = delim_word.0.iter().map(raw_text).collect();

        let len = self.chars.len();
        let start = match self.heredoc_end {
            Some(end) => end,
            None => match self.chars[self.pos..].iter().position(|c| *c == '\n') {
                Some(i) => self.pos + i + 1,
                None => len,
            },
        };
        let mut i = start;
        let mut body = String::new();
        let mut terminated = false;
        while i < len {
            let line_end = self.chars[i..].iter().position(|c| *c == '\n').map_or(len, |n| i + n);
            let mut line: String = self.chars[i..line_end].iter().collect();
            if strip_tabs {
                line = line.trim_start_matches('\t').to_string();
            }
            i = (line_end + 1).min(len);
            if line == delim {
                terminated = true;
                break;
            }
            body.push_str(&line);
            body.push('\n');
        }
        let _ = terminated; // bash warns but accepts an unterminated here-doc
        self.heredoc_end = Some(i);

        let target = if quoted {
            Word(vec![Part::Quoted(body)])
        } else {
            parse_dq_text(&body, false).map_err(|e| self.relocate(e))?
        };
        Ok(Redirect { fd, kind: RedirKind::HereDoc, target })
    }

    fn try_assignment(&mut self) -> PResult<Option<Assign>> {
        let save = (self.pos, self.line);
        let mut name = String::new();
        if !self.peek().is_some_and(is_name_start) {
            return Ok(None);
        }
        while let Some(c) = self.peek().filter(|c| is_name_char(*c)) {
            name.push(c);
            self.bump();
        }
        let index = if self.peek() == Some('[') {
            self.bump();
            let w = self.parse_word_until(&[']'])?;
            if !self.eat("]") {
                (self.pos, self.line) = save;
                return Ok(None);
            }
            Some(w)
        } else {
            None
        };
        let append = if self.eat("+=") {
            true
        } else if self.eat("=") {
            false
        } else {
            (self.pos, self.line) = save;
            return Ok(None);
        };
        let value = if self.peek() == Some('(') && index.is_none() {
            self.bump();
            let mut items = Vec::new();
            loop {
                self.skip_linebreaks();
                if self.eat(")") {
                    break;
                }
                if self.at_end() {
                    return self.err("syntax error: unterminated array assignment");
                }
                items.push(self.parse_word()?);
            }
            AssignValue::Array(items)
        } else if self.at_command_end() || matches!(self.peek(), Some(' ') | Some('\t')) {
            AssignValue::Scalar(Word::default())
        } else {
            AssignValue::Scalar(self.parse_word()?)
        };
        Ok(Some(Assign { name, index, value, append }))
    }

    // ---------- words ----------

    pub(crate) fn parse_word(&mut self) -> PResult<Word> {
        self.parse_word_until(META)
    }

    /// Parse word parts until an unquoted character from `stops`.
    fn parse_word_until(&mut self, stops: &[char]) -> PResult<Word> {
        if self.nesting >= 64 { return self.err("maximum shell syntax nesting exceeded (64)"); }
        self.nesting += 1;
        let result = self.parse_word_until_inner(stops);
        self.nesting -= 1;
        result
    }

    fn parse_word_until_inner(&mut self, stops: &[char]) -> PResult<Word> {
        let mut parts = Vec::new();
        while self.parse_word_piece(&mut parts, stops)? {}
        Ok(Word(parts))
    }

    /// Parse one piece of a word; false when a stop character (or the end) is reached.
    fn parse_word_piece(&mut self, parts: &mut Vec<Part>, stops: &[char]) -> PResult<bool> {
        let Some(c) = self.peek() else { return Ok(false) };
        if stops.contains(&c) {
            return Ok(false);
        }
        match c {
            '\\' => {
                self.bump();
                match self.bump() {
                    Some('\n') => {}
                    Some(next) => push_quoted(parts, next),
                    None => push_lit(parts, '\\'),
                }
            }
            '\'' => {
                self.bump();
                let mut s = String::new();
                loop {
                    match self.bump() {
                        Some('\'') => break,
                        Some(ch) => s.push(ch),
                        None => return self.err("unexpected EOF while looking for matching `''"),
                    }
                }
                parts.push(Part::Quoted(s));
            }
            '"' => {
                self.bump();
                let inner = self.parse_dq_parts(Some('"'), false)?;
                parts.push(Part::Double(inner));
            }
            '$' => {
                let part = self.parse_dollar()?;
                match part {
                    Part::Lit(s) => {
                        for ch in s.chars() {
                            push_lit(parts, ch);
                        }
                    }
                    other => parts.push(other),
                }
            }
            '`' => {
                let script = self.parse_backticks()?;
                parts.push(Part::CmdSub(Arc::new(script)));
            }
            _ => {
                self.bump();
                push_lit(parts, c);
            }
        }
        Ok(true)
    }

    /// Body of a double-quoted region (or of here-doc / arithmetic text when
    /// `close` is `None`).
    fn parse_dq_parts(&mut self, close: Option<char>, drop_dquotes: bool) -> PResult<Vec<Part>> {
        if self.nesting >= 64 { return self.err("maximum shell syntax nesting exceeded (64)"); }
        self.nesting += 1;
        let result = self.parse_dq_parts_inner(close, drop_dquotes);
        self.nesting -= 1;
        result
    }

    fn parse_dq_parts_inner(&mut self, close: Option<char>, drop_dquotes: bool) -> PResult<Vec<Part>> {
        let mut parts = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                if close.is_some() {
                    return self.err("unexpected EOF while looking for matching `\"'");
                }
                break;
            };
            if Some(c) == close {
                self.bump();
                break;
            }
            match c {
                '\\' => {
                    self.bump();
                    match self.peek() {
                        Some('\n') => {
                            self.bump();
                        }
                        Some(n) if matches!(n, '$' | '`' | '"' | '\\') => {
                            self.bump();
                            push_quoted(&mut parts, n);
                        }
                        _ => push_quoted(&mut parts, '\\'),
                    }
                }
                '$' => {
                    let part = self.parse_dollar()?;
                    match part {
                        Part::Lit(s) => {
                            for ch in s.chars() {
                                push_quoted(&mut parts, ch);
                            }
                        }
                        // `$'..'` is not special inside double quotes.
                        other => parts.push(other),
                    }
                }
                '`' => {
                    let script = self.parse_backticks()?;
                    parts.push(Part::CmdSub(Arc::new(script)));
                }
                '"' if drop_dquotes => {
                    self.bump();
                }
                _ => {
                    self.bump();
                    push_quoted(&mut parts, c);
                }
            }
        }
        Ok(parts)
    }

    fn parse_backticks(&mut self) -> PResult<Script> {
        self.bump(); // `
        let mut text = String::new();
        loop {
            match self.bump() {
                Some('`') => break,
                Some('\\') => match self.bump() {
                    Some(c) if matches!(c, '$' | '`' | '\\') => text.push(c),
                    Some(c) => {
                        text.push('\\');
                        text.push(c);
                    }
                    None => return self.err("unexpected EOF while looking for matching ``'"),
                },
                Some(c) => text.push(c),
                None => return self.err("unexpected EOF while looking for matching ``'"),
            }
        }
        parse(&text).map_err(|e| self.relocate(e))
    }

    fn relocate(&self, mut e: ParseError) -> ParseError {
        e.line = self.line + e.line.saturating_sub(1);
        e
    }

    fn parse_dollar(&mut self) -> PResult<Part> {
        self.bump(); // $
        match self.peek() {
            Some('(') if self.peek_at(1) == Some('(') => {
                self.pos += 2;
                let text = self.read_until_double_paren()?;
                Ok(Part::Arith(parse_dq_text(&text, true).map_err(|e| self.relocate(e))?))
            }
            Some('(') => {
                self.bump();
                let script = self.parse_list(&Stop::Paren)?;
                self.skip_linebreaks();
                if !self.eat(")") {
                    return self.err("unexpected EOF while looking for matching `)'");
                }
                Ok(Part::CmdSub(Arc::new(script)))
            }
            Some('{') => {
                self.bump();
                self.parse_brace_param()
            }
            Some('\'') => {
                self.bump();
                Ok(Part::Quoted(self.parse_ansi_c()?))
            }
            Some('"') => {
                self.bump();
                Ok(Part::Double(self.parse_dq_parts(Some('"'), false)?))
            }
            Some(c) if is_name_start(c) => {
                let mut name = String::new();
                while let Some(ch) = self.peek().filter(|ch| is_name_char(*ch)) {
                    name.push(ch);
                    self.bump();
                }
                Ok(param(name, ParamOp::None))
            }
            Some(c) if c.is_ascii_digit() || matches!(c, '@' | '*' | '#' | '?' | '$' | '!' | '-') => {
                self.bump();
                Ok(param(c.to_string(), ParamOp::None))
            }
            _ => Ok(Part::Lit("$".into())),
        }
    }

    fn parse_ansi_c(&mut self) -> PResult<String> {
        let mut s = String::new();
        loop {
            match self.bump() {
                Some('\'') => return Ok(s),
                Some('\\') => {
                    let Some(c) = self.bump() else { break };
                    match c {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        'a' => s.push('\x07'),
                        'b' => s.push('\x08'),
                        'e' | 'E' => s.push('\x1b'),
                        'f' => s.push('\x0c'),
                        'v' => s.push('\x0b'),
                        '\\' => s.push('\\'),
                        '\'' => s.push('\''),
                        '"' => s.push('"'),
                        '?' => s.push('?'),
                        'x' => s.push(self.read_code(16, 2).unwrap_or('x')),
                        'u' => s.push(self.read_code(16, 4).unwrap_or('u')),
                        'U' => s.push(self.read_code(16, 8).unwrap_or('U')),
                        '0'..='7' => {
                            self.pos -= 1;
                            s.push(self.read_code(8, 3).unwrap_or('0'));
                        }
                        other => {
                            s.push('\\');
                            s.push(other);
                        }
                    }
                }
                Some(c) => s.push(c),
                None => break,
            }
        }
        self.err("unexpected EOF while looking for matching `''")
    }

    fn read_code(&mut self, radix: u32, max: usize) -> Option<char> {
        let mut digits = String::new();
        while digits.len() < max && self.peek().is_some_and(|c| c.is_digit(radix)) {
            digits.push(self.bump()?);
        }
        if digits.is_empty() {
            return None;
        }
        u32::from_str_radix(&digits, radix).ok().and_then(char::from_u32)
    }

    /// After `((` / `$((`: raw text up to the matching `))`.
    fn read_until_double_paren(&mut self) -> PResult<String> {
        let mut depth = 0usize;
        let mut text = String::new();
        loop {
            match self.peek() {
                None => return self.err("unexpected EOF while looking for matching `))'"),
                Some('(') => {
                    depth += 1;
                    text.push('(');
                    self.bump();
                }
                Some(')') if depth == 0 => {
                    if self.peek_at(1) == Some(')') {
                        self.pos += 2;
                        return Ok(text);
                    }
                    return self.err("syntax error: expected `))'");
                }
                Some(')') => {
                    depth -= 1;
                    text.push(')');
                    self.bump();
                }
                Some(c) => {
                    text.push(c);
                    self.bump();
                }
            }
        }
    }

    /// After `${`.
    fn parse_brace_param(&mut self) -> PResult<Part> {
        // ${#name} (length) — but ${#} alone is the parameter count.
        if self.peek() == Some('#') && !matches!(self.peek_at(1), Some('}')) {
            self.bump();
            let (name, index) = self.param_name()?;
            if !self.eat("}") {
                return self.err("bad substitution");
            }
            return Ok(Part::Param(Box::new(Param { name, index, indirect: false, op: ParamOp::Length })));
        }
        let indirect = self.peek() == Some('!') && !matches!(self.peek_at(1), Some('}'));
        if indirect {
            self.bump();
        }
        let (name, index) = self.param_name()?;
        let op = if self.eat("}") {
            ParamOp::None
        } else if let Some(op) = self.param_op()? {
            if !self.eat("}") {
                return self.err("bad substitution");
            }
            op
        } else {
            return self.err("bad substitution");
        };
        Ok(Part::Param(Box::new(Param { name, index, indirect, op })))
    }

    fn param_name(&mut self) -> PResult<(String, Option<Word>)> {
        let mut name = String::new();
        match self.peek() {
            Some(c) if is_name_start(c) => {
                while let Some(ch) = self.peek().filter(|ch| is_name_char(*ch)) {
                    name.push(ch);
                    self.bump();
                }
            }
            Some(c) if c.is_ascii_digit() => {
                while let Some(ch) = self.peek().filter(|ch| ch.is_ascii_digit()) {
                    name.push(ch);
                    self.bump();
                }
            }
            Some(c) if matches!(c, '@' | '*' | '#' | '?' | '$' | '!' | '-') => {
                name.push(c);
                self.bump();
            }
            _ => return self.err("bad substitution"),
        }
        let index = if self.peek() == Some('[') && is_name(&name) {
            self.bump();
            let w = self.parse_word_until(&[']'])?;
            if !self.eat("]") {
                return self.err("bad substitution");
            }
            Some(w)
        } else {
            None
        };
        Ok((name, index))
    }

    fn param_op(&mut self) -> PResult<Option<ParamOp>> {
        let until_brace = |p: &mut Parser| p.parse_brace_word(&['}']);
        let op = if self.eat(":-") {
            ParamOp::Default { colon: true, word: until_brace(self)? }
        } else if self.eat(":=") {
            ParamOp::Assign { colon: true, word: until_brace(self)? }
        } else if self.eat(":?") {
            ParamOp::Error { colon: true, word: until_brace(self)? }
        } else if self.eat(":+") {
            ParamOp::Alt { colon: true, word: until_brace(self)? }
        } else if self.eat("-") {
            ParamOp::Default { colon: false, word: until_brace(self)? }
        } else if self.eat("=") {
            ParamOp::Assign { colon: false, word: until_brace(self)? }
        } else if self.eat("?") {
            ParamOp::Error { colon: false, word: until_brace(self)? }
        } else if self.eat("+") {
            ParamOp::Alt { colon: false, word: until_brace(self)? }
        } else if self.eat("##") {
            ParamOp::TrimPrefix { longest: true, pattern: until_brace(self)? }
        } else if self.eat("#") {
            ParamOp::TrimPrefix { longest: false, pattern: until_brace(self)? }
        } else if self.eat("%%") {
            ParamOp::TrimSuffix { longest: true, pattern: until_brace(self)? }
        } else if self.eat("%") {
            ParamOp::TrimSuffix { longest: false, pattern: until_brace(self)? }
        } else if self.peek() == Some('/') {
            self.bump();
            let all = self.eat("/");
            let anchor = if !all && matches!(self.peek(), Some('#') | Some('%')) { self.bump() } else { None };
            let pattern = self.parse_brace_word(&['/', '}'])?;
            let with = if self.eat("/") { self.parse_brace_word(&['}'])? } else { Word::default() };
            ParamOp::Replace { all, anchor, pattern, with }
        } else if self.eat(":") {
            let offset = self.parse_brace_word(&[':', '}'])?;
            let length = if self.eat(":") { Some(self.parse_brace_word(&['}'])?) } else { None };
            ParamOp::Substring { offset, length }
        } else if self.eat("^^") {
            ParamOp::Case { upper: true, all: true }
        } else if self.eat("^") {
            ParamOp::Case { upper: true, all: false }
        } else if self.eat(",,") {
            ParamOp::Case { upper: false, all: true }
        } else if self.eat(",") {
            ParamOp::Case { upper: false, all: false }
        } else {
            return Ok(None);
        };
        Ok(Some(op))
    }

    /// A word inside `${...}`: blanks are literal, nested braces balance.
    fn parse_brace_word(&mut self, stops: &[char]) -> PResult<Word> {
        let mut parts = Vec::new();
        let mut depth = 0usize;
        loop {
            match self.peek() {
                None => return self.err("bad substitution: no closing `}'"),
                Some('}') if depth > 0 => {
                    depth -= 1;
                    self.bump();
                    push_lit(&mut parts, '}');
                }
                Some(c) if stops.contains(&c) => break,
                Some('{') => {
                    depth += 1;
                    self.bump();
                    push_lit(&mut parts, '{');
                }
                Some(c) if META.contains(&c) => {
                    self.bump();
                    push_lit(&mut parts, c);
                }
                Some(_) => {
                    let stops_inner: Vec<char> = stops.iter().copied().chain(['{']).collect();
                    self.parse_word_piece(&mut parts, &stops_inner)?;
                }
            }
        }
        Ok(Word(parts))
    }
}

const UNARY_TESTS: &[&str] = &[
    "-a", "-b", "-c", "-d", "-e", "-f", "-g", "-h", "-k", "-p", "-r", "-s", "-t", "-u", "-w", "-x",
    "-G", "-L", "-N", "-O", "-S", "-z", "-n", "-v", "-o", "-R",
];

const BINARY_TESTS: &[&str] =
    &["==", "=", "!=", "=~", "-eq", "-ne", "-lt", "-le", "-gt", "-ge", "-nt", "-ot", "-ef"];

fn param(name: String, op: ParamOp) -> Part {
    Part::Param(Box::new(Param { name, index: None, indirect: false, op }))
}

fn push_lit(parts: &mut Vec<Part>, c: char) {
    if let Some(Part::Lit(s)) = parts.last_mut() {
        s.push(c);
    } else {
        parts.push(Part::Lit(c.to_string()));
    }
}

fn push_quoted(parts: &mut Vec<Part>, c: char) {
    if let Some(Part::Quoted(s)) = parts.last_mut() {
        s.push(c);
    } else {
        parts.push(Part::Quoted(c.to_string()));
    }
}

fn push_redirect(command: &mut Command, r: Redirect) {
    match command {
        Command::Simple(s) => s.redirects.push(r),
        Command::Compound { redirects, .. } => redirects.push(r),
        Command::FunctionDef { .. } => {}
    }
}

/// Literal text of a word part as written (for here-doc delimiters).
fn raw_text(part: &Part) -> String {
    match part {
        Part::Lit(s) | Part::Quoted(s) => s.clone(),
        Part::Double(inner) => inner.iter().map(raw_text).collect(),
        Part::Param(p) => format!("${}", p.name),
        Part::CmdSub(_) | Part::Arith(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) -> Script {
        parse(src).unwrap_or_else(|e| panic!("{src:?}: {e}"))
    }

    fn bad(src: &str) -> String {
        parse(src).expect_err(src).message
    }

    #[test]
    fn parses_lists_pipelines_and_connectors() {
        let s = ok("a | b && c || d; e &\nf");
        assert_eq!(s.0.len(), 3);
        assert_eq!(s.0[0].and_or.first.commands.len(), 2);
        assert_eq!(s.0[0].and_or.rest.len(), 2);
        assert!(s.0[1].background);
    }

    #[test]
    fn parses_compound_commands() {
        ok("if a; then b; elif c; then d; else e; fi");
        ok("while read x; do echo $x; done < f");
        ok("until false\ndo\n  break\ndone");
        ok("for i in 1 2 3; do echo $i; done");
        ok("for i; do :; done");
        ok("for ((i=0; i<3; i++)); do echo $i; done");
        ok("case $x in\n a|b) echo ab;;\n (c) echo c;&\n *) echo other\nesac");
        ok("{ echo a; echo b; } > out");
        ok("(cd /tmp && ls)");
        ok("f() { echo hi; }\nfunction g { echo g; }\nfunction h() ( echo h )");
        ok("[[ -f x && $a == b* || ! -d y ]]");
        ok("[[ $s =~ ^(a|b)+$ ]]");
        ok("(( i += 2 ))");
    }

    #[test]
    fn keeps_quoting_structure() {
        let s = ok(r#"echo 'a b' "c $d" e\ f"#);
        let Command::Simple(cmd) = &s.0[0].and_or.first.commands[0] else { panic!() };
        assert_eq!(cmd.words[1], Word(vec![Part::Quoted("a b".into())]));
        assert!(matches!(&cmd.words[2].0[0], Part::Double(inner) if inner.len() == 2));
        assert_eq!(cmd.words[3], Word(vec![Part::Lit("e".into()), Part::Quoted(" ".into()), Part::Lit("f".into())]));
    }

    #[test]
    fn parses_parameter_operators() {
        for src in [
            "${a:-b c}", "${a-x}", "${a:=1}", "${a:?oops}", "${a:+y}", "${#a}", "${#}", "${a#*/}",
            "${a##*/}", "${a%.*}", "${a%%.*}", "${a/x/y}", "${a//x}", "${a/#x/y}", "${a:1:2}",
            "${a^^}", "${!ref}", "${arr[@]}", "${#arr[@]}", "${a:-${b:-c}}",
        ] {
            ok(&format!("echo {src}"));
        }
        assert!(bad("echo ${a@Z}").contains("bad substitution"));
    }

    #[test]
    fn reads_heredocs_after_the_line() {
        let s = ok("cat <<EOF > out; echo after\nline $x\nEOF\necho next");
        assert_eq!(s.0.len(), 3);
        let Command::Simple(cmd) = &s.0[0].and_or.first.commands[0] else { panic!() };
        assert_eq!(cmd.redirects[0].kind, RedirKind::HereDoc);
        assert!(matches!(cmd.redirects[0].target.0[1], Part::Param(_)));

        let s = ok("cat <<'EOF'\n$not expanded\nEOF\n");
        let Command::Simple(cmd) = &s.0[0].and_or.first.commands[0] else { panic!() };
        assert_eq!(cmd.redirects[0].target, Word(vec![Part::Quoted("$not expanded\n".into())]));

        ok("cat <<-EOF\n\tindented\n\tEOF\n");
    }

    #[test]
    fn parses_assignments_and_arrays() {
        let s = ok("a=1 b+=2 arr=(x 'y z') c[3]=4 cmd");
        let Command::Simple(cmd) = &s.0[0].and_or.first.commands[0] else { panic!() };
        assert_eq!(cmd.assigns.len(), 4);
        assert!(matches!(&cmd.assigns[2].value, AssignValue::Array(v) if v.len() == 2));
        assert_eq!(cmd.words.len(), 1);
        // `=` after the command name is an ordinary argument.
        let s = ok("echo a=b");
        let Command::Simple(cmd) = &s.0[0].and_or.first.commands[0] else { panic!() };
        assert!(cmd.assigns.is_empty());
    }

    #[test]
    fn reports_syntax_errors() {
        assert!(bad("if true; then echo").contains("fi"));
        assert!(bad("echo )").contains("`)'"));
        assert!(bad("| x").contains("`|'"));
        assert!(bad("echo 'open").contains("matching"));
        assert!(bad("done").contains("`done'"));
    }
}
