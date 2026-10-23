//! sed: addresses (`N`, `$`, `/re/`, `first~step`, ranges, `!`), blocks,
//! `s y d D p P n N q Q = a i c h H g G x z : b t T`, `-n -e -f -E -i -s`.
//! `r w e l` (file/command side channels) are not supported and say so.

use regex::Regex;

use super::regex_util;
use super::{UtilCtx, lines};

#[derive(Clone)]
enum Addr {
    Line(usize),
    Last,
    /// `None` reuses the last regex.
    Re(Option<Regex>),
    Step(usize, usize),
}

#[derive(Clone)]
enum Addr2 {
    Addr(Addr),
    Plus(usize),
    Multiple(usize),
}

#[derive(Clone)]
enum Rep {
    Lit(String),
    Whole,
    Group(usize),
    Upper,
    Lower,
    UpperOne,
    LowerOne,
    EndCase,
}

#[derive(Clone)]
enum Kind {
    Block(usize),
    EndBlock,
    Subst { re: Option<Regex>, rep: Vec<Rep>, global: bool, nth: usize, print: bool },
    Translate(Vec<(char, char)>),
    Delete,
    DeleteFirst,
    Print,
    PrintFirst,
    Next,
    NextAppend,
    Quit(i32),
    QuitSilent(i32),
    LineNo,
    Append(String),
    Insert(String),
    Change(String),
    Hold,
    HoldAppend,
    Get,
    GetAppend,
    Exchange,
    Zap,
    Label,
    Branch(Option<String>),
    Test(Option<String>),
    TestNot(Option<String>),
}

#[derive(Clone)]
struct Cmd {
    a1: Option<Addr>,
    a2: Option<Addr2>,
    negate: bool,
    kind: Kind,
    label: Option<String>,
}

struct Parser<'a> {
    chars: Vec<char>,
    pos: usize,
    extended: bool,
    _src: &'a str,
}

