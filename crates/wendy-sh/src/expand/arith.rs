//! Shell arithmetic: `$(( ))`, `(( ))`, `let`, array subscripts.
//!
//! 64-bit wrapping integers, C operator precedence, short-circuit `&&`,
//! `||` and `?:` (the untaken side is parsed but never evaluated, so its
//! assignments do not happen). Variables are read and written through
//! [`ArithVars`], which lets the interpreter evaluate a variable's value as
//! an expression in turn (`x=1+2; echo $((x*2))` is 6).

pub trait ArithVars {
    fn get(&mut self, name: &str, index: Option<i64>) -> Result<i64, String>;
    fn set(&mut self, name: &str, index: Option<i64>, value: i64) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(i64),
    Ident(String),
    Op(&'static str),
}

const OPS: &[&str] = &[
    "<<=", ">>=", "**", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+=", "-=", "*=",
    "/=", "%=", "&=", "^=", "|=", "+", "-", "*", "/", "%", "<", ">", "&", "^", "|", "!", "~", "?",
    ":", "=", "(", ")", "[", "]", ",",
];

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() || c == '"' {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '#' || chars[i] == '_' || chars[i] == '@') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            toks.push(Tok::Num(parse_number(&text)?));
            continue;
        }
        if c == '_' || c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && (chars[i] == '_' || chars[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            toks.push(Tok::Ident(chars[start..i].iter().collect()));
            continue;
        }
        let rest: String = chars[i..chars.len().min(i + 3)].iter().collect();
        match OPS.iter().find(|op| rest.starts_with(**op)) {
            Some(op) => {
                toks.push(Tok::Op(op));
                i += op.len();
            }
            None => return Err(format!("syntax error: invalid arithmetic operator (error token is \"{}\")", &src[src.char_indices().nth(i).map_or(0, |(b, _)| b)..])),
        }
    }
    Ok(toks)
}

/// `42`, `0x2a`, `052`, `2#101010`, `64#G`.
pub fn parse_number(text: &str) -> Result<i64, String> {
    let bad = || format!("{text}: value too great for base (error token is \"{text}\")");
    if let Some((base, digits)) = text.split_once('#') {
        let base: u32 = base.parse().map_err(|_| bad())?;
        if !(2..=64).contains(&base) {
            return Err(format!("{text}: invalid arithmetic base (error token is \"{text}\")"));
        }
        let mut value: i64 = 0;
        for c in digits.chars() {
            let d = match c {
                '0'..='9' => c as u32 - '0' as u32,
                'a'..='z' => c as u32 - 'a' as u32 + 10,
                'A'..='Z' if base <= 36 => c as u32 - 'A' as u32 + 10,
                'A'..='Z' => c as u32 - 'A' as u32 + 36,
                '@' => 62,
                '_' => 63,
                _ => return Err(bad()),
            };
            if d >= base {
                return Err(bad());
            }
            value = value.wrapping_mul(base as i64).wrapping_add(d as i64);
        }
        return Ok(value);
    }
    let (digits, radix) = if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        (hex, 16)
    } else if text.len() > 1 && text.starts_with('0') {
        (&text[1..], 8)
    } else {
        (text, 10)
    };
    u64::from_str_radix(digits, radix).map(|v| v as i64).map_err(|_| bad())
}

