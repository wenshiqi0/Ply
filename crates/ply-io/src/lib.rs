//! The IO layer under every ply runtime: which storage exists, and how a
//! real process is started on it.
//!
//! Important decisions:
//! - Only what is mounted exists. Paths above mount points are synthetic
//!   directories; nothing else of the host is reachable through [`Io`].
//! - Mounts can be added, removed and swapped at any time. Callers only
//!   see paths, so a mount can move from a host directory to memory to a
//!   database without them noticing.
//! - Read-only or read-write is a property of the mount, like a mount flag.
//! - A real process ([`Io::exec`]) runs in a directory on a disk mount
//!   (storage that *is* a host directory) and sees the host as it is.
//! - Nothing is recorded or decided here. Recording happens at the fiber
//!   level in the kernel; permissions are the plugins' business.

mod exec;
mod handle;
mod namespace;

pub use exec::{ExecResult, ExecSpec};
pub use handle::Io;
pub use namespace::{MountInfo, MountMode};
pub use ply_storage::{
    DirItem, DiskStorage, Entry, EntryKind, MemoryStorage, SqliteStorage, Storage, StorageInfo, StorageSpec,
};
