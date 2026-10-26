//! The shape of a parsed script.
//!
//! Words keep their quoting structure (`Part`s) all the way to expansion,
//! because bash decides splitting, globbing and `"$@"` behavior from it.

use std::sync::Arc;

/// A sequence of commands (`a; b & c`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Script(pub Vec<ListItem>);

#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub and_or: AndOr,
    pub background: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AndOr {
    pub first: Pipeline,
    pub rest: Vec<(Connector, Pipeline)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connector {
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pipeline {
    pub negate: bool,
    pub commands: Vec<Command>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Simple(SimpleCommand),
    Compound { body: Compound, redirects: Vec<Redirect> },
    FunctionDef { name: String, body: Arc<Command> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimpleCommand {
    pub assigns: Vec<Assign>,
    pub words: Vec<Word>,
    pub redirects: Vec<Redirect>,
    pub line: usize,
    /// Arguments of `declare`/`typeset`/`local`/`export`/`readonly`, where
    /// `name=value` and `name=(...)` are real assignments, as in bash. When
    /// set, `words` holds only the command name.
    pub decl: Option<Vec<DeclItem>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeclItem {
    Assign(Assign),
    Word(Word),
}

pub const DECLARATION_BUILTINS: &[&str] = &["declare", "typeset", "local", "export", "readonly"];

#[derive(Debug, Clone, PartialEq)]
pub struct Assign {
    pub name: String,
    /// `name[index]=value`
    pub index: Option<Word>,
    pub value: AssignValue,
    /// `+=`
    pub append: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssignValue {
    Scalar(Word),
    Array(Vec<Word>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Compound {
    Group(Script),
    Subshell(Script),
    If { branches: Vec<(Script, Script)>, otherwise: Option<Script> },
    While { cond: Script, body: Script, until: bool },
    For { var: String, words: Option<Vec<Word>>, body: Script },
    ArithFor { init: Word, cond: Word, step: Word, body: Script },
    Case { word: Word, arms: Vec<CaseArm> },
    Arith(Word),
    Cond(CondExpr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaseArm {
    pub patterns: Vec<Word>,
    pub body: Script,
    pub end: CaseEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseEnd {
    /// `;;`
    Break,
    /// `;&` run the next arm's body too
    FallThrough,
    /// `;;&` keep testing the following patterns
    Continue,
}

/// `[[ ... ]]`
#[derive(Debug, Clone, PartialEq)]
pub enum CondExpr {
    Word(Word),
    Unary(String, Word),
    Binary(Word, String, Word),
    Not(Box<CondExpr>),
    And(Box<CondExpr>, Box<CondExpr>),
    Or(Box<CondExpr>, Box<CondExpr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Redirect {
    /// Explicit fd (`2>`); `None` means the operator's default.
    pub fd: Option<u32>,
    pub kind: RedirKind,
    pub target: Word,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirKind {
    /// `<`
    In,
    /// `>`
    Out,
    /// `>>`
    Append,
    /// `>|`
    Clobber,
    /// `<>`
    ReadWrite,
    /// `>&` / `n>&m`
    DupOut,
    /// `<&`
    DupIn,
    /// `&>`
    OutErr,
    /// `&>>`
    AppendErr,
    /// `<<<`
    HereString,
    /// `<<` / `<<-`; the target word is the (already read) body
    HereDoc,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Word(pub Vec<Part>);

#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    /// Unquoted text: subject to tilde, brace and glob expansion.
    Lit(String),
    /// Quoted literal text (`'..'`, `$'..'`, `\x`, text inside `".."`).
    Quoted(String),
    /// A `".."` region; its expansions are quoted.
    Double(Vec<Part>),
    Param(Box<Param>),
    CmdSub(Arc<Script>),
    Arith(Word),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Variable name, digit(s), or one of `@ * # ? $ ! - 0`.
    pub name: String,
    /// `${name[index]}`
    pub index: Option<Word>,
    /// `${!name}` indirection (or `${!arr[@]}` keys).
    pub indirect: bool,
    pub op: ParamOp,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamOp {
    None,
    /// `${#name}`
    Length,
    /// `${name:-word}` / `${name-word}`
    Default { colon: bool, word: Word },
    /// `${name:=word}`
    Assign { colon: bool, word: Word },
    /// `${name:?word}`
    Error { colon: bool, word: Word },
    /// `${name:+word}`
    Alt { colon: bool, word: Word },
    /// `${name#pat}` / `${name##pat}`
    TrimPrefix { longest: bool, pattern: Word },
    /// `${name%pat}` / `${name%%pat}`
    TrimSuffix { longest: bool, pattern: Word },
    /// `${name/pat/rep}`, `//` all, `/#` anchored start, `/%` anchored end
    Replace { all: bool, anchor: Option<char>, pattern: Word, with: Word },
    /// `${name:offset:length}` (arithmetic text)
    Substring { offset: Word, length: Option<Word> },
    /// `${name^}` `${name^^}` `${name,}` `${name,,}`
    Case { upper: bool, all: bool },
}

impl Word {
    pub fn lit(s: &str) -> Word {
        Word(vec![Part::Lit(s.to_string())])
    }

    /// The word's text if it is a single unquoted literal (keywords, names).
    pub fn as_plain(&self) -> Option<&str> {
        match self.0.as_slice() {
            [Part::Lit(s)] => Some(s),
            _ => None,
        }
    }
}
