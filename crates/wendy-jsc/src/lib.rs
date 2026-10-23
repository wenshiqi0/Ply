//! wendy-jsc: wendy's second outlet — JavaScript/TypeScript aligned with
//! node, running in JavaScriptCore embedded in the wendy process.
//!
//! Important decisions:
//! - The engine is the system JavaScriptCore (macOS for now), driven
//!   through its C API. It ships no host APIs of its own; everything node
//!   code can do comes from wendy's prelude, and every IO goes through
//!   `wendy-libc` into `wendy-io`.
//! - TypeScript runs directly: oxc strips types and wendy loads modules
//!   itself (see `transpile`), the way bun puts a transpiler in front of
//!   JSC. No external toolchain is needed at runtime.
//! - One realm per plugin, each on its own thread with its own context
//!   group and event loop: no shared globals, no shared stalls.
//! - Besides node's modules, plugins import `"wendy"` (the SDK:
//!   `definePlugin`, `exec`, and calling other tools through the kernel).

#[cfg(not(target_os = "macos"))]
compile_error!("wendy-jsc currently supports macOS only (system JavaScriptCore)");

mod ffi;
mod runtime;
pub mod transpile;

pub use runtime::{Caller, HostBridge, JsError, JsRuntime, Limits, ModuleSource, Output, RealmConfig, RealmId};