type PResult<T> = Result<T, String>;

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn err<T>(&self, msg: &str) -> PResult<T> {
        Err(format!("-e expression #1, char {}: {msg}", self.pos))
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(' ') | Some('\t')) {
            self.pos += 1;
        }
    }

    fn number(&mut self) -> Option<usize> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if start == self.pos { None } else { self.chars[start..self.pos].iter().collect::<String>().parse().ok() }
    }

    /// Text up to an unescaped `delim`; `\delim` becomes `delim`, `\n` a newline
    /// (when `newline_escape`), other escapes are kept for the regex.
    fn delimited(&mut self, delim: char, newline_escape: bool) -> PResult<String> {
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return self.err("unterminated `s' command"),
                Some(c) if c == delim => return Ok(out),
                Some('\\') => match self.bump() {
                    Some(c) if c == delim => out.push(c),
                    Some('n') if newline_escape => out.push('\n'),
                    Some(c) => {
                        out.push('\\');
                        out.push(c);
                    }
                    None => return self.err("unterminated `s' command"),
                },
                Some(c) => out.push(c),
            }
        }
    }

    fn regex(&self, pat: &str, icase: bool) -> PResult<Option<Regex>> {
        if pat.is_empty() {
            return Ok(None);
        }
        regex_util::build(pat, self.extended, false, icase).map(Some).map_err(|e| format!("-e expression #1, char {}: {e}", self.pos))
    }

    fn address(&mut self) -> PResult<Option<Addr>> {
        match self.peek() {
            Some(c) if c.is_ascii_digit() => {
                let n = self.number().unwrap_or(0);
                if self.peek() == Some('~') {
                    self.pos += 1;
                    let step = self.number().unwrap_or(0);
                    return Ok(Some(Addr::Step(n, step)));
                }
                Ok(Some(Addr::Line(n)))
            }
            Some('$') => {
                self.pos += 1;
                Ok(Some(Addr::Last))
            }
            Some('/') | Some('\\') => {
                let delim = if self.bump() == Some('\\') { self.bump().unwrap_or('/') } else { '/' };
                let pat = self.delimited(delim, true)?;
                let icase = if matches!(self.peek(), Some('I')) {
                    self.pos += 1;
                    true
                } else {
                    false
                };
                Ok(Some(Addr::Re(self.regex(&pat, icase)?)))
            }
            _ => Ok(None),
        }
    }

    /// Rest of the line for `a`/`i`/`c` (GNU one-liner and `a\` forms).
    fn text_arg(&mut self) -> String {
        self.skip_space();
        if self.peek() == Some('\\') {
            self.pos += 1;
            if self.peek() == Some('\n') {
                self.pos += 1;
            }
        }
        let mut out = String::new();
        while let Some(c) = self.bump() {
            match c {
                '\n' => break,
                '\\' => match self.bump() {
                    Some('\n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(c) => out.push(c),
                    None => {}
                },
                c => out.push(c),
            }
        }
        out
    }

    fn label_arg(&mut self) -> Option<String> {
        self.skip_space();
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == '\n' || c == ';' || c == '}' {
                break;
            }
            out.push(c);
            self.pos += 1;
        }
        let out = out.trim().to_string();
        if out.is_empty() { None } else { Some(out) }
    }

    fn program(&mut self) -> PResult<Vec<Cmd>> {
        let mut cmds: Vec<Cmd> = Vec::new();
        let mut open: Vec<usize> = Vec::new();
        loop {
            while matches!(self.peek(), Some(' ') | Some('\t') | Some('\n') | Some(';')) {
                self.pos += 1;
            }
            let Some(_) = self.peek() else { break };
            if self.peek() == Some('#') {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.pos += 1;
                }
                continue;
            }
            let a1 = self.address()?;
            let mut a2 = None;
            if a1.is_some() && self.peek() == Some(',') {
                self.pos += 1;
                self.skip_space();
                a2 = match self.peek() {
                    Some('+') => {
                        self.pos += 1;
                        Some(Addr2::Plus(self.number().unwrap_or(0)))
                    }
                    Some('~') => {
                        self.pos += 1;
                        Some(Addr2::Multiple(self.number().unwrap_or(0)))
                    }
                    _ => match self.address()? {
                        Some(a) => Some(Addr2::Addr(a)),
                        None => return self.err("unexpected `,'"),
                    },
                };
            }
            self.skip_space();
            let mut negate = false;
            while self.peek() == Some('!') {
                negate = true;
                self.pos += 1;
                self.skip_space();
            }
            let Some(c) = self.bump() else { return self.err("missing command") };
            let mut label = None;
            let kind = match c {
                '{' => {
                    open.push(cmds.len());
                    Kind::Block(0)
                }
                '}' => {
                    let Some(start) = open.pop() else { return self.err("unexpected `}'") };
                    let here = cmds.len();
                    if let Kind::Block(end) = &mut cmds[start].kind {
                        *end = here;
                    }
                    Kind::EndBlock
                }
                's' => {
                    let Some(delim) = self.bump() else { return self.err("unterminated `s' command") };
                    let pat = self.delimited(delim, true)?;
                    let rep_text = self.delimited(delim, false)?;
                    let (mut global, mut nth, mut print, mut icase) = (false, 1, false, false);
                    loop {
                        match self.peek() {
                            Some('g') => global = true,
                            Some('p') => print = true,
                            Some('i') | Some('I') => icase = true,
                            Some('m') | Some('M') => {}
                            Some(d) if d.is_ascii_digit() => {
                                nth = self.number().unwrap_or(1).max(1);
                                continue;
                            }
                            Some('w') | Some('e') => return self.err("the `w' and `e' flags are not supported"),
                            _ => break,
                        }
                        self.pos += 1;
                    }
                    Kind::Subst { re: self.regex(&pat, icase)?, rep: parse_replacement(&rep_text), global, nth, print }
                }
                'y' => {
                    let Some(delim) = self.bump() else { return self.err("unterminated `y' command") };
                    let from: Vec<char> = self.delimited(delim, true)?.replace("\\\\", "\\").chars().collect();
                    let to: Vec<char> = self.delimited(delim, true)?.replace("\\\\", "\\").chars().collect();
                    if from.len() != to.len() {
                        return self.err("strings for `y' command are different lengths");
                    }
                    Kind::Translate(from.into_iter().zip(to).collect())
                }
                'd' => Kind::Delete,
                'D' => Kind::DeleteFirst,
                'p' => Kind::Print,
                'P' => Kind::PrintFirst,
                'n' => Kind::Next,
                'N' => Kind::NextAppend,
                'q' | 'Q' => {
                    self.skip_space();
                    let code = self.number().unwrap_or(0) as i32;
                    if c == 'q' { Kind::Quit(code) } else { Kind::QuitSilent(code) }
                }
                '=' => Kind::LineNo,
                'a' => Kind::Append(self.text_arg()),
                'i' => Kind::Insert(self.text_arg()),
                'c' => Kind::Change(self.text_arg()),
                'h' => Kind::Hold,
                'H' => Kind::HoldAppend,
                'g' => Kind::Get,
                'G' => Kind::GetAppend,
                'x' => Kind::Exchange,
                'z' => Kind::Zap,
                ':' => {
                    label = self.label_arg();
                    if label.is_none() {
                        return self.err("\":\" lacks a label");
                    }
                    Kind::Label
                }
                'b' => Kind::Branch(self.label_arg()),
                't' => Kind::Test(self.label_arg()),
                'T' => Kind::TestNot(self.label_arg()),
                'r' | 'R' | 'w' | 'W' | 'e' | 'l' | 'F' | 'v' => {
                    return self.err(&format!("the `{c}' command is not supported"));
                }
                other => return self.err(&format!("unknown command: `{other}'")),
            };
            cmds.push(Cmd { a1, a2, negate, kind, label });
        }
        if !open.is_empty() {
            return self.err("unmatched `{'");
        }
        Ok(cmds)
    }
}

