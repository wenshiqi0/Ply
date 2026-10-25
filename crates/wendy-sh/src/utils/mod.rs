//! Utilities: the commands a system would normally provide as programs
//! (`cat`, `grep`, `sed` ...). Here each one is a Rust implementation that
//! gets its streams and an [`Fs`] over the mounted namespace. File access
//! and read-only behavior are shared with the other Wendy runtimes.
//!
//! Hosts can add their own with [`Registry::insert`].

mod files;
mod find;
mod grep;
mod misc;
pub(crate) mod regex_util;
mod sed;
mod text;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use wendy_libc::Fs;

use crate::exec::state::Fds;

pub struct UtilCtx<'a> {
    pub name: &'a str,
    pub args: &'a [String],
    pub fds: &'a Fds,
    pub fs: &'a Fs,
    pub env: &'a BTreeMap<String, String>,
    pub output_truncated: &'a std::sync::atomic::AtomicBool,
}

impl UtilCtx<'_> {
    pub fn out(&self, s: &str) {
        self.fds.out.write_str(s);
    }

    pub fn out_bytes(&self, b: &[u8]) {
        self.fds.out.write(b);
    }

    /// `name: msg` on stderr.
    pub fn error(&self, msg: &str) {
        self.fds.err.write_str(&format!("{}: {msg}\n", self.name));
    }

    pub fn stdin(&self) -> Vec<u8> {
        self.fds.stdin.read_all()
    }

    /// Contents of an operand, `-` meaning stdin.
    pub async fn read_input(&self, path: &str) -> Result<Vec<u8>, wendy_libc::SysError> {
        if path == "-" { Ok(self.stdin()) } else { self.fs.read(path).await }
    }
}

#[async_trait::async_trait]
pub trait Utility: Send + Sync {
    async fn run(&self, ctx: UtilCtx<'_>) -> i32;
}

#[derive(Clone, Default)]
pub struct Registry {
    map: HashMap<String, Arc<dyn Utility>>,
}

macro_rules! utility {
    ($($name:literal => $f:path),* $(,)?) => {{
        let mut r = Registry::empty();
        $(
            {
                struct U;
                #[async_trait::async_trait]
                impl Utility for U {
                    async fn run(&self, ctx: UtilCtx<'_>) -> i32 {
                        $f(ctx).await
                    }
                }
                r.insert($name, Arc::new(U));
            }
        )*
        r
    }};
}

impl Registry {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Every utility wendy ships.
    pub fn standard() -> Self {
        utility! {
            "cat" => files::cat,
            "ls" => files::ls,
            "mkdir" => files::mkdir,
            "rmdir" => files::rmdir,
            "rm" => files::rm,
            "cp" => files::cp,
            "mv" => files::mv,
            "touch" => files::touch,
            "realpath" => files::realpath,
            "readlink" => files::readlink,
            "basename" => files::basename,
            "dirname" => files::dirname,
            "stat" => files::stat,
            "du" => files::du,
            "find" => find::find,
            "grep" => grep::grep,
            "egrep" => grep::egrep,
            "fgrep" => grep::fgrep,
            "sed" => sed::sed,
            "head" => text::head,
            "tail" => text::tail,
            "wc" => text::wc,
            "sort" => text::sort,
            "uniq" => text::uniq,
            "cut" => text::cut,
            "tr" => text::tr,
            "tee" => text::tee,
            "rev" => text::rev,
            "tac" => text::tac,
            "nl" => text::nl,
            "paste" => text::paste,
            "seq" => text::seq,
            "env" => misc::env,
            "printenv" => misc::printenv,
            "sleep" => misc::sleep,
            "uname" => misc::uname,
            "whoami" => misc::whoami,
            "hostname" => misc::hostname,
        }
    }

