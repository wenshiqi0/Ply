//! ply-sh: a shell aligned with bash, written from scratch for ply.
//!
//! Important decisions:
//! - Callers cannot tell it from bash at the call level: same syntax, same
//!   expansion rules, same `$?`, same error text (`bash: line N: ...`).
//!   Divergences are bugs unless listed in the crate README.
//! - It owns no storage. Its own file operations —
//!   redirections, globbing, `test -f`, `cd`, each utility's reads and
//!   writes — goes through [`ply_io::Io`] (via `ply-libc`), so the IO
//!   namespace and read-only mounts apply to it. An optional host command
//!   bridge runs native programs with their host filesystem semantics. Plugin-level execution
//!   events are handled by the kernel, not by this library.
//! - Builtins (need shell state) live in `builtins`; utilities (only need
//!   files and streams) implement [`Utility`] and can be added by hosts.

mod builtins;
mod exec;
mod expand;
pub mod syntax;
mod utils;

pub use exec::state::{Fds, Input, Sink};
pub use exec::{ExecOutput, Limits, Shell};
pub use utils::{ExternalCommands, Registry, UtilCtx, Utility};
