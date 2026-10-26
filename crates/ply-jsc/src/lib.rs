//! ply-jsc: JavaScript/TypeScript aligned with node, running in
//! JavaScriptCore embedded in the ply process.
//!
//! Important decisions:
//! - The engine is the system JavaScriptCore (macOS for now), driven
//!   through its C API. It ships no host APIs of its own; everything plugin
//!   code can do comes from ply's prelude. Files/processes go through
//!   `ply-libc` into `ply-io`; the embedded model layer uses Rust HTTP.
//! - TypeScript runs directly: oxc strips types and ply loads modules
//!   itself (see `transpile`), the way bun puts a transpiler in front of
//!   JSC. No external toolchain is needed at runtime.
//! - One realm per plugin, each on its own thread with its own context
//!   group and event loop. A realm runs all fibers of its plugin; they
//!   share its closures and interleave at `await`.
//! - Plugin code sees node's globals and modules plus one global of
//!   ply's modules: import the plugin directory from `ply:plugins`.
//! - Layers above reach the IO layer through this crate only, so the
//!   types they need are re-exported here.

#[cfg(not(target_os = "macos"))]
compile_error!("ply-jsc currently supports macOS only (system JavaScriptCore)");

mod ffi;
mod checkpoints;
mod http;
mod runtime;
mod sh;
mod terminal;
mod markdown;
mod skills;
pub mod transpile;

pub use ply_core::{CleanupStatus, FiberEvent};
pub use runtime::{FailureKind, Caller, HostBridge, JsError, JsRuntime, Limits, ModuleSource, Output, RealmConfig, RealmId};
pub use ply_libc::{DiskStorage, Io, MemoryStorage, MountInfo, MountMode, SqliteStorage, Storage, StorageInfo, StorageSpec};
