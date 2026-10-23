//! wendy-sh: a shell aligned with bash, written from scratch for wendy.
//!
//! Important decisions:
//! - Callers cannot tell it from bash at the call level: same syntax, same
//!   expansion rules, same `$?`, same error text (`bash: line N: ...`).
//!   Divergences are bugs unless listed in the crate README.
//! - It owns no storage and spawns no processes. Every file operation —
//!   redirections, globbing, `test -f`, `cd`, each utility's reads and
//!   writes — goes through [`wendy_io::Io`] (via `wendy-libc`), so the IO
//!   layer gates and traces all of it with the command that caused it.
//! - Builtins (need shell state) live in `builtins`; utilities (only need
//!   files and streams) implement [`Utility`] and can be added by hosts.

mod builtins;
mod exec;
mod expand;
pub mod syntax;
mod utils;

pub use exec::state::{Fds, Input, Sink};
pub use exec::{ExecOutput, Limits, Shell};
pub use utils::{Registry, UtilCtx, Utility};