    pub fn insert(&mut self, name: &str, utility: Arc<dyn Utility>) {
        self.map.insert(name.to_string(), utility);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Utility>> {
        self.resolve(name)
    }

    /// By name, or by the conventional absolute path (`/usr/bin/cat`).
    pub fn resolve(&self, name: &str) -> Option<Arc<dyn Utility>> {
        if let Some(u) = self.map.get(name) {
            return Some(u.clone());
        }
        let (dir, base) = name.rsplit_once('/')?;
        if matches!(dir, "/bin" | "/usr/bin" | "/usr/local/bin" | "/sbin" | "/usr/sbin") {
            self.map.get(base).cloned()
        } else {
            None
        }
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.map.keys().cloned().collect();
        names.sort();
        names
    }
}

/// Parsed command line: short flags (possibly grouped), their values, and
/// operands. `--long` names map onto short letters via `long`.
pub(crate) struct Args {
    pub flags: Vec<(char, Option<String>)>,
    pub operands: Vec<String>,
}

impl Args {
    pub fn has(&self, c: char) -> bool {
        self.flags.iter().any(|(f, _)| *f == c)
    }

    pub fn value(&self, c: char) -> Option<&str> {
        self.flags.iter().rev().find(|(f, _)| *f == c).and_then(|(_, v)| v.as_deref())
    }

    pub fn values(&self, c: char) -> Vec<&str> {
        self.flags.iter().filter(|(f, _)| *f == c).filter_map(|(_, v)| v.as_deref()).collect()
    }
}

/// `takes_value`: letters that consume an argument. `long`: (name, letter);
/// a long name ending in `=` takes a value. Unknown options are errors.
pub(crate) fn parse_args(args: &[String], known: &str, takes_value: &str, long: &[(&str, char)]) -> Result<Args, String> {
    let mut flags = Vec::new();
    let mut operands = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            operands.extend(args[i + 1..].iter().cloned());
            break;
        }
        if let Some(name) = arg.strip_prefix("--") {
            let (name, inline) = match name.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (name, None),
            };
            let Some((spec, letter)) = long.iter().find(|(l, _)| l.trim_end_matches('=') == name) else {
                return Err(format!("unrecognized option '--{name}'"));
            };
            if spec.ends_with('=') {
                let value = match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        args.get(i).cloned().ok_or_else(|| format!("option '--{name}' requires an argument"))?
                    }
                };
                flags.push((*letter, Some(value)));
            } else {
                flags.push((*letter, inline));
            }
            i += 1;
            continue;
        }
        if arg.len() > 1 && arg.starts_with('-') && !arg[1..].starts_with(|c: char| c.is_ascii_digit() && !known.contains(c)) {
            let chars: Vec<char> = arg[1..].chars().collect();
            let mut j = 0;
            while j < chars.len() {
                let c = chars[j];
                if !known.contains(c) && !takes_value.contains(c) {
                    return Err(format!("invalid option -- '{c}'"));
                }
                if takes_value.contains(c) {
                    let rest: String = chars[j + 1..].iter().collect();
                    let value = if !rest.is_empty() {
                        rest
                    } else {
                        i += 1;
                        args.get(i).cloned().ok_or_else(|| format!("option requires an argument -- '{c}'"))?
                    };
                    flags.push((c, Some(value)));
                    break;
                }
                flags.push((c, None));
                j += 1;
            }
            i += 1;
            continue;
        }
        operands.push(arg.clone());
        i += 1;
    }
    Ok(Args { flags, operands })
}

/// Lines of `data`, each without its trailing newline; whether the input
/// ended with a newline is returned separately.
pub(crate) fn lines(data: &[u8]) -> (Vec<String>, bool) {
    let text = String::from_utf8_lossy(data);
    let trailing = text.ends_with('\n');
    let body = if trailing { &text[..text.len() - 1] } else { &text[..] };
    if body.is_empty() && !trailing {
        return (Vec::new(), false);
    }
    (body.split('\n').map(str::to_string).collect(), trailing)
}