fn parse_replacement(text: &str) -> Vec<Rep> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut chars = text.chars();
    let flush = |lit: &mut String, out: &mut Vec<Rep>| {
        if !lit.is_empty() {
            out.push(Rep::Lit(std::mem::take(lit)));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '&' => {
                flush(&mut lit, &mut out);
                out.push(Rep::Whole);
            }
            '\\' => match chars.next() {
                Some(d @ '0'..='9') => {
                    flush(&mut lit, &mut out);
                    out.push(if d == '0' { Rep::Whole } else { Rep::Group(d as usize - '0' as usize) });
                }
                Some('n') => lit.push('\n'),
                Some('t') => lit.push('\t'),
                Some('U') => {
                    flush(&mut lit, &mut out);
                    out.push(Rep::Upper);
                }
                Some('L') => {
                    flush(&mut lit, &mut out);
                    out.push(Rep::Lower);
                }
                Some('u') => {
                    flush(&mut lit, &mut out);
                    out.push(Rep::UpperOne);
                }
                Some('l') => {
                    flush(&mut lit, &mut out);
                    out.push(Rep::LowerOne);
                }
                Some('E') => {
                    flush(&mut lit, &mut out);
                    out.push(Rep::EndCase);
                }
                Some(other) => lit.push(other),
                None => lit.push('\\'),
            },
            c => lit.push(c),
        }
    }
    flush(&mut lit, &mut out);
    out
}

fn expand_replacement(rep: &[Rep], caps: &regex::Captures<'_>) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        None,
        Upper,
        Lower,
    }
    let mut out = String::new();
    let mut mode = Mode::None;
    let mut one: Option<bool> = None;
    let mut push = |s: &str, mode: Mode, one: &mut Option<bool>| {
        for c in s.chars() {
            let c: String = match (one.take(), mode) {
                (Some(true), _) => c.to_uppercase().collect(),
                (Some(false), _) => c.to_lowercase().collect(),
                (None, Mode::Upper) => c.to_uppercase().collect(),
                (None, Mode::Lower) => c.to_lowercase().collect(),
                (None, Mode::None) => c.to_string(),
            };
            out.push_str(&c);
        }
    };
    for r in rep {
        match r {
            Rep::Lit(s) => push(s, mode, &mut one),
            Rep::Whole => push(caps.get(0).map_or("", |m| m.as_str()), mode, &mut one),
            Rep::Group(n) => push(caps.get(*n).map_or("", |m| m.as_str()), mode, &mut one),
            Rep::Upper => mode = Mode::Upper,
            Rep::Lower => mode = Mode::Lower,
            Rep::EndCase => mode = Mode::None,
            Rep::UpperOne => one = Some(true),
            Rep::LowerOne => one = Some(false),
        }
    }
    out
}

