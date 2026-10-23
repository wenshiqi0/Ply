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
//!   layer over [`wendy_io::Io`], so every call is still gated and traced
//!   under the caller's actor.

pub mod errno;
pub mod fs;
pub mod glob;
pub mod path;
pub mod time;

pub use errno::{Errno, SysError};
pub use fs::Fs;