#[derive(Debug, Clone)]
enum Expr {
    Num(i64),
    Var(String, Option<Box<Expr>>),
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Assign(&'static str, String, Option<Box<Expr>>, Box<Expr>),
    IncDec { name: String, index: Option<Box<Expr>>, delta: i64, prefix: bool },
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Comma(Box<Expr>, Box<Expr>),
}

struct P {
    toks: Vec<Tok>,
    pos: usize,
}

fn binary_prec(op: &str) -> Option<(u8, bool)> {
    // (precedence, right-associative)
    Some(match op {
        "||" => (1, false),
        "&&" => (2, false),
        "|" => (3, false),
        "^" => (4, false),
        "&" => (5, false),
        "==" | "!=" => (6, false),
        "<" | ">" | "<=" | ">=" => (7, false),
        "<<" | ">>" => (8, false),
        "+" | "-" => (9, false),
        "*" | "/" | "%" => (10, false),
        "**" => (11, true),
        _ => return None,
    })
}

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Op(o)) if *o == op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn comma(&mut self) -> Result<Expr, String> {
        let mut left = self.assign()?;
        while self.eat_op(",") {
            let right = self.assign()?;
            left = Expr::Comma(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn assign(&mut self) -> Result<Expr, String> {
        let save = self.pos;
        if let Some(Tok::Ident(name)) = self.peek().cloned() {
            self.pos += 1;
            let index = if self.eat_op("[") {
                let e = self.comma()?;
                if !self.eat_op("]") {
                    return Err("syntax error: missing `]'".into());
                }
                Some(Box::new(e))
            } else {
                None
            };
            if let Some(Tok::Op(op)) = self.peek().cloned()
                && matches!(op, "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "<<=" | ">>=" | "&=" | "^=" | "|=")
            {
                self.pos += 1;
                let value = self.assign()?;
                return Ok(Expr::Assign(op, name, index, Box::new(value)));
            }
            self.pos = save;
        }
        self.ternary()
    }

    fn ternary(&mut self) -> Result<Expr, String> {
        let cond = self.binary(1)?;
        if self.eat_op("?") {
            let yes = self.assign()?;
            if !self.eat_op(":") {
                return Err("syntax error: `:' expected for conditional expression".into());
            }
            let no = self.assign()?;
            return Ok(Expr::Ternary(Box::new(cond), Box::new(yes), Box::new(no)));
        }
        Ok(cond)
    }

    fn binary(&mut self, min: u8) -> Result<Expr, String> {
        let mut left = self.unary()?;
        while let Some(Tok::Op(op)) = self.peek().cloned() {
            let Some((prec, right_assoc)) = binary_prec(op) else { break };
            if prec < min {
                break;
            }
            self.pos += 1;
            let right = self.binary(if right_assoc { prec } else { prec + 1 })?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if let Some(Tok::Op(op)) = self.peek().cloned() {
            match op {
                "++" | "--" => {
                    self.pos += 1;
                    let (name, index) = self.lvalue()?;
                    return Ok(Expr::IncDec { name, index, delta: if op == "++" { 1 } else { -1 }, prefix: true });
                }
                "!" | "~" | "+" | "-" => {
                    self.pos += 1;
                    return Ok(Expr::Unary(op, Box::new(self.unary()?)));
                }
                _ => {}
            }
        }
        self.postfix()
    }

    fn lvalue(&mut self) -> Result<(String, Option<Box<Expr>>), String> {
        let Some(Tok::Ident(name)) = self.peek().cloned() else {
            return Err("syntax error: operand expected".into());
        };
        self.pos += 1;
        let index = if self.eat_op("[") {
            let e = self.comma()?;
            if !self.eat_op("]") {
                return Err("syntax error: missing `]'".into());
            }
            Some(Box::new(e))
        } else {
            None
        };
        Ok((name, index))
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        match self.peek().cloned() {
            Some(Tok::Num(n)) => {
                self.pos += 1;
                Ok(Expr::Num(n))
            }
            Some(Tok::Ident(_)) => {
                let (name, index) = self.lvalue()?;
                if self.eat_op("++") {
                    return Ok(Expr::IncDec { name, index, delta: 1, prefix: false });
                }
                if self.eat_op("--") {
                    return Ok(Expr::IncDec { name, index, delta: -1, prefix: false });
                }
                Ok(Expr::Var(name, index))
            }
            Some(Tok::Op("(")) => {
                self.pos += 1;
                let e = self.comma()?;
                if !self.eat_op(")") {
                    return Err("syntax error: missing `)'".into());
                }
                Ok(e)
            }
            Some(Tok::Op(op)) => Err(format!("syntax error: operand expected (error token is \"{op}\")")),
            None => Err("syntax error: operand expected".into()),
        }
    }
}

pub fn eval(src: &str, vars: &mut dyn ArithVars) -> Result<i64, String> {
    let toks = tokenize(src)?;
    if toks.len() > 256 { return Err("maximum arithmetic expression size exceeded (256 tokens)".into()); }
    if toks.is_empty() {
        return Ok(0);
    }
    let mut p = P { toks, pos: 0 };
    let expr = p.comma()?;
    if let Some(tok) = p.peek() {
        let shown = match tok {
            Tok::Num(n) => n.to_string(),
            Tok::Ident(s) => s.clone(),
            Tok::Op(o) => o.to_string(),
        };
        return Err(format!("syntax error in expression (error token is \"{shown}\")"));
    }
    run(&expr, vars)
}

fn index_of(index: &Option<Box<Expr>>, vars: &mut dyn ArithVars) -> Result<Option<i64>, String> {
    index.as_ref().map(|e| run(e, vars)).transpose()
}

fn run(expr: &Expr, vars: &mut dyn ArithVars) -> Result<i64, String> {
    Ok(match expr {
        Expr::Num(n) => *n,
        Expr::Var(name, index) => {
            let index = index_of(index, vars)?;
            vars.get(name, index)?
        }
        Expr::Unary(op, e) => {
            let v = run(e, vars)?;
            match *op {
                "!" => (v == 0) as i64,
                "~" => !v,
                "-" => v.wrapping_neg(),
                _ => v,
            }
        }
        Expr::Binary("&&", l, r) => (run(l, vars)? != 0 && run(r, vars)? != 0) as i64,
        Expr::Binary("||", l, r) => (run(l, vars)? != 0 || run(r, vars)? != 0) as i64,
        Expr::Binary(op, l, r) => {
            let a = run(l, vars)?;
            let b = run(r, vars)?;
            apply(op, a, b)?
        }
        Expr::Assign(op, name, index, value) => {
            let index = index_of(index, vars)?;
            let rhs = run(value, vars)?;
            let result = if *op == "=" {
                rhs
            } else {
                let current = vars.get(name, index)?;
                apply(&op[..op.len() - 1], current, rhs)?
            };
            vars.set(name, index, result)?;
            result
        }
        Expr::IncDec { name, index, delta, prefix } => {
            let index = index_of(index, vars)?;
            let current = vars.get(name, index)?;
            let next = current.wrapping_add(*delta);
            vars.set(name, index, next)?;
            if *prefix { next } else { current }
        }
        Expr::Ternary(c, yes, no) => {
            if run(c, vars)? != 0 { run(yes, vars)? } else { run(no, vars)? }
        }
        Expr::Comma(l, r) => {
            run(l, vars)?;
            run(r, vars)?
        }
    })
}

fn apply(op: &str, a: i64, b: i64) -> Result<i64, String> {
    Ok(match op {
        "+" => a.wrapping_add(b),
        "-" => a.wrapping_sub(b),
        "*" => a.wrapping_mul(b),
        "/" | "%" if b == 0 => return Err("division by 0".into()),
        "/" => a.wrapping_div(b),
        "%" => a.wrapping_rem(b),
        "**" => {
            if b < 0 {
                return Err("exponent less than 0".into());
            }
            a.wrapping_pow(b.min(u32::MAX as i64) as u32)
        }
        "<<" => a.wrapping_shl(b as u32),
        ">>" => a.wrapping_shr(b as u32),
        "<" => (a < b) as i64,
        ">" => (a > b) as i64,
        "<=" => (a <= b) as i64,
        ">=" => (a >= b) as i64,
        "==" => (a == b) as i64,
        "!=" => (a != b) as i64,
        "&" => a & b,
        "^" => a ^ b,
        "|" => a | b,
        _ => return Err(format!("unknown operator {op}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Env(HashMap<String, i64>);

    impl ArithVars for Env {
        fn get(&mut self, name: &str, _index: Option<i64>) -> Result<i64, String> {
            Ok(*self.0.get(name).unwrap_or(&0))
        }
        fn set(&mut self, name: &str, _index: Option<i64>, value: i64) -> Result<(), String> {
            self.0.insert(name.into(), value);
            Ok(())
        }
    }

    fn e(src: &str) -> i64 {
        eval(src, &mut Env::default()).unwrap()
    }

    #[test]
    fn follows_c_precedence() {
        assert_eq!(e("1 + 2 * 3"), 7);
        assert_eq!(e("(1 + 2) * 3"), 9);
        assert_eq!(e("2 ** 3 ** 2"), 512);
        assert_eq!(e("-2 ** 2"), 4);
        assert_eq!(e("7 / 2 + 7 % 2"), 4);
        assert_eq!(e("1 < 2 && 3 > 4 || 1"), 1);
        assert_eq!(e("5 & 3 | 8 ^ 1"), 9);
        assert_eq!(e("1 ? 10 : 20"), 10);
        assert_eq!(e("0x1f + 010 + 2#11"), 31 + 8 + 3);
        assert_eq!(e(""), 0);
    }

    #[test]
    fn assignments_and_short_circuit() {
        let mut env = Env::default();
        assert_eq!(eval("x = 5, x += 2, x++", &mut env).unwrap(), 7);
        assert_eq!(env.0["x"], 8);
        eval("0 && (y = 1)", &mut env).unwrap();
        assert!(!env.0.contains_key("y"));
        assert_eq!(eval("++x * 2", &mut env).unwrap(), 18);
    }

    #[test]
    fn reports_errors() {
        assert_eq!(eval("1 / 0", &mut Env::default()).unwrap_err(), "division by 0");
        assert!(eval("1 +", &mut Env::default()).is_err());
        assert!(eval("09", &mut Env::default()).is_err());
    }
}
