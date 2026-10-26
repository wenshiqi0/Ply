//! ply core: what the kernel runs, described. No IO, no runtime.
//!
//! Important decisions:
//! - A plugin is a module with up to two hooks, both named exports:
//!   `onHandle(input) → output`, run once per call, and `onLoad()`, run
//!   once when the plugin loads. Its name is its directory, its description
//!   is the JSDoc on `onHandle` (or `onLoad`), and the description of its
//!   input and output is `onHandle`'s TypeScript signature, read as a
//!   [`Type`]. Plugins declare nothing else; checking input is the
//!   plugin's own business.
//! - Every execution of `onHandle` or a subscribed event callback is a
//!   [`Fiber`]. Event callbacks carry their event name and emitting parent.
//!   A plugin reaches another
//!   only by awaiting it (`await plugins.bash(...)`): `then` is the call, and
//!   the awaited execution is a child fiber.
//! - What `onLoad` returns is mounted, flat, on the global `ctx` every
//!   plugin sees ([`Mounts`]); `plugins.<name>.loaded` resolves when that
//!   plugin's `onLoad` has returned, so plugins can build on each other
//!   (register into a context, say).
//! - What is recorded is fibers: their input, their structured records,
//!   how they exited, and which fiber awaited which ([`FiberEvent`],
//!   delivered to [`FiberHook`]s).
//! - These types are the only definition. The kernel builds them, hooks
//!   receive them, records are their serialization.

pub mod fiber;
pub mod plugin;
pub mod types;

pub use fiber::{BudgetResource, CleanupStatus, Exit, Fiber, FiberEvent, FiberHook};
pub use plugin::{Mounts, PluginSpec, ToolSchema};
pub use types::{Field, Type};

/// Opaque data a fiber takes or returns.
pub type Payload = serde_json::Value;
