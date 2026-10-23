//! wendy-jsc: JavaScript/TypeScript aligned with node, running in
//! JavaScriptCore embedded in the wendy process.
//!
//! Important decisions:
//! - The engine is the system JavaScriptCore (macOS for now), driven
//!   through its C API. It ships no host APIs of its own; everything plugin
//!   code can do comes from wendy's prelude, and every IO goes through
//!   `wendy-libc` into `wendy-io`.
//! - TypeScript runs directly: oxc strips types and wendy loads modules
//!   itself (see `transpile`), the way bun puts a transpiler in front of
//!   JSC. No external toolchain is needed at runtime.
//! - One realm per plugin, each on its own thread with its own context
//!   group and event loop. A realm runs all fibers of its plugin; they
//!   share its closures and interleave at `await`.
//! - Plugin code sees node's globals and modules plus one global of
//!   wendy's: `tools`, through which it awaits other plugins.
//! - Layers above reach the IO layer through this crate only, so the
//!   types they need are re-exported here.

#[cfg(not(target_os = "macos"))]
compile_error!("wendy-jsc currently supports macOS only (system JavaScriptCore)");

mod ffi;
mod runtime;
pub mod transpile;

pub use runtime::{Caller, HostBridge, JsError, JsRuntime, Limits, ModuleSource, Output, RealmConfig, RealmId};
pub use wendy_libc::{DiskStorage, Io, MemoryStorage, MountInfo, MountMode, SqliteStorage, Storage, StorageInfo, StorageSpec};
