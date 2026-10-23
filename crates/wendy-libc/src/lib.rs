//! wendy's libc: the system layer both runtimes are composed from.
//!
//! Important decisions:
//! - wendy's runtimes (today the node-aligned JS sandbox; wendy's own
//!   bash-aligned sh is parked) sit on this crate the way programs sit on
//!   libc, so they share one definition of what reading a file means, one
//!   error vocabulary and one path model.
//! - Modules are independent so each outlet takes only what it needs:
//!   [`errno`] (error codes and messages), [`path`] (POSIX path algebra),
//!   [`fs`] (cwd-relative file operations), [`glob`] (shell patterns),
//!   [`time`] (UTC calendar arithmetic).
//! - Nothing here touches storage directly. [`fs`] is a thin, cwd-aware
//!   layer over [`Io`].
//! - Layers above use the IO layer through this crate only, so the IO
//!   types they need are re-exported here.

pub mod errno;
pub mod fs;
pub mod glob;
pub mod path;
pub mod time;

pub use errno::{Errno, SysError};
pub use fs::Fs;
pub use wendy_io::{
    DirItem, DiskStorage, Entry, EntryKind, ExecResult, ExecSpec, Io, MemoryStorage, MountInfo, MountMode,
    SqliteStorage, Storage, StorageInfo, StorageSpec,
};