#[derive(Clone, Default)]
struct RangeState {
    active: bool,
    end_line: usize,
}

struct Run<'p> {
    cmds: &'p [Cmd],
    quiet: bool,
    last_re: Option<Regex>,
    hold: String,
    quit: Option<i32>,
}

impl Run<'_> {
    fn re<'a>(&'a mut self, re: &'a Option<Regex>) -> Option<Regex> {
        match re {
            Some(r) => {
                self.last_re = Some(r.clone());
                Some(r.clone())
            }
            None => self.last_re.clone(),
        }
    }

    fn addr_match(&mut self, a: &Addr, ps: &str, line: usize, last: bool) -> bool {
        match a {
            Addr::Line(n) => line == *n,
            Addr::Last => last,
            Addr::Re(re) => self.re(re).is_some_and(|r| r.is_match(ps)),
            Addr::Step(first, step) => {
                if *step == 0 { line == *first } else { line >= *first && (line - first).is_multiple_of(*step) }
            }
        }
    }

    fn selected(&mut self, idx: usize, ps: &str, line: usize, last: bool, ranges: &mut [RangeState]) -> bool {
        let cmd = &self.cmds[idx];
        let (a1, a2, negate) = (cmd.a1.clone(), cmd.a2.clone(), cmd.negate);
        let hit = match (a1, a2) {
            (None, _) => true,
            (Some(a), None) => self.addr_match(&a, ps, line, last),
            (Some(a), Some(b)) => {
                let state = &mut ranges[idx];
                if state.active {
                    let ends = match &b {
                        Addr2::Addr(Addr::Line(n)) => line >= *n,
                        Addr2::Addr(Addr::Last) => last,
                        Addr2::Addr(Addr::Re(re)) => {
                            let re = re.clone();
                            self.re(&re).is_some_and(|r| r.is_match(ps))
                        }
                        Addr2::Addr(Addr::Step(..)) => true,
                        Addr2::Plus(_) => line >= ranges[idx].end_line,
                        Addr2::Multiple(m) => *m == 0 || line.is_multiple_of(*m),
                    };
                    if ends {
                        ranges[idx].active = false;
                    }
                    true
                } else if self.addr_match(&a, ps, line, last) {
                    let (active, end_line) = match &b {
                        Addr2::Addr(Addr::Line(n)) => (*n > line, 0),
                        Addr2::Addr(Addr::Last) => (!last, 0),
                        Addr2::Plus(n) => (*n > 0, line + n),
                        Addr2::Multiple(m) => (*m > 0 && !line.is_multiple_of(*m), 0),
                        _ => (true, 0),
                    };
                    ranges[idx] = RangeState { active, end_line };
                    true
                } else {
                    false
                }
            }
        };
        hit != negate
    }

    /// Run over `input`; output is appended to `out`.
    fn run(&mut self, input: &[String], trailing_newline: bool, out: &mut String) {
        let mut ranges = vec![RangeState::default(); self.cmds.len()];
        let n = input.len();
        let mut next = 0usize;
        let mut line_no = 0usize;
        let mut carried: Option<String> = None;
        while self.quit.is_none() && (carried.is_some() || next < n) {
            let mut ps = match carried.take() {
                Some(ps) => ps,
                None => {
                    line_no += 1;
                    next += 1;
                    input[next - 1].clone()
                }
            };
            let mut appends: Vec<String> = Vec::new();
            let mut sub_flag = false;
            let mut autoprint = !self.quiet;
            let mut restart = false;
            let mut pc = 0;
            while pc < self.cmds.len() {
                let last = next == n;
                if !self.selected(pc, &ps, line_no, last, &mut ranges) {
                    pc = match self.cmds[pc].kind {
                        Kind::Block(end) => end + 1,
                        _ => pc + 1,
                    };
                    continue;
                }
                let kind = self.cmds[pc].kind.clone();
                pc += 1;
                match kind {
                    Kind::Block(_) | Kind::EndBlock | Kind::Label => {}
                    Kind::Subst { re, rep, global, nth, print } => {
                        let Some(re) = self.re(&re) else { continue };
                        let mut result = String::new();
                        let mut last_end = 0;
                        let mut count = 0;
                        let mut replaced = false;
                        for caps in re.captures_iter(&ps) {
                            let m = caps.get(0).expect("group 0 always matches");
                            count += 1;
                            if count < nth || (replaced && !global) {
                                continue;
                            }
                            result.push_str(&ps[last_end..m.start()]);
                            result.push_str(&expand_replacement(&rep, &caps));
                            last_end = m.end();
                            replaced = true;
                        }
                        if replaced {
                            result.push_str(&ps[last_end..]);
                            ps = result;
                            sub_flag = true;
                            if print {
                                out.push_str(&ps);
                                out.push('\n');
                            }
                        }
                    }
                    Kind::Translate(map) => {
                        ps = ps.chars().map(|c| map.iter().find(|(f, _)| *f == c).map_or(c, |(_, t)| *t)).collect();
                    }
                    Kind::Delete => {
                        autoprint = false;
                        break;
                    }
                    Kind::DeleteFirst => {
                        if let Some(i) = ps.find('\n') {
                            carried = Some(ps[i + 1..].to_string());
                            restart = true;
                        }
                        autoprint = false;
                        break;
                    }
                    Kind::Print => {
                        out.push_str(&ps);
                        out.push('\n');
                    }
                    Kind::PrintFirst => {
                        out.push_str(ps.split('\n').next().unwrap_or(""));
                        out.push('\n');
                    }
                    Kind::Next => {
                        if next < n {
                            if !self.quiet {
                                out.push_str(&ps);
                                out.push('\n');
                            }
                            ps = input[next].clone();
                            next += 1;
                            line_no += 1;
                        } else {
                            self.quit = Some(0);
                            break;
                        }
                    }
                    Kind::NextAppend => {
                        if next < n {
                            ps.push('\n');
                            ps.push_str(&input[next]);
                            next += 1;
                            line_no += 1;
                        } else {
                            self.quit = Some(0);
                            break;
                        }
                    }
                    Kind::Quit(code) => {
                        self.quit = Some(code);
                        break;
                    }
                    Kind::QuitSilent(code) => {
                        self.quit = Some(code);
                        autoprint = false;
                        break;
                    }
                    Kind::LineNo => out.push_str(&format!("{line_no}\n")),
                    Kind::Append(t) => appends.push(t),
                    Kind::Insert(t) => {
                        out.push_str(&t);
                        out.push('\n');
                    }
                    Kind::Change(t) => {
                        let in_range = self.cmds[pc - 1].a2.is_some() && ranges[pc - 1].active;
                        if !in_range {
                            out.push_str(&t);
                            out.push('\n');
                        }
                        autoprint = false;
                        break;
                    }
                    Kind::Hold => self.hold = ps.clone(),
                    Kind::HoldAppend => {
                        self.hold.push('\n');
                        self.hold.push_str(&ps);
                    }
                    Kind::Get => ps = self.hold.clone(),
                    Kind::GetAppend => {
                        ps.push('\n');
                        ps.push_str(&self.hold);
                    }
                    Kind::Exchange => std::mem::swap(&mut ps, &mut self.hold),
                    Kind::Zap => ps.clear(),
                    Kind::Branch(label) => pc = self.jump(label.as_deref()),
                    Kind::Test(label) => {
                        if sub_flag {
                            sub_flag = false;
                            pc = self.jump(label.as_deref());
                        }
                    }
                    Kind::TestNot(label) => {
                        if !sub_flag {
                            pc = self.jump(label.as_deref());
                        } else {
                            sub_flag = false;
                        }
                    }
                }
            }
            if autoprint {
                out.push_str(&ps);
                if next < n || trailing_newline || carried.is_some() {
                    out.push('\n');
                }
            }
            for t in appends {
                out.push_str(&t);
                out.push('\n');
            }
            if restart {
                continue;
            }
        }
    }

    fn jump(&self, label: Option<&str>) -> usize {
        match label {
            None => self.cmds.len(),
            Some(l) => self.cmds.iter().position(|c| c.label.as_deref() == Some(l)).map_or(self.cmds.len(), |i| i + 1),
        }
    }
}

