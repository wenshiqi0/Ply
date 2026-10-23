//! The one IO layer under every wendy runtime.
//!
//! Important decisions:
//! - Side effects on a machine come from IO, so IO is what wendy controls.
//!   Plugins (the JS sandbox, node-aligned) do not touch storage themselves:
//!   every read and write they perform is a call on [`Io`]. Control lives
//!   here, once, instead of in each runtime.
//! - Starting a real process ([`Io::exec`], e.g. the system bash) is IO too.
//!   It is gated like a write. The process is a black box, so wendy holds its
//!   channels instead: stdio is captured, file writes are confined by the OS
//!   sandbox to read-write disk mounts and observed while it runs, the
//!   network is closed unless allowed. What it changed lands in the trace as
//!   ordinary IO events attributed to it.
//! - Each call is decided on the spot by the [`Gate`] (allow / deny, where a
//!   gate may ask a human before answering), checked against the mount mode,
//!   performed on the mounted [`Storage`](wendy_storage::Storage), and traced
//!   with its verdict and outcome. A denied call is traced too.
//! - Only what is mounted exists. Paths above mount points are synthetic
//!   directories; nothing else of the host is reachable.
//! - Who asked is part of every request ([`Actor`]): runtime, session and the
//!   command or API name, so a trace line says "`cp` in sh session 3 wrote
//!   /out/a", not just "something wrote /out/a".

mod exec;
#[cfg(target_os = "macos")]
mod fsevents;
mod gate;
mod handle;
mod namespace;
mod seatbelt;

pub use exec::{Change, ExecPolicy, ExecResult, ExecSpec};
pub use gate::{AllowAll, Approver, Answer, Decision, Gate, Policy, Rule, RuleAction};
pub use handle::Io;
pub use namespace::{MountInfo, MountMode, Namespace};
pub use wendy_storage::{DirItem, Entry, EntryKind, Storage, StorageInfo, StorageSpec};

use serde::Serialize;

/// Whether an operation only observes or also changes state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OpKind {
    Read,
    Stat,
    List,
    Write,
    Append,
    Mkdir,
    Remove,
    Rename,
    /// Start a real process.
    Exec,
}

impl OpKind {
    pub fn access(self) -> Access {
        match self {
            OpKind::Read | OpKind::Stat | OpKind::List => Access::Read,
            _ => Access::Write,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OpKind::Read => "read",
            OpKind::Stat => "stat",
            OpKind::List => "list",
            OpKind::Write => "write",
            OpKind::Append => "append",
            OpKind::Mkdir => "mkdir",
            OpKind::Remove => "remove",
            OpKind::Rename => "rename",
            OpKind::Exec => "exec",
        }
    }
}

/// Which runtime an IO call came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    /// Plugin code in the JS sandbox.
    Js,
    /// A real process started through [`Io::exec`]; its IO is observed.
    Exec,
    /// wendy itself.
    Host,
}

/// Who is performing an IO call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Actor {
    pub runtime: Runtime,
    /// Session (a plugin realm, the caller of a process) the call belongs to.
    pub session: String,
    /// API (`fs.promises.writeFile`) or program (`bash`) name.
    pub label: String,
    /// The IPO operation (tool call) the IO belongs to, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
}

impl Actor {
    pub fn new(runtime: Runtime, session: impl Into<String>, label: impl Into<String>) -> Self {
        Actor { runtime, session: session.into(), label: label.into(), op: None }
    }

    pub fn host() -> Self {
        Actor::new(Runtime::Host, "host", "host")
    }
}

/// One IO call, as the gate sees it.
#[derive(Debug, Clone, Serialize)]
pub struct IoRequest {
    pub op: OpKind,
    pub access: Access,
    /// Normalized absolute path in the namespace.
    pub path: String,
    /// Rename destination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Program and arguments, for [`OpKind::Exec`] (`path` is its cwd).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    pub actor: Actor,
}