pub(crate) async fn sed(ctx: UtilCtx<'_>) -> i32 {
    let mut quiet = false;
    let mut extended = false;
    let mut in_place: Option<String> = None;
    let mut separate = false;
    let mut scripts: Vec<String> = Vec::new();
    let mut operands = Vec::new();
    let mut i = 0;
    let args = ctx.args;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-n" | "--quiet" | "--silent" => quiet = true,
            "-E" | "-r" | "--regexp-extended" => extended = true,
            "-s" | "--separate" => separate = true,
            "-z" | "-u" | "--posix" | "--debug" | "--sandbox" => {}
            "-e" | "--expression" => {
                i += 1;
                scripts.push(args.get(i).cloned().unwrap_or_default());
            }
            "-f" | "--file" => {
                i += 1;
                let file = args.get(i).cloned().unwrap_or_default();
                match ctx.read_input(&file).await {
                    Ok(d) => scripts.push(String::from_utf8_lossy(&d).into_owned()),
                    Err(e) => {
                        ctx.error(&format!("couldn't open file {file}: {e}"));
                        return 1;
                    }
                }
            }
            a if a.starts_with("--expression=") => scripts.push(a["--expression=".len()..].to_string()),
            a if a.starts_with("--in-place") => in_place = Some(a.strip_prefix("--in-place=").unwrap_or("").to_string()),
            a if a.starts_with("-i") => in_place = Some(a[2..].to_string()),
            a if a.starts_with('-') && a.len() > 1 && a[1..].chars().all(|c| "nErsz".contains(c)) => {
                quiet |= a.contains('n');
                extended |= a.contains('E') || a.contains('r');
                separate |= a.contains('s');
            }
            a if a.starts_with('-') && a.len() > 1 => {
                ctx.error(&format!("invalid option -- '{}'", &a[1..2]));
                return 1;
            }
            a => operands.push(a.to_string()),
        }
        i += 1;
    }
    if scripts.is_empty() {
        if operands.is_empty() {
            ctx.fds.err.write_str("Usage: sed [OPTION]... {script-only-if-no-other-script} [input-file]...\n");
            return 1;
        }
        scripts.push(operands.remove(0));
    }
    let script = scripts.join("\n");
    let mut parser = Parser { chars: script.chars().collect(), pos: 0, extended, _src: &script };
    let cmds = match parser.program() {
        Ok(c) => c,
        Err(e) => {
            ctx.error(&e);
            return 1;
        }
    };

    if in_place.is_some() && operands.is_empty() {
        ctx.error("no input files");
        return 1;
    }
    let mut status = 0;
    let mut run = Run { cmds: &cmds, quiet, last_re: None, hold: String::new(), quit: None };

    if let Some(suffix) = &in_place {
        for file in &operands {
            let data = match ctx.fs.read(file).await {
                Ok(d) => d,
                Err(e) => {
                    ctx.error(&format!("can't read {file}: {e}"));
                    status = 2;
                    continue;
                }
            };
            let (input, trailing) = lines(&data);
            let mut out = String::new();
            run.quit = None;
            run.run(&input, trailing, &mut out);
            if !suffix.is_empty() {
                let backup = if suffix.contains('*') {
                    suffix.replace('*', &wendy_libc::path::basename(file))
                } else {
                    format!("{file}{suffix}")
                };
                if let Err(e) = ctx.fs.write(&backup, &data).await {
                    ctx.error(&format!("couldn't write {backup}: {e}"));
                    status = 4;
                    continue;
                }
            }
            if let Err(e) = ctx.fs.write(file, out.as_bytes()).await {
                ctx.error(&format!("couldn't edit {file}: {e}"));
                status = 4;
            }
        }
        return run.quit.filter(|_| status == 0).unwrap_or(status);
    }

    let operands = if operands.is_empty() { vec!["-".to_string()] } else { operands };
    let mut streams: Vec<(Vec<String>, bool)> = Vec::new();
    let mut combined: Vec<String> = Vec::new();
    let mut trailing = true;
    for file in &operands {
        match ctx.read_input(file).await {
            Ok(d) => {
                let (l, t) = lines(&d);
                if separate {
                    streams.push((l, t));
                } else {
                    combined.extend(l);
                    trailing = t;
                }
            }
            Err(e) => {
                ctx.error(&format!("can't read {file}: {e}"));
                status = 2;
            }
        }
    }
    if !separate {
        streams.push((combined, trailing));
    }
    let mut out = String::new();
    for (input, trailing) in streams {
        if run.quit.is_some() {
            break;
        }
        run.run(&input, trailing, &mut out);
    }
    ctx.out(&out);
    run.quit.unwrap_or(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sed(script: &str, input: &str, quiet: bool, extended: bool) -> String {
        let mut p = Parser { chars: script.chars().collect(), pos: 0, extended, _src: script };
        let cmds = p.program().unwrap_or_else(|e| panic!("{script}: {e}"));
        let mut run = Run { cmds: &cmds, quiet, last_re: None, hold: String::new(), quit: None };
        let (lines, trailing) = lines(input.as_bytes());
        let mut out = String::new();
        run.run(&lines, trailing, &mut out);
        out
    }

    #[test]
    fn substitutes_with_flags_and_groups() {
        assert_eq!(sed("s/a/b/", "aaa\n", false, false), "baa\n");
        assert_eq!(sed("s/a/b/g", "aaa\n", false, false), "bbb\n");
        assert_eq!(sed("s/a/b/2", "aaa\n", false, false), "aba\n");
        assert_eq!(sed("s/\\(x\\)\\(y\\)/\\2\\1/", "xy\n", false, false), "yx\n");
        assert_eq!(sed("s/(x)(y)/\\2\\1/", "xy\n", false, true), "yx\n");
        assert_eq!(sed("s/w/[&]/", "a w b\n", false, false), "a [w] b\n");
        assert_eq!(sed("s/.*/\\U&/", "abc\n", false, false), "ABC\n");
        assert_eq!(sed("s|/|_|g", "a/b/c\n", false, false), "a_b_c\n");
        assert_eq!(sed("s/x/y/", "no newline", false, false), "no newline");
    }

    #[test]
    fn addresses_ranges_and_commands() {
        let input = "1\n2\n3\n4\n5\n";
        assert_eq!(sed("2,4d", input, false, false), "1\n5\n");
        assert_eq!(sed("2,4!d", input, false, false), "2\n3\n4\n");
        assert_eq!(sed("$p", input, true, false), "5\n");
        assert_eq!(sed("/3/,$d", input, false, false), "1\n2\n");
        assert_eq!(sed("1~2d", input, false, false), "2\n4\n");
        assert_eq!(sed("2q", input, false, false), "1\n2\n");
        assert_eq!(sed("n;d", input, false, false), "1\n3\n5\n");
        assert_eq!(sed("$!N;s/\\n/-/", input, false, false), "1-2\n3-4\n5\n");
        assert_eq!(sed("2i\\\nhead", "a\nb\n", false, false), "a\nhead\nb\n");
        assert_eq!(sed("1a tail", "a\nb\n", false, false), "a\ntail\nb\n");
        assert_eq!(sed("2c\\\nnew", "a\nb\nc\n", false, false), "a\nnew\nc\n");
        assert_eq!(sed("=", "a\nb\n", true, false), "1\n2\n");
        assert_eq!(sed("y/abc/xyz/", "aabbcc\n", false, false), "xxyyzz\n");
        assert_eq!(sed("1!G;h;$!d", "1\n2\n3\n", false, false), "3\n2\n1\n");
        assert_eq!(sed(":a;s/x//;ta", "xxxa\n", false, false), "a\n");
        assert_eq!(sed("/b/{s/b/B/;p}", "a\nb\n", true, false), "B\n");
    }
}
